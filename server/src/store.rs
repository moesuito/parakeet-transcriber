//! SQLite storage for jobs, correction rules and the transcript cache.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct Rule {
    pub id: String,
    pub from: String,
    pub to: String,
    pub at: Option<(f64, f64)>,
    pub case_sensitive: bool,
    pub created_at: String,
    pub hits: i64,
}

#[derive(Debug, Clone, Default)]
pub struct JobRecord {
    pub id: String,
    pub status: String,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub source: Option<String>,
    pub params: Option<String>,
    pub text: Option<String>,
    pub result: Option<String>,
    pub error: Option<String>,
    pub duration_ms: Option<i64>,
}

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: &str) -> Result<Self> {
        if let Some(parent) = Path::new(path).parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path).with_context(|| format!("opening {path}"))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS corrections (
                id TEXT PRIMARY KEY,
                from_text TEXT NOT NULL,
                to_text TEXT NOT NULL,
                at_start REAL,
                at_end REAL,
                case_sensitive INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                hits INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS jobs (
                id TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL,
                started_at TEXT,
                finished_at TEXT,
                source TEXT,
                params TEXT,
                text TEXT,
                result TEXT,
                error TEXT,
                duration_ms INTEGER
            );
            CREATE INDEX IF NOT EXISTS jobs_created ON jobs(created_at DESC);
            CREATE TABLE IF NOT EXISTS cache (
                key TEXT PRIMARY KEY,
                created_at TEXT NOT NULL,
                payload TEXT NOT NULL,
                hits INTEGER NOT NULL DEFAULT 0
            );
            "#,
        )?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    // ---- corrections -----------------------------------------------------

    pub fn rules(&self) -> Result<Vec<Rule>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, from_text, to_text, at_start, at_end, case_sensitive, created_at, hits
             FROM corrections ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], |r| {
            let at_start: Option<f64> = r.get(3)?;
            let at_end: Option<f64> = r.get(4)?;
            Ok(Rule {
                id: r.get(0)?,
                from: r.get(1)?,
                to: r.get(2)?,
                at: match (at_start, at_end) {
                    (Some(a), Some(b)) => Some((a, b)),
                    _ => None,
                },
                case_sensitive: r.get::<_, i64>(5)? != 0,
                created_at: r.get(6)?,
                hits: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn add_rule(&self, rule: &Rule) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO corrections
             (id, from_text, to_text, at_start, at_end, case_sensitive, created_at, hits)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                rule.id,
                rule.from,
                rule.to,
                rule.at.map(|a| a.0),
                rule.at.map(|a| a.1),
                rule.case_sensitive as i64,
                rule.created_at,
                rule.hits
            ],
        )?;
        Ok(())
    }

    pub fn delete_rule(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute("DELETE FROM corrections WHERE id = ?1", params![id])? > 0)
    }

    pub fn add_hits(&self, count: &[(String, i64)]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        for (id, n) in count {
            conn.execute(
                "UPDATE corrections SET hits = hits + ?1 WHERE id = ?2",
                params![n, id],
            )?;
        }
        Ok(())
    }

    // ---- cache -----------------------------------------------------------

    pub fn cache_get(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let payload: Option<String> = conn
            .query_row("SELECT payload FROM cache WHERE key = ?1", params![key], |r| r.get(0))
            .optional()?;
        if payload.is_some() {
            let _ = conn.execute("UPDATE cache SET hits = hits + 1 WHERE key = ?1", params![key]);
        }
        Ok(payload)
    }

    pub fn cache_put(&self, key: &str, payload: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO cache (key, created_at, payload, hits) VALUES (?1, ?2, ?3, 0)",
            params![key, crate::formats::now(), payload],
        )?;
        Ok(())
    }

    // ---- jobs ------------------------------------------------------------

    pub fn job_create(&self, job: &JobRecord) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO jobs (id, status, created_at, source, params) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![job.id, job.status, job.created_at, job.source, job.params],
        )?;
        Ok(())
    }

    pub fn job_started(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE jobs SET status = 'running', started_at = ?1 WHERE id = ?2",
            params![crate::formats::now(), id],
        )?;
        Ok(())
    }

    pub fn job_finished(&self, id: &str, text: &str, payload: &str, duration_ms: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE jobs SET status = 'done', finished_at = ?1, text = ?2, result = ?3, duration_ms = ?4 WHERE id = ?5",
            params![crate::formats::now(), text, payload, duration_ms, id],
        )?;
        Ok(())
    }

    pub fn job_failed(&self, id: &str, error: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE jobs SET status = 'failed', finished_at = ?1, error = ?2 WHERE id = ?3",
            params![crate::formats::now(), error, id],
        )?;
        Ok(())
    }

    pub fn job_get(&self, id: &str) -> Result<Option<JobRecord>> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                "SELECT id, status, created_at, started_at, finished_at, source, params, text, result, error, duration_ms
                 FROM jobs WHERE id = ?1",
                params![id],
                map_job,
            )
            .optional()?;
        Ok(row)
    }

    pub fn job_list(&self, limit: i64) -> Result<Vec<JobRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, status, created_at, started_at, finished_at, source, params, text, result, error, duration_ms
             FROM jobs ORDER BY created_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], map_job)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn job_delete(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute("DELETE FROM jobs WHERE id = ?1", params![id])? > 0)
    }

    pub fn purge(&self, days: i64) -> Result<usize> {
        if days <= 0 {
            return Ok(0);
        }
        let cutoff = (chrono::Utc::now() - chrono::Duration::days(days))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "DELETE FROM jobs WHERE created_at < ?1 AND status IN ('done', 'failed')",
            params![cutoff],
        )?;
        Ok(n)
    }
}

fn map_job(r: &rusqlite::Row<'_>) -> rusqlite::Result<JobRecord> {
    Ok(JobRecord {
        id: r.get(0)?,
        status: r.get(1)?,
        created_at: r.get(2)?,
        started_at: r.get(3)?,
        finished_at: r.get(4)?,
        source: r.get(5)?,
        params: r.get(6)?,
        text: r.get(7)?,
        result: r.get(8)?,
        error: r.get(9)?,
        duration_ms: r.get(10)?,
    })
}
