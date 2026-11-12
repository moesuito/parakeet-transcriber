//! HTTP surface: OpenAI compatibility, the rich transcription endpoint, jobs and corrections.

use axum::{
    body::Body,
    extract::{Multipart, Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;

use crate::config::Config;
use crate::engine::Engine;
use crate::formats::{openai_json, openai_verbose, Transcript};
use crate::queue::{record_queued, Queue};
use crate::store::{Rule, Store};

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub store: Arc<Store>,
    pub engine: Arc<Engine>,
    pub queue: Arc<Queue>,
}

pub fn router(state: AppState) -> Router {
    let max_body = state.cfg.max_upload_mb * 1024 * 1024;
    Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/v1/audio/transcriptions", post(transcriptions))
        .route("/transcribe", post(transcribe))
        .route("/jobs", get(list_jobs))
        .route("/jobs/{id}", get(get_job).delete(delete_job))
        .route("/corrections", get(list_corrections).post(add_correction))
        .route("/corrections/import", post(import_corrections))
        .route("/corrections/reload", post(reload_corrections))
        .route("/corrections/{id}", delete(delete_correction))
        .layer(axum::extract::DefaultBodyLimit::max(max_body))
        .with_state(state)
}

// ---------------------------------------------------------------- errors

pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into() }
    }
    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
    fn too_many(message: impl Into<String>) -> Self {
        Self::new(StatusCode::TOO_MANY_REQUESTS, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": { "message": self.message } }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(err: anyhow::Error) -> Self {
        let message = format!("{err:#}");
        let status = if message.contains("queue is full") {
            StatusCode::TOO_MANY_REQUESTS
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        Self::new(status, message)
    }
}

// ---------------------------------------------------------------- helpers

struct Parsed {
    file: Option<(String, Vec<u8>)>,
    format: Option<String>,
    language: Option<String>,
    diarize: Option<String>,
    with_words: bool,
}

async fn parse(mut mp: Multipart) -> Result<Parsed, ApiError> {
    let mut out = Parsed { file: None, format: None, language: None, diarize: None, with_words: false };
    while let Some(field) = mp.next_field().await.map_err(|e| ApiError::bad_request(e.to_string()))? {
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "file" => {
                let filename = field.file_name().unwrap_or("audio").to_string();
                let bytes = field.bytes().await.map_err(|e| ApiError::bad_request(e.to_string()))?;
                out.file = Some((filename, bytes.to_vec()));
            }
            "response_format" | "format" => out.format = Some(field.text().await.unwrap_or_default()),
            "language" => out.language = Some(field.text().await.unwrap_or_default()),
            "diarize" | "diarization" | "speakers" => out.diarize = Some(field.text().await.unwrap_or_default()),
            "timestamp_granularities[]" | "timestamp_granularities" => {
                let value = field.text().await.unwrap_or_default();
                if value.contains("word") {
                    out.with_words = true;
                }
            }
            _ => {
                let _ = field.text().await;
            }
        }
    }
    Ok(out)
}

fn save_upload(cfg: &Config, name: &str, bytes: &[u8]) -> anyhow::Result<(std::path::PathBuf, String)> {
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin")
        .to_lowercase();
    let id = uuid::Uuid::new_v4();
    let path = cfg.temp_dir().join(format!("upload-{id}.{ext}"));
    std::fs::write(&path, bytes)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let hash = format!("{:x}", hasher.finalize());
    Ok((path, hash))
}

fn cache_key(hash: &str, language: &Option<String>, diarize: bool) -> String {
    format!("{hash}|{}|{}", language.clone().unwrap_or_else(|| "auto".into()), diarize)
}

async fn transcript_from_cache(store: &Store, key: &str) -> Option<Transcript> {
    let payload = store.cache_get(key).ok().flatten()?;
    let value: Value = serde_json::from_str(&payload).ok()?;
    let transcript: Transcript = serde_json::from_value(json!({
        "text": value.get("text").cloned().unwrap_or(Value::String(String::new())),
        "words": value.get("words").cloned().unwrap_or(Value::Array(vec![])),
        "language": value.get("language").cloned().unwrap_or(Value::Null),
        "duration": value.get("duration").cloned().unwrap_or(Value::Null),
    }))
    .ok()?;
    Some(transcript)
}

/// Applies the saved rules to a transcript that came from the cache.
fn apply_saved(store: &Store, transcript: &mut Transcript) {
    let rules: Vec<crate::corrections::Correction> = match store.rules() {
        Ok(rules) => rules.into_iter().map(crate::corrections::Correction::from).collect(),
        Err(_) => return,
    };
    let hits = crate::corrections::apply(&mut transcript.words, &rules);
    if !hits.is_empty() {
        transcript.rebuild_text();
        let _ = store.add_hits(&hits);
    }
}

/// Runs one transcription: cache first, then the queue.
async fn run(
    state: &AppState,
    parsed: Parsed,
    diarize: bool,
) -> Result<Transcript, ApiError> {
    let (name, bytes) = parsed.file.ok_or_else(|| ApiError::bad_request("a file field is required"))?;
    let (mut path, hash) = save_upload(&state.cfg, &name, &bytes)?;
    let key = cache_key(&hash, &parsed.language, diarize);

    if let Some(mut cached) = transcript_from_cache(&state.store, &key).await {
        let _ = std::fs::remove_file(&path);
        tracing::info!(%key, "cache hit");
        apply_saved(&state.store, &mut cached);
        return Ok(cached);
    }

    let id = uuid::Uuid::new_v4().to_string();

    // Keep the original audio when the archive is enabled; the queue then leaves it alone.
    let mut archive = false;
    let mut audio_path = None;
    if state.cfg.audio_keep > 0 {
        let dir = std::path::Path::new(&state.cfg.audio_dir);
        if std::fs::create_dir_all(dir).is_ok() {
            let target = crate::storage::archive_path(dir, &id, &name);
            if std::fs::rename(&path, &target).is_ok() {
                path = target.clone();
                archive = true;
                audio_path = Some(target.to_string_lossy().to_string());
            }
        }
    }

    record_queued(
        &state.store,
        &id,
        &name,
        &json!({ "language": parsed.language, "diarize": diarize }),
        audio_path.clone(),
    )
    .map_err(ApiError::from)?;

    state
        .queue
        .submit(crate::queue::JobRequest {
            id,
            source: path,
            name,
            language: parsed.language.clone(),
            diarize,
            archive,
            cache_key: key,
        })
        .await
        .map_err(ApiError::from)
}

// ---------------------------------------------------------------- handlers

async fn index() -> Json<Value> {
    Json(json!({
        "service": "parakeet-server-rs",
        "version": env!("CARGO_PKG_VERSION"),
        "default_format": "text",
        "endpoints": [
            "POST /v1/audio/transcriptions  (OpenAI compatible)",
            "POST /transcribe               (rich: text, json, srt, vtt, words)",
            "GET  /v1/models",
            "GET  /health",
            "GET  /jobs",
            "GET|POST /corrections"
        ]
    }))
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    let engine = state.engine.healthy().await;
    Json(json!({
        "status": if engine { "ok" } else { "degraded" },
        "engine": engine,
        "model": state.cfg.model_id,
        "queue": { "waiting": state.queue.depth(), "capacity": state.queue.capacity() },
    }))
}

async fn models(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "object": "list",
        "data": [{
            "id": state.cfg.model_id,
            "object": "model",
            "created": 0,
            "owned_by": "parakeet.cpp"
        }]
    }))
}

async fn transcriptions(State(state): State<AppState>, mp: Multipart) -> Result<Response, ApiError> {
    let parsed = parse(mp).await?;
    let format = parsed
        .format
        .clone()
        .filter(|f| !f.is_empty())
        .unwrap_or_else(|| state.cfg.default_response_format.clone());
    let with_words = parsed.with_words || format == "verbose_json";

    // OpenAI clients ask for diarization through our own field; it stays off unless asked.
    let diarize = matches!(parsed.diarize.as_deref(), Some("on") | Some("true") | Some("1"));
    let transcript = run(&state, parsed, diarize).await?;

    let response = match format.as_str() {
        "text" | "txt" => text_response("text/plain; charset=utf-8", transcript.text.clone()),
        "srt" => text_response("application/x-subrip", transcript.srt()),
        "vtt" => text_response("text/vtt", transcript.vtt()),
        "verbose_json" => Json(openai_verbose(&transcript, with_words)).into_response(),
        _ => Json(openai_json(&transcript.text)).into_response(),
    };
    Ok(response)
}

#[derive(Deserialize)]
struct TranscribeQuery {
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    diarize: Option<String>,
}

async fn transcribe(
    State(state): State<AppState>,
    Query(query): Query<TranscribeQuery>,
    mp: Multipart,
) -> Result<Response, ApiError> {
    let parsed = parse(mp).await?;
    let format = query
        .format
        .clone()
        .or_else(|| parsed.format.clone())
        .unwrap_or_else(|| "all".into());
    // Diarization is opt-in: only `diarize=on` runs the speaker pass.
    let diarize = matches!(
        query.diarize.as_deref().or(parsed.diarize.as_deref()),
        Some("on") | Some("true") | Some("1")
    );
    let transcript = run(&state, parsed, diarize).await?;

    Ok(match format.as_str() {
        "text" | "txt" => text_response("text/plain; charset=utf-8", transcript.text.clone()),
        "srt" => text_response("application/x-subrip", transcript.srt()),
        "vtt" => text_response("text/vtt", transcript.vtt()),
        "json" => Json(transcript.payload()).into_response(),
        _ => Json(transcript.payload()).into_response(),
    })
}

async fn list_jobs(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let jobs = state.store.job_list(50).map_err(ApiError::from)?;
    Ok(Json(json!({ "jobs": jobs.iter().map(job_json).collect::<Vec<_>>() })))
}

async fn get_job(State(state): State<AppState>, Path(id): Path<String>) -> Result<Response, ApiError> {
    let job = state
        .store
        .job_get(&id)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such job"))?;
    Ok(Json(job_json(&job)).into_response())
}

async fn delete_job(State(state): State<AppState>, Path(id): Path<String>) -> Result<Response, ApiError> {
    let removed = state.store.job_delete(&id).map_err(ApiError::from)?;
    if removed {
        Ok(StatusCode::NO_CONTENT.into_response())
    } else {
        Err(ApiError::new(StatusCode::NOT_FOUND, "no such job"))
    }
}

fn job_json(job: &crate::store::JobRecord) -> Value {
    json!({
        "id": job.id,
        "status": job.status,
        "created_at": job.created_at,
        "started_at": job.started_at,
        "finished_at": job.finished_at,
        "source": job.source,
        "duration_ms": job.duration_ms,
        "error": job.error,
        "audio": job.audio_path,
        "text": if job.status == "done" { job.text.clone() } else { None },
    })
}

async fn list_corrections(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let rules = state.store.rules().map_err(ApiError::from)?;
    Ok(Json(json!({ "corrections": rules.iter().map(rule_json).collect::<Vec<_>>() })))
}

fn rule_json(rule: &Rule) -> Value {
    json!({
        "id": rule.id,
        "from": rule.from,
        "to": rule.to,
        "at": rule.at,
        "case_sensitive": rule.case_sensitive,
        "created_at": rule.created_at,
        "hits": rule.hits,
        "source": rule.source,
    })
}

#[derive(Deserialize)]
struct RuleInput {
    from: String,
    to: String,
    #[serde(default)]
    at: Option<(f64, f64)>,
    #[serde(default)]
    case_sensitive: bool,
}

async fn add_correction(State(state): State<AppState>, Json(input): Json<RuleInput>) -> Result<Json<Value>, ApiError> {
    if input.from.trim().is_empty() {
        return Err(ApiError::bad_request("`from` cannot be empty"));
    }
    let rule = Rule {
        id: uuid::Uuid::new_v4().to_string(),
        from: input.from,
        to: input.to,
        at: input.at,
        case_sensitive: input.case_sensitive,
        created_at: crate::formats::now(),
        hits: 0,
        source: "user".into(),
    };
    state.store.add_rule(&rule).map_err(ApiError::from)?;
    Ok(Json(rule_json(&rule)))
}

async fn delete_correction(State(state): State<AppState>, Path(id): Path<String>) -> Result<Response, ApiError> {
    let removed = state.store.delete_rule(&id).map_err(ApiError::from)?;
    if removed {
        Ok(StatusCode::NO_CONTENT.into_response())
    } else {
        Err(ApiError::new(StatusCode::NOT_FOUND, "no such correction"))
    }
}

#[derive(Deserialize)]
struct FixesFile {
    #[serde(default)]
    replacements: Vec<RuleInput>,
}

/// Accepts the skill's `fixes.json`, so existing correction files keep working.
async fn import_corrections(State(state): State<AppState>, Json(body): Json<FixesFile>) -> Result<Json<Value>, ApiError> {
    let mut added = Vec::new();
    for item in body.replacements {
        if item.from.trim().is_empty() {
            continue;
        }
        let rule = Rule {
            id: uuid::Uuid::new_v4().to_string(),
            from: item.from,
            to: item.to,
            at: item.at,
            case_sensitive: item.case_sensitive,
            created_at: crate::formats::now(),
            hits: 0,
            source: "user".into(),
        };
        state.store.add_rule(&rule).map_err(ApiError::from)?;
        added.push(rule_json(&rule));
    }
    Ok(Json(json!({ "added": added })))
}

/// Re-reads the community lexicon file (after pulling a new `all.json`, for example).
async fn reload_corrections(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    if state.cfg.corrections_file.is_empty() {
        return Err(ApiError::bad_request("no corrections_file is configured"));
    }
    let imported =
        crate::corrections::import_community(&state.store, std::path::Path::new(&state.cfg.corrections_file))
            .map_err(ApiError::from)?;
    let total = state.store.rules().map_err(ApiError::from)?.len();
    Ok(Json(json!({ "imported": imported, "total": total })))
}

fn text_response(content_type: &'static str, body: String) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}
