//! Word corrections applied to every transcript before it is formatted.
//!
//! A rule replaces one word (whole word, case-insensitive by default) and keeps the
//! original text in `raw_text`, so a review can always see what the model heard.

use crate::formats::Word;

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

/// Applies every rule to every word. Returns how many times each rule fired.
pub fn apply(words: &mut [Word], rules: &[Correction]) -> Vec<(String, i64)> {
    let mut hits: Vec<(String, i64)> = Vec::new();
    if rules.is_empty() {
        return hits;
    }

    for word in words.iter_mut() {
        let (prefix, core, suffix) = split_affixes(&word.word);
        if core.is_empty() {
            continue;
        }
        let midpoint = (word.start + word.end) / 2.0;
        for rule in rules {
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
            word.word = format!("{prefix}{}{suffix}", rule.to);
            match hits.iter_mut().find(|(id, _)| id == &rule.id) {
                Some(entry) => entry.1 += 1,
                None => hits.push((rule.id.clone(), 1)),
            }
            break;
        }
    }
    hits
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
        // A different word must not be touched.
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
        assert_eq!(words[0].word, "radio");
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
}
