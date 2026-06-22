//! Per-file debouncer.
//!
//! Without this, 10,000 small write events to the same file become 10,000
//! database rows. The debouncer accumulates bytes per (file_path) until
//! activity stops for `debounce_seconds`, then emits a single
//! `FileChangeRecord`.
//!
//! Threshold: events whose absolute accumulated delta is smaller than
//! `min_delta_bytes` are silently dropped.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use dashmap::DashMap;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, trace};

use crate::config::Config;
use crate::db::FileChangeRecord;
use crate::process_cache::ProcessCache;

#[derive(Debug, Clone)]
pub struct RawFileEvent {
    pub pid: u32,
    pub file_path: String,
    pub bytes_written: u64,
    pub category_hint: String,
}

#[derive(Debug)]
struct FileState {
    accumulated: i64,
    pid: u32,
    process_name: String,
    last_event: Instant,
}

/// The debouncer is shared between the consumer thread and the background
/// flush task, so it lives behind an `Arc`.
pub struct Debouncer {
    state: DashMap<String, FileState>,
    threshold: i64,
    debounce: Duration,
}

impl Debouncer {
    pub fn new(threshold: i64, debounce: Duration) -> Arc<Self> {
        Arc::new(Self {
            state: DashMap::new(),
            threshold,
            debounce,
        })
    }

    /// Feed a raw event. Returns true if the event was added to an existing
    /// aggregation; false if a fresh file entry was created. Used by tests
    /// only.
    pub fn feed(&self, event: &RawFileEvent, process_name: &str) {
        let mut entry = self.state.entry(event.file_path.clone()).or_insert_with(|| {
            FileState {
                accumulated: 0,
                pid: event.pid,
                process_name: process_name.to_string(),
                last_event: Instant::now(),
            }
        });
        entry.accumulated = entry.accumulated.saturating_add(event.bytes_written as i64);
        entry.pid = event.pid;
        entry.process_name = process_name.to_string();
        entry.last_event = Instant::now();
    }

    /// Drain entries that have been quiet for longer than the debounce window.
    /// Returns the records to persist. Caller (the writer task) is responsible
    /// for insertion; this function just produces them.
    pub fn drain_ready(&self, cfg: &Config, cache: &ProcessCache) -> Vec<FileChangeRecord> {
        let now = Instant::now();
        let mut out = Vec::new();
        let mut to_remove: Vec<String> = Vec::new();

        for entry in self.state.iter() {
            if now.duration_since(entry.value().last_event) >= self.debounce {
                let abs = entry.value().accumulated.abs();
                if abs >= self.threshold {
                    // Re-resolve PID at emission time to capture the most
                    // recent process name (the entry was last-touched at
                    // the most recent write, but the process may have
                    // exited; cache TTL is short, this is safe).
                    let name = cache.resolve(entry.value().pid);
                    let label = cfg.label_for(&name);
                    let category = detect_category(&entry.key(), &name);
                    out.push(FileChangeRecord {
                        id: None,
                        changed_at: Utc::now(),
                        process_name: name,
                        process_label: label,
                        file_path: entry.key().clone(),
                        delta_bytes: entry.value().accumulated,
                        category,
                    });
                } else {
                    trace!(file = %entry.key(), accumulated = entry.value().accumulated, "below threshold, dropping");
                }
                to_remove.push(entry.key().clone());
            }
        }

        for k in to_remove {
            self.state.remove(&k);
        }
        out
    }

    /// Number of pending files. Used by tests.
    pub fn pending(&self) -> usize {
        self.state.len()
    }

    /// Force-flush everything (used on shutdown).
    pub fn drain_all(&self, cfg: &Config, cache: &ProcessCache) -> Vec<FileChangeRecord> {
        let mut out = Vec::new();
        for entry in self.state.iter() {
            let abs = entry.value().accumulated.abs();
            if abs >= self.threshold {
                let name = cache.resolve(entry.value().pid);
                let label = cfg.label_for(&name);
                let category = detect_category(&entry.key(), &name);
                out.push(FileChangeRecord {
                    id: None,
                    changed_at: Utc::now(),
                    process_name: name,
                    process_label: label,
                    file_path: entry.key().clone(),
                    delta_bytes: entry.value().accumulated,
                    category,
                });
            }
        }
        self.state.clear();
        out
    }
}

/// Run the debouncer loop: pull raw events from `input`, emit debounced
/// records to `output`, periodically flushing entries that have been quiet.
pub async fn run_debouncer(
    cfg: Arc<Config>,
    cache: Arc<ProcessCache>,
    debouncer: Arc<Debouncer>,
    mut input: mpsc::Receiver<RawFileEvent>,
    output: mpsc::Sender<FileChangeRecord>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    let final_flush = debouncer.drain_all(&cfg, &cache);
                    for r in final_flush {
                        let _ = output.send(r).await;
                    }
                    break;
                }
            }
            maybe = input.recv() => {
                match maybe {
                    Some(event) => {
                        let name = cache.resolve(event.pid);
                        if cfg.should_exclude_process(&name) {
                            debug!(pid = event.pid, name = %name, "excluded by process filter");
                            continue;
                        }
                        if cfg.should_exclude_path(&event.file_path) {
                            trace!(file = %event.file_path, "excluded by path filter");
                            continue;
                        }
                        debouncer.feed(&event, &name);
                    }
                    None => break,
                }
            }
            _ = tick.tick() => {
                let ready = debouncer.drain_ready(&cfg, &cache);
                for r in ready {
                    if output.send(r).await.is_err() {
                        break;
                    }
                }
                cache.evict_expired();
            }
        }
    }
}

/// Derive a category string from a file path and process name. Used by both
/// the debouncer and the dashboard.
pub fn detect_category(file_path: &str, process_name: &str) -> String {
    let path_lower = file_path.to_lowercase();
    let proc_lower = process_name.to_lowercase();

    if path_lower.contains("\\docker\\") || path_lower.contains("\\.docker\\")
        || proc_lower.contains("docker")
    {
        return "Docker".into();
    }
    if path_lower.contains("\\node_modules\\")
        || proc_lower == "node.exe"
        || proc_lower == "npm"
    {
        return "Node/npm".into();
    }
    if path_lower.contains("\\.cargo\\") || proc_lower == "cargo.exe" {
        return "Rust/Cargo".into();
    }
    if path_lower.contains("\\pip\\")
        || path_lower.contains("\\site-packages\\")
        || path_lower.contains("\\.venv\\")
        || proc_lower.starts_with("python")
    {
        return "Python".into();
    }
    if path_lower.contains("\\.ollama\\") || proc_lower == "ollama.exe" {
        return "Ollama (AI Models)".into();
    }
    if path_lower.contains("\\claude\\") && path_lower.contains("appdata") {
        return "Claude Desktop".into();
    }
    if path_lower.contains("\\.git\\") || proc_lower == "git.exe" {
        return "Git".into();
    }
    if path_lower.contains("\\wsl\\")
        || path_lower.contains("\\lxss\\")
        || proc_lower == "vmmem"
        || proc_lower == "wsl.exe"
    {
        return "WSL2".into();
    }
    if path_lower.contains("\\appdata\\local\\programs\\") {
        return "App Installer".into();
    }
    process_name.trim_end_matches(".exe").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(path: &str, pid: u32, bytes: u64) -> RawFileEvent {
        RawFileEvent {
            pid,
            file_path: path.into(),
            bytes_written: bytes,
            category_hint: String::new(),
        }
    }

    #[test]
    fn small_writes_below_threshold_are_dropped() {
        let db = Debouncer::new(50_000, Duration::from_millis(50));
        // 10 events of 1 KB each = 10 KB total, below 50 KB threshold.
        for _ in 0..10 {
            db.feed(&ev(r"C:\a.bin", 1, 1024), "test.exe");
        }
        assert_eq!(db.pending(), 1);
    }

    #[test]
    fn aggregate_above_threshold_emits_one_record() {
        // Build a minimal config and cache just enough for drain_ready.
        let cfg = Arc::new(Config::default());
        let cache = Arc::new(ProcessCache::new(Duration::from_secs(30)));

        let db = Debouncer::new(50_000, Duration::from_millis(50));
        for _ in 0..1000 {
            db.feed(&ev(r"C:\big.bin", 99999, 1024), "test.exe");
        }
        std::thread::sleep(Duration::from_millis(80));
        let ready = db.drain_ready(&cfg, &cache);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].delta_bytes, 1_024_000);
        assert_eq!(db.pending(), 0);
    }

    #[test]
    fn different_files_separate_records() {
        let cfg = Arc::new(Config::default());
        let cache = Arc::new(ProcessCache::new(Duration::from_secs(30)));

        let db = Debouncer::new(50_000, Duration::from_millis(50));
        for _ in 0..100 {
            db.feed(&ev(r"C:\a.bin", 1, 1024), "test.exe");
            db.feed(&ev(r"C:\b.bin", 1, 1024), "test.exe");
        }
        std::thread::sleep(Duration::from_millis(80));
        let ready = db.drain_ready(&cfg, &cache);
        assert_eq!(ready.len(), 2);
        let mut paths: Vec<_> = ready.iter().map(|r| r.file_path.clone()).collect();
        paths.sort();
        assert_eq!(paths, vec![r"C:\a.bin", r"C:\b.bin"]);
    }

    #[test]
    fn detect_category_docker() {
        assert_eq!(detect_category(r"C:\ProgramData\Docker\volumes\_data\file", "com.docker.backend.exe"), "Docker");
        assert_eq!(detect_category(r"C:\some\path\file.bin", "docker.exe"), "Docker");
    }

    #[test]
    fn detect_category_python() {
        assert_eq!(detect_category(r"C:\Users\me\proj\.venv\lib\file", "python.exe"), "Python");
    }
}