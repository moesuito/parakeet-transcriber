//! Runtime configuration, read from JSON with sensible defaults for this machine.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DEFAULT_CONFIG_PATH: &str = r"C:\AI\stt\config\server.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Address the HTTP server binds to. Local only by default.
    #[serde(default = "default_bind")]
    pub bind: String,
    /// Id reported by `GET /v1/models` and accepted in requests.
    #[serde(default = "default_model_id")]
    pub model_id: String,
    #[serde(default)]
    pub engine: EngineConfig,
    #[serde(default = "default_ffmpeg")]
    pub ffmpeg: String,
    #[serde(default = "default_database")]
    pub database: String,
    #[serde(default)]
    pub temp_dir: Option<String>,
    #[serde(default)]
    pub queue: QueueConfig,
    #[serde(default = "default_max_upload_mb")]
    pub max_upload_mb: usize,
    /// `text`, `json`, `srt` or `vtt`. Used when a request names no format.
    #[serde(default = "default_response_format")]
    pub default_response_format: String,
    /// Path of the skill runtime used for diarization (`parakeet-cli`).
    #[serde(default = "default_cli")]
    pub cli: String,
    #[serde(default = "default_diar_model")]
    pub diar_model: String,
    /// Keep finished jobs in the database for this many days (0 = forever).
    #[serde(default = "default_retention_days")]
    pub retention_days: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    /// The C++/Vulkan engine that keeps the model warm.
    #[serde(default = "default_engine_exe")]
    pub exe: String,
    #[serde(default = "default_engine_model")]
    pub model: String,
    #[serde(default = "default_engine_port")]
    pub port: u16,
    /// Extra arguments appended to the engine command line.
    #[serde(default = "default_engine_args")]
    pub args: Vec<String>,
    /// How long to wait for the engine to answer `/health` after a start.
    #[serde(default = "default_startup_timeout")]
    pub startup_timeout_secs: u64,
    /// Do not start the engine ourselves (it is managed elsewhere).
    #[serde(default)]
    pub external: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueConfig {
    /// Requests waiting for the GPU before we answer 429.
    #[serde(default = "default_queue_capacity")]
    pub capacity: usize,
    /// A single transcription may run at most this long.
    #[serde(default = "default_job_timeout")]
    pub job_timeout_secs: u64,
}

fn default_bind() -> String {
    "127.0.0.1:8800".into()
}
fn default_model_id() -> String {
    "parakeet-ultra".into()
}
fn default_ffmpeg() -> String {
    "ffmpeg".into()
}
fn default_database() -> String {
    r"C:\AI\stt\data\server.db".into()
}
fn default_max_upload_mb() -> usize {
    512
}
fn default_response_format() -> String {
    "text".into()
}
fn default_cli() -> String {
    r"C:\AI\STT\bin\master\parakeet-cli.exe".into()
}
fn default_diar_model() -> String {
    r"C:\AI\STT\models\nemotron-3-diarization-f16.gguf".into()
}
fn default_retention_days() -> i64 {
    30
}
fn default_engine_exe() -> String {
    r"C:\AI\STT\bin\master\parakeet-server.exe".into()
}
fn default_engine_model() -> String {
    r"C:\Users\Alano\.parakeet-transcriber\models\parakeet-ultra.f16.gguf".into()
}
fn default_engine_port() -> u16 {
    8799
}
fn default_engine_args() -> Vec<String> {
    vec!["--keep-loaded".into(), "--idle-timeout".into(), "0".into()]
}
fn default_startup_timeout() -> u64 {
    180
}
fn default_queue_capacity() -> usize {
    64
}
fn default_job_timeout() -> u64 {
    900
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            exe: default_engine_exe(),
            model: default_engine_model(),
            port: default_engine_port(),
            args: default_engine_args(),
            startup_timeout_secs: default_startup_timeout(),
            external: false,
        }
    }
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            capacity: default_queue_capacity(),
            job_timeout_secs: default_job_timeout(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            model_id: default_model_id(),
            engine: EngineConfig::default(),
            ffmpeg: default_ffmpeg(),
            database: default_database(),
            temp_dir: None,
            queue: QueueConfig::default(),
            max_upload_mb: default_max_upload_mb(),
            default_response_format: default_response_format(),
            cli: default_cli(),
            diar_model: default_diar_model(),
            retention_days: default_retention_days(),
        }
    }
}

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let path = path.map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH));
        if !path.exists() {
            // First run: write the defaults so the file is there to edit.
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let cfg = Config::default();
            if let Ok(text) = serde_json::to_string_pretty(&cfg) {
                let _ = std::fs::write(&path, text);
            }
            tracing::info!(path = %path.display(), "wrote default configuration");
            return Ok(cfg);
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let mut cfg: Config = serde_json::from_str(&text)
            .with_context(|| format!("parsing {}", path.display()))?;
        // Environment overrides keep service installs simple.
        if let Ok(v) = std::env::var("PARAKEET_BIND") {
            cfg.bind = v;
        }
        if let Ok(v) = std::env::var("PARAKEET_MODEL_ID") {
            cfg.model_id = v;
        }
        Ok(cfg)
    }

    pub fn engine_base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.engine.port)
    }

    pub fn temp_dir(&self) -> PathBuf {
        self.temp_dir
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
    }
}

pub fn init_logging() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_env("PARAKEET_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt().with_env_filter(filter).with_target(false).try_init();
}
