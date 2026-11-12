//! One GPU, one queue: requests wait their turn instead of fighting the model.

use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

use crate::config::Config;
use crate::corrections::Correction;
use crate::engine::Engine;
use crate::formats::{now, Transcript};
use crate::store::{JobRecord, Store};

pub struct JobRequest {
    pub id: String,
    pub source: PathBuf,
    pub name: String,
    pub language: Option<String>,
    pub diarize: bool,
    /// Keep the file on disk after the job (the audio archive owns it).
    pub archive: bool,
    /// Cache slot for the raw result, so corrections can change later.
    pub cache_key: String,
}

struct Job {
    request: JobRequest,
    reply: oneshot::Sender<Result<Transcript>>,
}

pub struct Queue {
    tx: mpsc::Sender<Job>,
    depth: Arc<AtomicUsize>,
    capacity: usize,
}

impl Queue {
    pub fn start(cfg: Arc<Config>, store: Arc<Store>, engine: Arc<Engine>) -> Arc<Self> {
        let (tx, rx) = mpsc::channel(cfg.queue.capacity);
        let depth = Arc::new(AtomicUsize::new(0));
        let worker = Worker { cfg: cfg.clone(), store, engine, depth: depth.clone() };
        tokio::spawn(worker.run(rx));
        Arc::new(Self { tx, depth, capacity: cfg.queue.capacity })
    }

    pub fn depth(&self) -> usize {
        self.depth.load(Ordering::Relaxed)
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Queues a job and waits for its transcript.
    pub async fn submit(&self, request: JobRequest) -> Result<Transcript> {
        let (tx, rx) = oneshot::channel();
        let id = request.id.clone();

        self.depth.fetch_add(1, Ordering::Relaxed);
        if self.tx.try_send(Job { request, reply: tx }).is_err() {
            self.depth.fetch_sub(1, Ordering::Relaxed);
            return Err(anyhow!("queue is full"));
        }

        let started = Instant::now();
        let waited = rx.await.map_err(|_| anyhow!("job {id} was dropped"))?;
        tracing::info!(job = %id, elapsed_ms = started.elapsed().as_millis(), "job finished");
        waited
    }
}

struct Worker {
    cfg: Arc<Config>,
    store: Arc<Store>,
    engine: Arc<Engine>,
    depth: Arc<AtomicUsize>,
}

impl Worker {
    async fn run(self, mut rx: mpsc::Receiver<Job>) {
        while let Some(job) = rx.recv().await {
            self.depth.fetch_sub(1, Ordering::Relaxed);
            let id = job.request.id.clone();
            let result = self.handle(&job.request).await;
            if let Err(err) = &result {
                let message = format!("{err:#}");
                tracing::error!(job = %id, %message, "job failed");
                let _ = self.store.job_failed(&id, &message);
            }
            let _ = job.reply.send(result);
            if !job.request.archive {
                let _ = tokio::fs::remove_file(&job.request.source).await;
            }
            if self.cfg.audio_keep > 0 {
                let dir = std::path::PathBuf::from(&self.cfg.audio_dir);
                let removed = crate::storage::trim(&dir, self.cfg.audio_keep);
                if removed > 0 {
                    tracing::info!(removed, keep = self.cfg.audio_keep, "trimmed the audio archive");
                }
            }
        }
    }

    async fn handle(&self, job: &JobRequest) -> Result<Transcript> {
        let _ = self.store.job_started(&job.id);
        let started = Instant::now();

        let work = async {
            let wav = self.to_wav(&job.source).await?;
            let mut transcript = if job.diarize {
                let scene = self
                    .engine
                    .scene(&self.cfg.cli, &self.cfg.engine.model, &self.cfg.diar_model, &wav)
                    .await?;
                if scene.words.is_empty() {
                    tracing::warn!(job = %job.id, "diarization returned no words; using the warm engine");
                    self.engine.transcribe(&wav, job.language.as_deref()).await?
                } else {
                    scene
                }
            } else {
                self.engine.transcribe(&wav, job.language.as_deref()).await?
            };
            let _ = tokio::fs::remove_file(&wav).await;

            // The cache holds the raw result: rules can change later without a re-run.
            if let Ok(payload) = serde_json::to_string(&transcript.payload()) {
                let _ = self.store.cache_put(&job.cache_key, &payload);
            }

            // Corrections are part of every result, whichever format is asked for.
            let rules: Vec<Correction> = self.store.rules()?.into_iter().map(Correction::from).collect();
            let hits = crate::corrections::apply(&mut transcript.words, &rules);
            if !hits.is_empty() {
                transcript.rebuild_text();
                let _ = self.store.add_hits(&hits);
                tracing::info!(job = %job.id, rules = hits.len(), "applied corrections");
            }
            Ok::<Transcript, anyhow::Error>(transcript)
        };

        let transcript = tokio::time::timeout(Duration::from_secs(self.cfg.queue.job_timeout_secs), work)
            .await
            .map_err(|_| anyhow!("job timed out after {}s", self.cfg.queue.job_timeout_secs))??;

        let payload = serde_json::to_string(&transcript.payload())?;
        self.store.job_finished(&job.id, &transcript.text, &payload, started.elapsed().as_millis() as i64)?;
        Ok(transcript)
    }

    /// Any input becomes 16 kHz mono WAV, which is what the engine accepts.
    async fn to_wav(&self, source: &std::path::Path) -> Result<PathBuf> {
        let target = self.cfg.temp_dir().join(format!("parakeet-{}.wav", uuid::Uuid::new_v4()));
        let mut cmd = tokio::process::Command::new(&self.cfg.ffmpeg);
        cmd.arg("-y")
            .arg("-loglevel").arg("error")
            .arg("-i").arg(source)
            .arg("-vn")
            .arg("-ac").arg("1")
            .arg("-ar").arg("16000")
            .arg("-f").arg("wav")
            .arg(&target);
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let output = cmd.output().await.context("running ffmpeg")?;
        if !output.status.success() {
            return Err(anyhow!(
                "could not decode the audio: {}",
                String::from_utf8_lossy(&output.stderr).chars().take(300).collect::<String>()
            ));
        }
        Ok(target)
    }
}

/// Registers a queued job so `GET /jobs` lists it even before it runs.
pub fn record_queued(
    store: &Store,
    id: &str,
    source: &str,
    params: &serde_json::Value,
    audio_path: Option<String>,
) -> Result<()> {
    store.job_create(&JobRecord {
        id: id.to_string(),
        status: "queued".into(),
        created_at: now(),
        source: Some(source.to_string()),
        params: Some(params.to_string()),
        audio_path,
        ..Default::default()
    })
}
