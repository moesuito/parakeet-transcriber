//! Word-level results and the output formats: plain text (default), JSON, SRT, VTT.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Word {
    #[serde(alias = "text")]
    pub word: String,
    pub start: f64,
    pub end: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conf: Option<f32>,
    /// The word before any correction, kept for review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Transcript {
    pub text: String,
    #[serde(default)]
    pub words: Vec<Word>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub duration: Option<f64>,
}

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl Transcript {
    /// Rebuild the plain text from the (possibly corrected) words.
    pub fn rebuild_text(&mut self) {
        self.text = self.words.iter().map(|w| w.word.as_str()).collect::<Vec<_>>().join(" ");
    }

    pub fn srt(&self) -> String {
        let mut out = String::new();
        for (i, (start, end, text)) in cues(&self.words).into_iter().enumerate() {
            out.push_str(&format!("{}\n{} --> {}\n{}\n\n", i + 1, stamp(start, ','), stamp(end, ','), text));
        }
        out
    }

    pub fn vtt(&self) -> String {
        let mut out = String::from("WEBVTT\n\n");
        for (start, end, text) in cues(&self.words) {
            out.push_str(&format!("{} --> {}\n{}\n\n", stamp(start, '.'), stamp(end, '.'), text));
        }
        out
    }

    /// The full JSON body kept in the cache and returned by `/transcribe`.
    pub fn payload(&self) -> Value {
        json!({
            "text": self.text,
            "language": self.language,
            "duration": self.duration,
            "words": self.words,
            "srt": self.srt(),
            "vtt": self.vtt(),
        })
    }
}

/// `HH:MM:SS,mmm` (SRT) or `HH:MM:SS.mmm` (VTT).
fn stamp(seconds: f64, sep: char) -> String {
    let total_ms = (seconds.max(0.0) * 1000.0).round() as u64;
    let ms = total_ms % 1000;
    let s = (total_ms / 1000) % 60;
    let m = (total_ms / 60_000) % 60;
    let h = total_ms / 3_600_000;
    format!("{h:02}:{m:02}:{s:02}{sep}{ms:03}")
}

/// Group words into subtitle cues by silence, length and duration.
fn cues(words: &[Word]) -> Vec<(f64, f64, String)> {
    let mut out = Vec::new();
    let mut current: Vec<&Word> = Vec::new();
    let mut start = 0.0;

    for word in words {
        if current.is_empty() {
            start = word.start;
        } else {
            let last = *current.last().unwrap();
            let gap = word.start - last.end;
            let span = word.end - start;
            let len: usize = current.iter().map(|w| w.word.chars().count() + 1).sum::<usize>()
                + word.word.chars().count();
            if gap > 0.8 || span > 6.0 || len > 84 {
                out.push((start, last.end, join(&current)));
                current.clear();
                start = word.start;
            }
        }
        current.push(word);
    }
    if let Some(last) = current.last() {
        out.push((start, last.end, join(&current)));
    }
    out
}

fn join(words: &[&Word]) -> String {
    let text = words.iter().map(|w| w.word.as_str()).collect::<Vec<_>>().join(" ");
    // Wrap into at most two balanced lines, always on a word boundary.
    if text.chars().count() <= 42 {
        return text;
    }
    let total = text.chars().count();
    let middle = (total / 2) as isize;
    let mut best: Option<(isize, usize)> = None;
    for (byte, ch) in text.char_indices() {
        if ch != ' ' {
            continue;
        }
        let position = text[..byte].chars().count() as isize;
        let distance = (position - middle).abs();
        if best.map_or(true, |(best_distance, _)| distance < best_distance) {
            best = Some((distance, byte));
        }
    }
    let cut = best.map(|(_, byte)| byte).unwrap_or(text.len());
    format!("{}\n{}", text[..cut].trim_end(), text[cut..].trim_start())
}

/// Plain OpenAI `json` body: only `text`, as clients expect.
pub fn openai_json(text: &str) -> Value {
    json!({ "text": text })
}

pub fn openai_verbose(tr: &Transcript, with_words: bool) -> Value {
    let segments: Vec<Value> = cues(&tr.words)
        .into_iter()
        .enumerate()
        .map(|(i, (start, end, text))| {
            json!({ "id": i, "start": start, "end": end, "text": text.replace('\n', " ") })
        })
        .collect();
    let mut body = json!({
        "task": "transcribe",
        "language": tr.language.clone().unwrap_or_else(|| "en".into()),
        "duration": tr.duration.unwrap_or(0.0),
        "text": tr.text,
        "segments": segments,
    });
    if with_words {
        let words: Vec<Value> = tr
            .words
            .iter()
            .map(|w| json!({ "word": w.word, "start": w.start, "end": w.end, "conf": w.conf }))
            .collect();
        body["words"] = Value::Array(words);
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, start: f64, end: f64) -> Word {
        Word { word: text.into(), start, end, conf: None, raw_text: None, speaker: None }
    }

    #[test]
    fn builds_srt_with_gaps() {
        let words = vec![w("Olá,", 0.0, 0.4), w("mundo.", 0.4, 0.9), w("Depois.", 3.0, 3.4)];
        let tr = Transcript { text: String::new(), words, language: None, duration: None };
        let srt = tr.srt();
        assert!(srt.starts_with("1\n00:00:00,000 --> 00:00:00,900\nOlá, mundo."));
        assert!(srt.contains("2\n00:00:03,000 --> 00:00:03,400\nDepois."));
    }

    #[test]
    fn text_joins_words() {
        let mut tr = Transcript {
            text: String::new(),
            words: vec![w("O", 0.0, 0.1), w("objetivo", 0.1, 0.4), w("é", 0.4, 0.5)],
            language: None,
            duration: None,
        };
        tr.rebuild_text();
        assert_eq!(tr.text, "O objetivo é");
    }

    #[test]
    fn wraps_only_on_word_boundaries() {
        let tokens: Vec<&str> = "O objetivo principal da ciência é entender como o mundo funciona através do método científico".split(' ').collect();
        let words: Vec<Word> = tokens.iter().enumerate().map(|(i, t)| w(t, i as f64, i as f64 + 0.5)).collect();
        let tr = Transcript { text: String::new(), words, language: None, duration: None };
        for (_, _, text) in cues(&tr.words) {
            for line in text.lines() {
                for token in line.split(' ') {
                    assert!(tokens.contains(&token), "cue split a word: {token:?} in {text:?}");
                }
            }
        }
    }
}
