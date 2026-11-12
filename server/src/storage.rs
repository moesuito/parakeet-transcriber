//! Disk housekeeping for the audio archive.
//!
//! Transcripts live in SQLite and are never trimmed by this module. Audio is different:
//! uploads are only useful for a while, so the server keeps the newest `audio_keep` files
//! and deletes the oldest one as soon as a new file pushes past that limit.

use std::path::{Path, PathBuf};

/// The archive file for one job: a sortable timestamp, the job id and the original extension.
pub fn archive_path(dir: &Path, job_id: &str, source_name: &str) -> PathBuf {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let extension: String = Path::new(source_name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect())
        .filter(|e: &String| !e.is_empty())
        .unwrap_or_else(|| "bin".to_string());
    let short = job_id.chars().take(8).collect::<String>();
    dir.join(format!("{stamp}-{short}.{extension}"))
}

/// Files in the archive, oldest first.
pub fn list(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| path.is_file())
                .collect()
        })
        .unwrap_or_default();
    // Names start with a UTC timestamp, so lexical order is chronological.
    files.sort();
    files
}

/// Keeps only the newest `keep` files. Returns how many were deleted.
pub fn trim(dir: &Path, keep: usize) -> usize {
    let files = list(dir);
    if files.len() <= keep {
        return 0;
    }
    let mut removed = 0;
    for path in &files[..files.len() - keep] {
        if std::fs::remove_file(path).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_the_newest_files() {
        let dir = std::env::temp_dir().join(format!("parakeet-trim-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        for index in 0..25 {
            std::fs::write(dir.join(format!("20260101T0000{index:02}Z-abc.{index}wav")), b"x").unwrap();
        }
        assert_eq!(list(&dir).len(), 25);
        assert_eq!(trim(&dir, 20), 5);
        let remaining = list(&dir);
        assert_eq!(remaining.len(), 20);
        // The five oldest are gone, the newest one survives.
        assert!(!remaining.iter().any(|p| p.to_string_lossy().contains("000000Z")));
        assert!(remaining.iter().any(|p| p.to_string_lossy().contains("000024Z")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn nothing_happens_below_the_limit() {
        let dir = std::env::temp_dir().join(format!("parakeet-trim-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("20260101T000000Z-abc.wav"), b"x").unwrap();
        assert_eq!(trim(&dir, 20), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn names_sort_by_time_and_keep_the_extension() {
        let path = archive_path(Path::new("C:/tmp"), "94d12bdc-e601-491d", "audio.wav");
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.ends_with("-94d12bdc.wav"), "{name}");
        assert!(name.starts_with("20"), "{name}");
    }
}
