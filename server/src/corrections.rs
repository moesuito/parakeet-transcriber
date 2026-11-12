//! Word corrections applied to every transcript before it is formatted.
//!
//! Rules replace whole words or short phrases (case-insensitive by default), keep the
//! original text in `raw_text`, and follow the same conventions as the community
//! `parakeet-corrections` lexicons: word-boundary matching, multi-word entries, and the
//! correction inherits the input's case pattern.

use crate::formats::Word;
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct Correction {
    pub id: String,
    pub from: String,
    pub to: String,
    /// Only apply inside this time range, in seconds.
    pub at: Option<(f64, f64)>,
    pub case_sensitive: bool,
}

impl From<crate::store::Rule> for Correction {
    fn from(r: crate::store::Rule) -> Self {
        Self { id: r.id, from: r.from, to: r.to, at: r.at, case_sensitive: r.case_sensitive }
    }
}

/// Applies every rule to the words. Returns how many times each rule fired.
pub fn apply(words: &mut Vec<Word>, rules: &[Correction]) -> Vec<(String, i64)> {
    let mut hits: Vec<(String, i64)> = Vec::new();
    if rules.is_empty() {
        return hits;
    }

    // Phrases first, longest first, so "excala draw" wins over "excala".
    let mut phrases: Vec<&Correction> = rules
        .iter()
        .filter(|rule| rule.from.split_whitespace().count() > 1)
        .collect();
    phrases.sort_by_key(|rule| std::cmp::Reverse(rule.from.split_whitespace().count()));

    for rule in phrases {
        let parts: Vec<String> = rule.from.split_whitespace().map(|p| p.to_lowercase()).collect();
        let span = parts.len();
        let mut index = 0;
        while index + span <= words.len() {
            let replacement = {
                let window = &words[index..index + span];
                let inside = rule
                    .at
                    .map(|(start, end)| {
                        let midpoint = (window[0].start + window[span - 1].end) / 2.0;
                        midpoint >= start && midpoint <= end
                    })
                    .unwrap_or(true);
                if inside && span_matches(window, &parts, rule.case_sensitive) {
                    let (prefix, _, _) = split_affixes(&window[0].word);
                    let (_, _, suffix) = split_affixes(&window[span - 1].word);
                    let joined = window
                        .iter()
                        .map(|w| w.word.as_str())
                        .collect::<Vec<_>>()
                        .join(" ");
                    let text = match_case(&window[0].word, &rule.to);
                    Some((prefix, text, suffix, window[span - 1].end, joined))
                } else {
                    None
                }
            };
            match replacement {
                Some((prefix, text, suffix, last_end, joined)) => {
                    words[index].raw_text = Some(joined);
                    words[index].word = format!("{prefix}{text}{suffix}");
                    words[index].end = last_end;
                    for word in &mut words[index + 1..index + span] {
                        word.word.clear();
                    }
                    bump(&mut hits, &rule.id);
                    index += span;
                }
                None => index += 1,
            }
        }
    }
    words.retain(|word| !word.word.is_empty());

    // Single words over what is left.
    let single: Vec<&Correction> = rules
        .iter()
        .filter(|rule| rule.from.split_whitespace().count() == 1)
        .collect();
    for word in words.iter_mut() {
        if word.raw_text.is_some() {
            continue;
        }
        let (prefix, core, suffix) = split_affixes(&word.word);
        if core.is_empty() {
            continue;
        }
        let midpoint = (word.start + word.end) / 2.0;
        for rule in &single {
            if let Some((start, end)) = rule.at {
                if midpoint < start || midpoint > end {
                    continue;
                }
            }
            let matched = if rule.case_sensitive {
                core == rule.from
            } else {
                core.to_lowercase() == rule.from.to_lowercase()
            };
            if !matched {
                continue;
            }
            word.raw_text = Some(word.word.clone());
            word.word = format!("{prefix}{}{suffix}", match_case(&core, &rule.to));
            bump(&mut hits, &rule.id);
            break;
        }
    }
    hits
}

fn bump(hits: &mut Vec<(String, i64)>, id: &str) {
    match hits.iter_mut().find(|(entry, _)| entry == id) {
        Some(entry) => entry.1 += 1,
        None => hits.push((id.to_string(), 1)),
    }
}

fn span_matches(window: &[Word], parts: &[String], case_sensitive: bool) -> bool {
    window.iter().zip(parts).all(|(word, part)| {
        let (_, core, _) = split_affixes(&word.word);
        if core.is_empty() {
            return false;
        }
        if case_sensitive {
            core == *part
        } else {
            core.to_lowercase() == *part
        }
    })
}

/// The community convention: ALL CAPS in -> ALL CAPS out, Leading cap -> Capitalised,
/// otherwise the correction is emitted exactly as written.
fn match_case(input: &str, correction: &str) -> String {
    let letters: Vec<char> = input.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.len() > 1 && letters.iter().all(|c| c.is_uppercase()) {
        return correction.to_uppercase();
    }
    if letters.first().map(|c| c.is_uppercase()).unwrap_or(false) && correction.chars().next().map(|c| c.is_lowercase()).unwrap_or(false) {
        let mut chars = correction.chars();
        if let Some(first) = chars.next() {
            return first.to_uppercase().collect::<String>() + chars.as_str();
        }
    }
    correction.to_string()
}

/// Splits a token into leading punctuation, the core word, and trailing punctuation.
fn split_affixes(token: &str) -> (String, String, String) {
    let chars: Vec<char> = token.chars().collect();
    let is_core = |c: char| c.is_alphanumeric() || c == '\'' || c == '-';
    let start = chars.iter().position(|c| is_core(*c)).unwrap_or(chars.len());
    let end = chars.iter().rposition(|c| is_core(*c)).map(|i| i + 1).unwrap_or(start);
    if start >= end {
        return (token.to_string(), String::new(), String::new());
    }
    let prefix: String = chars[..start].iter().collect();
    let core: String = chars[start..end].iter().collect();
    let suffix: String = chars[end..].iter().collect();
    (prefix, core, suffix)
}

// ---------------------------------------------------------------- community lexicons

#[derive(serde::Deserialize)]
struct CommunityEntry {
    mistranslation: String,
    correction: String,
}

/// Imports a `parakeet-corrections` style file (`data/all.json`) as community rules.
///
/// Community rules are replaced as a block on every import; rules the user added by hand
/// are never touched. Returns how many rules were imported (0 when the file is unchanged).
pub fn import_community(store: &crate::store::Store, path: &Path) -> Result<usize> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest = format!("{:x}", hasher.finalize());
    if store.meta_get("community_corrections_sha")?.as_deref() == Some(digest.as_str()) {
        return Ok(0);
    }

    let entries: Vec<CommunityEntry> =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))?;
    let rules: Vec<crate::store::Rule> = entries
        .into_iter()
        .filter(|entry| !entry.mistranslation.trim().is_empty())
        .map(|entry| crate::store::Rule {
            id: uuid::Uuid::new_v4().to_string(),
            from: entry.mistranslation,
            to: entry.correction,
            at: None,
            case_sensitive: false,
            created_at: crate::formats::now(),
            hits: 0,
            source: "community".into(),
        })
        .collect();

    let imported = store.replace_community(&rules)?;
    store.meta_set("community_corrections_sha", &digest)?;
    Ok(imported)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, start: f64, end: f64) -> Word {
        Word { word: text.into(), start, end, conf: None, raw_text: None, speaker: None }
    }

    fn rule(from: &str, to: &str) -> Correction {
        Correction {
            id: format!("r-{from}"),
            from: from.into(),
            to: to.into(),
            at: None,
            case_sensitive: false,
        }
    }

    #[test]
    fn replaces_whole_word_and_keeps_punctuation() {
        let mut words = vec![word("reunion,", 0.0, 1.0), word("reunions", 1.0, 2.0)];
        let hits = apply(&mut words, &[rule("reunion", "reunião")]);
        assert_eq!(words[0].word, "reunião,");
        assert_eq!(words[0].raw_text.as_deref(), Some("reunion,"));
        assert_eq!(words[1].word, "reunions");
        assert_eq!(hits[0].1, 1);
    }

    #[test]
    fn respects_case_sensitivity_flag() {
        let mut words = vec![word("Rádio", 0.0, 1.0)];
        let mut sensitive = rule("rádio", "radio");
        sensitive.case_sensitive = true;
        apply(&mut words, &[sensitive]);
        assert_eq!(words[0].word, "Rádio");

        let mut words = vec![word("Rádio", 0.0, 1.0)];
        apply(&mut words, &[rule("rádio", "radio")]);
        // The correction inherits the leading capital of the input.
        assert_eq!(words[0].word, "Radio");
    }

    #[test]
    fn respects_time_window() {
        let mut words = vec![word("man", 9.0, 10.0), word("man", 30.0, 31.0)];
        let mut scoped = rule("man", "manhã");
        scoped.at = Some((8.0, 12.0));
        apply(&mut words, &[scoped]);
        assert_eq!(words[0].word, "manhã");
        assert_eq!(words[1].word, "man");
    }

    #[test]
    fn joins_multi_word_rules_into_one_word() {
        // "open code," -> "OpenCode," and the second token disappears.
        let mut words = vec![word("open", 0.0, 0.4), word("code,", 0.4, 0.9), word("today", 0.9, 1.2)];
        let hits = apply(&mut words, &[rule("open code", "OpenCode")]);
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].word, "OpenCode,");
        assert_eq!(words[0].end, 0.9);
        assert_eq!(words[0].raw_text.as_deref(), Some("open code,"));
        assert_eq!(words[1].word, "today");
        assert_eq!(hits[0].1, 1);
    }

    #[test]
    fn prefers_the_longest_phrase() {
        let mut words = vec![word("excala", 0.0, 0.4), word("draw", 0.4, 0.8)];
        apply(&mut words, &[rule("excala", "Excalidraw"), rule("excala draw", "Excalidraw")]);
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].word, "Excalidraw");
    }

    #[test]
    fn inherits_the_input_case() {
        assert_eq!(match_case("olama", "Ollama"), "Ollama");
        assert_eq!(match_case("Olama", "Ollama"), "Ollama");
        assert_eq!(match_case("OLAMA", "Ollama"), "OLLAMA");
    }
}
