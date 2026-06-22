//! Database layer for DiskSpy.
//!
//! Stores per-file change events in `change_log` and a per-day aggregate
//! in `daily_summary`. The aggregate is maintained lazily at write time so
//! dashboard queries stay cheap.

use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, TimeZone, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

/// A single debounced file change event, as the dashboard sees it.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct FileChangeRecord {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    pub changed_at: DateTime<Utc>,
    pub process_name: String,
    pub process_label: String,
    pub file_path: String,
    pub delta_bytes: i64,
    pub category: String,
}

/// A per-day per-process aggregate used by the dashboard chart.
#[derive(Debug, Clone, Serialize)]
pub struct DailyGrowth {
    pub date: String,
    pub process_label: String,
    pub total_bytes: i64,
    pub event_count: i64,
}

/// A row used by the "largest files" panel.
#[derive(Debug, Clone, Serialize)]
pub struct FileGrowth {
    pub file_path: String,
    pub process_label: String,
    pub total_bytes: i64,
}

/// A row used by the "top growers" panel.
#[derive(Debug, Clone, Serialize)]
pub struct TopGrower {
    pub process_label: String,
    pub total_bytes: i64,
    pub event_count: i64,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS change_log (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    changed_at      INTEGER NOT NULL,
    process_name    TEXT    NOT NULL,
    process_label   TEXT    NOT NULL,
    file_path       TEXT    NOT NULL,
    delta_bytes     INTEGER NOT NULL,
    category        TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_changed_at ON change_log(changed_at);
CREATE INDEX IF NOT EXISTS idx_process_name ON change_log(process_name);
CREATE INDEX IF NOT EXISTS idx_category ON change_log(category);

CREATE TABLE IF NOT EXISTS daily_summary (
    date_str        TEXT NOT NULL,
    process_label   TEXT NOT NULL,
    category        TEXT NOT NULL,
    total_bytes     INTEGER NOT NULL,
    event_count     INTEGER NOT NULL,
    PRIMARY KEY (date_str, process_label, category)
);
"#;

/// Thread-safe handle to the SQLite database.
///
/// SQLite is single-writer; we serialize writes through a Tokio mutex so the
/// HTTP server, ETW consumer and debouncer can all call `insert` concurrently.
pub struct Database {
    conn: parking_lot::Mutex<Connection>,
}

impl Database {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path.as_ref())
            .with_context(|| format!("opening database at {}", path.as_ref().display()))?;
        let db = Self { conn: parking_lot::Mutex::new(conn) };
        db.run_schema()?;
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("opening in-memory database")?;
        let db = Self { conn: parking_lot::Mutex::new(conn) };
        db.run_schema()?;
        Ok(db)
    }

    fn run_schema(&self) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute_batch(SCHEMA).context("running schema migrations")?;
        Ok(())
    }

    /// Insert a single change record and update the daily aggregate.
    pub fn insert(&self, record: &FileChangeRecord) -> Result<i64> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;

        let ts = record.changed_at.timestamp();
        let id = tx.execute(
            "INSERT INTO change_log (changed_at, process_name, process_label, file_path, delta_bytes, category)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                ts,
                record.process_name,
                record.process_label,
                record.file_path,
                record.delta_bytes,
                record.category,
            ],
        )?;

        // Maintain the daily aggregate so dashboard queries stay cheap.
        let date_str = record.changed_at.format("%Y-%m-%d").to_string();
        tx.execute(
            "INSERT INTO daily_summary (date_str, process_label, category, total_bytes, event_count)
             VALUES (?1, ?2, ?3, ?4, 1)
             ON CONFLICT(date_str, process_label, category) DO UPDATE SET
                total_bytes = total_bytes + excluded.total_bytes,
                event_count = event_count + 1",
            params![
                date_str,
                record.process_label,
                record.category,
                record.delta_bytes,
            ],
        )?;

        tx.commit()?;
        Ok(id as i64)
    }

    pub fn get_recent_changes(&self, limit: u32) -> Result<Vec<FileChangeRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, changed_at, process_name, process_label, file_path, delta_bytes, category
             FROM change_log ORDER BY changed_at DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], Self::map_record)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn get_top_growers(&self, days: u32) -> Result<Vec<TopGrower>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT process_label, SUM(delta_bytes) as total, COUNT(*) as events
             FROM change_log
             WHERE changed_at > strftime('%s', 'now', '-' || ?1 || ' days')
             AND delta_bytes > 0
             GROUP BY process_label ORDER BY total DESC LIMIT 20",
        )?;
        let rows = stmt.query_map(params![days as i64], |row| {
            Ok(TopGrower {
                process_label: row.get(0)?,
                total_bytes: row.get(1)?,
                event_count: row.get(2)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn get_daily_growth(&self, days: u32) -> Result<Vec<DailyGrowth>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT date(changed_at, 'unixepoch') as d, process_label, SUM(delta_bytes) as total, COUNT(*) as events
             FROM change_log
             WHERE changed_at > strftime('%s', 'now', '-' || ?1 || ' days')
             AND delta_bytes > 0
             GROUP BY d, process_label ORDER BY d ASC",
        )?;
        let rows = stmt.query_map(params![days as i64], |row| {
            Ok(DailyGrowth {
                date: row.get(0)?,
                process_label: row.get(1)?,
                total_bytes: row.get(2)?,
                event_count: row.get(3)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn get_largest_files(&self, days: u32) -> Result<Vec<FileGrowth>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT file_path, process_label, SUM(delta_bytes) as total
             FROM change_log
             WHERE changed_at > strftime('%s', 'now', '-' || ?1 || ' days')
             GROUP BY file_path ORDER BY total DESC LIMIT 50",
        )?;
        let rows = stmt.query_map(params![days as i64], |row| {
            Ok(FileGrowth {
                file_path: row.get(0)?,
                process_label: row.get(1)?,
                total_bytes: row.get(2)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn count_today(&self) -> Result<i64> {
        let conn = self.conn.lock();
        let n: Option<i64> = conn
            .query_row(
                "SELECT COUNT(*) FROM change_log WHERE changed_at > strftime('%s', 'now', 'start of day')",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(n.unwrap_or(0))
    }

    pub fn delete_older_than(&self, days: u32) -> Result<usize> {
        let conn = self.conn.lock();
        let n = conn.execute(
            "DELETE FROM change_log WHERE changed_at < strftime('%s', 'now', '-' || ?1 || ' days')",
            params![days as i64],
        )?;
        Ok(n)
    }

    /// Return the on-disk size of the database file in MB.
    pub fn size_mb(path: &Path) -> f64 {
        std::fs::metadata(path)
            .map(|m| m.len() as f64 / 1_048_576.0)
            .unwrap_or(0.0)
    }

    fn map_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileChangeRecord> {
        let id: i64 = row.get(0)?;
        let ts: i64 = row.get(1)?;
        let changed_at = Utc.timestamp_opt(ts, 0).single().unwrap_or_else(Utc::now);
        Ok(FileChangeRecord {
            id: Some(id),
            changed_at,
            process_name: row.get(2)?,
            process_label: row.get(3)?,
            file_path: row.get(4)?,
            delta_bytes: row.get(5)?,
            category: row.get(6)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn sample(label: &str, bytes: i64) -> FileChangeRecord {
        FileChangeRecord {
            id: None,
            changed_at: Utc::now(),
            process_name: "docker.exe".into(),
            process_label: label.into(),
            file_path: r"C:\ProgramData\Docker\volumes\_data\layer.tar".into(),
            delta_bytes: bytes,
            category: "Docker".into(),
        }
    }

    #[test]
    fn insert_and_query_recent() {
        let db = Database::open_in_memory().unwrap();
        db.insert(&sample("Docker Desktop", 1_000_000)).unwrap();
        db.insert(&sample("Docker Desktop", 2_000_000)).unwrap();

        let rows = db.get_recent_changes(10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].delta_bytes, 2_000_000);
        assert_eq!(rows[1].delta_bytes, 1_000_000);
    }

    #[test]
    fn top_growers_sums_per_label() {
        let db = Database::open_in_memory().unwrap();
        db.insert(&sample("Docker Desktop", 500)).unwrap();
        db.insert(&sample("Docker Desktop", 250)).unwrap();
        db.insert(&sample("Node.js / npm", 100)).unwrap();

        let top = db.get_top_growers(1).unwrap();
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].process_label, "Docker Desktop");
        assert_eq!(top[0].total_bytes, 750);
        assert_eq!(top[0].event_count, 2);
    }

    #[test]
    fn daily_growth_groups_by_date() {
        let db = Database::open_in_memory().unwrap();
        db.insert(&sample("Docker Desktop", 100)).unwrap();
        db.insert(&sample("Docker Desktop", 200)).unwrap();

        let daily = db.get_daily_growth(7).unwrap();
        assert_eq!(daily.len(), 1);
        assert_eq!(daily[0].total_bytes, 300);
    }

    #[test]
    fn deletion_respects_retention() {
        let db = Database::open_in_memory().unwrap();
        db.insert(&sample("Docker Desktop", 100)).unwrap();
        // 1-day retention: rows from right now are kept.
        assert_eq!(db.delete_older_than(1).unwrap(), 0);
        assert_eq!(db.get_recent_changes(10).unwrap().len(), 1);
        // Manually backdate the row and ensure a 0-day retention deletes it.
        {
            let conn = db.conn.lock();
            conn.execute(
                "UPDATE change_log SET changed_at = strftime('%s', 'now', '-2 days')",
                [],
            )
            .unwrap();
        }
        assert_eq!(db.delete_older_than(1).unwrap(), 1);
        assert_eq!(db.get_recent_changes(10).unwrap().len(), 0);
    }
}