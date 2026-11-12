//! Supervises the C++/Vulkan engine (`parakeet-server`) and talks to it.

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::path::Path;
use std::process::Child;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::config::EngineConfig;
use crate::formats::{Transcript, Word};

pub struct Engine {
    cfg: EngineConfig,
    client: reqwest::Client,
    child: Mutex<Option<Child>>,
    spawn_lock: tokio::sync::Mutex<()>,
}

impl Engine {
    pub fn new(cfg: EngineConfig) -> Arc<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(600))
            .build()
            .expect("http client");
        Arc::new(Self { cfg, client, child: Mutex::new(None), spawn_lock: tokio::sync::Mutex::new(()) })
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.cfg.port)
    }

    pub async fn healthy(&self) -> bool {
        let url = format!("{}/health", self.base_url());
        match self.client.get(url).timeout(Duration::from_secs(2)).send().await {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }

    /// Makes sure the engine answers, starting it when it does not.
    pub async fn ensure(&self) -> Result<()> {
        if self.healthy().await {
            return Ok(());
        }
        if self.cfg.external {
            return Err(anyhow!("engine at {} is not answering and is marked external", self.base_url()));
        }
        let _guard = self.spawn_lock.lock().await;
        // Another request may have started it while we waited.
        if self.healthy().await {
            return Ok(());
        }

        {
            let mut slot = self.child.lock().unwrap();
            let dead = match slot.as_mut() {
                Some(child) => matches!(child.try_wait(), Ok(Some(_)) | Err(_)),
                None => true,
            };
            if dead {
                *slot = None;
            }
            if slot.is_none() {
                let mut cmd = std::process::Command::new(&self.cfg.exe);
                cmd.arg("--model").arg(&self.cfg.model).arg("--port").arg(self.cfg.port.to_string());
                for arg in &self.cfg.args {
                    cmd.arg(arg);
                }
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                    cmd.creation_flags(CREATE_NO_WINDOW);
                }
                tracing::info!(exe = %self.cfg.exe, port = self.cfg.port, "starting the transcription engine");
                let child = cmd.spawn().with_context(|| format!("starting {}", self.cfg.exe))?;
                *slot = Some(child);
            }
        }

        let deadline = std::time::Instant::now() + Duration::from_secs(self.cfg.startup_timeout_secs);
        while std::time::Instant::now() < deadline {
            if self.healthy().await {
                tracing::info!("engine is ready");
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Err(anyhow!("engine did not answer within {}s", self.cfg.startup_timeout_secs))
    }

    /// Restarts the engine if it stops answering; runs for the life of the server.
    pub async fn supervise(self: Arc<Self>) {
        let mut ticker = tokio::time::interval(Duration::from_secs(20));
        loop {
            ticker.tick().await;
            if !self.healthy().await {
                tracing::warn!("engine is not answering, restarting it");
                if let Err(err) = self.ensure().await {
                    tracing::error!(%err, "engine restart failed");
                }
            }
        }
    }

    /// Sends a 16 kHz mono WAV to the warm engine and returns words with timestamps.
    pub async fn transcribe(&self, wav: &Path, language: Option<&str>) -> Result<Transcript> {
        self.ensure().await?;
        let bytes = tokio::fs::read(wav).await.with_context(|| format!("reading {}", wav.display()))?;
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name("audio.wav")
            .mime_str("audio/wav")?;
        let mut form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("model", "parakeet")
            .text("response_format", "verbose_json")
            .text("timestamp_granularities[]", "word");
        if let Some(lang) = language {
            if !lang.is_empty() && lang != "automatic" {
                form = form.text("language", lang.to_string());
            }
        }

        let url = format!("{}/v1/audio/transcriptions", self.base_url());
        let response = self.client.post(url).multipart(form).send().await.context("calling the engine")?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("engine answered {status}: {}", body.chars().take(300).collect::<String>()));
        }
        let value: Value = serde_json::from_str(&body).context("engine returned invalid JSON")?;

        let mut words: Vec<Word> = value
            .get("words")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|w| {
                        Some(Word {
                            word: w.get("word")?.as_str()?.to_string(),
                            start: w.get("start")?.as_f64()?,
                            end: w.get("end")?.as_f64()?,
                            conf: w.get("conf").and_then(Value::as_f64).map(|c| c as f32),
                            raw_text: None,
                            speaker: None,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let text = value.get("text").and_then(Value::as_str).unwrap_or_default().trim().to_string();
        if words.is_empty() && !text.is_empty() {
            // A model without word timestamps: keep the text, no timings.
            words = text
                .split_whitespace()
                .map(|w| Word { word: w.to_string(), start: 0.0, end: 0.0, conf: None, raw_text: None, speaker: None })
                .collect();
        }

        Ok(Transcript {
            text,
            words,
            language: value.get("language").and_then(Value::as_str).map(str::to_string),
            duration: value.get("duration").and_then(Value::as_f64),
        })
    }

    /// Diarization path: `parakeet-cli scene` reads the whole file and labels speakers.
    /// It loads models per call, so it is only used when a request asks for it.
    pub async fn scene(&self, cli: &str, asr_model: &str, diar_model: &str, wav: &Path) -> Result<Transcript> {
        let mut cmd = tokio::process::Command::new(cli);
        cmd.arg("scene")
            .arg("--model").arg(asr_model)
            .arg("--diar").arg(diar_model)
            .arg("--input").arg(wav)
            .arg("--json");
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let output = cmd.output().await.context("running parakeet-cli scene")?;
        if !output.status.success() {
            return Err(anyhow!("scene failed: {}", String::from_utf8_lossy(&output.stderr).chars().take(300).collect::<String>()));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut words: Vec<Word> = Vec::new();
        let mut speakers: Vec<(f64, f64, String)> = Vec::new();
        let mut duration: f64 = 0.0;

        for line in stdout.lines() {
            let line = line.trim();
            if !line.starts_with('{') {
                continue;
            }
            let Ok(snapshot) = serde_json::from_str::<Value>(line) else { continue };
            if let Some(t) = snapshot.get("t").and_then(Value::as_f64) {
                duration = duration.max(t);
            }
            if let Some(list) = snapshot.get("words").and_then(Value::as_array) {
                for w in list {
                    let Some(text) = w.get("word").or_else(|| w.get("text")).and_then(Value::as_str) else { continue };
                    if text.trim().is_empty() {
                        continue;
                    }
                    let start = w.get("start").and_then(Value::as_f64).unwrap_or(0.0);
                    let end = w.get("end").and_then(Value::as_f64).unwrap_or(start);
                    let speaker = w.get("speaker").map(|s| match s {
                        Value::String(v) => v.clone(),
                        other => other.to_string(),
                    });
                    if words.iter().any(|e| e.start == start && e.end == end && e.word == text) {
                        continue;
                    }
                    words.push(Word {
                        word: text.to_string(),
                        start,
                        end,
                        conf: w.get("conf").and_then(Value::as_f64).map(|c| c as f32),
                        raw_text: None,
                        speaker,
                    });
                }
            }
            if let Some(list) = snapshot.get("speakers").and_then(Value::as_array) {
                for s in list {
                    let Some(id) = s.get("speaker").and_then(Value::as_str) else { continue };
                    let start = s.get("start").and_then(Value::as_f64).unwrap_or(0.0);
                    let end = s.get("end").and_then(Value::as_f64).unwrap_or(start);
                    if !speakers.iter().any(|(a, b, c)| a == &start && b == &end && c == id) {
                        speakers.push((start, end, id.to_string()));
                    }
                }
            }
        }

        // Attach a speaker to each word from the speaker turns it overlaps most.
        for word in words.iter_mut() {
            if word.speaker.is_some() {
                continue;
            }
            let mid = (word.start + word.end) / 2.0;
            if let Some((_, _, id)) = speakers.iter().find(|(s, e, _)| mid >= *s && mid <= *e) {
                word.speaker = Some(id.clone());
            }
        }
        words.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));

        let mut transcript = Transcript { text: String::new(), words, language: None, duration: Some(duration) };
        transcript.rebuild_text();
        Ok(transcript)
    }
}
