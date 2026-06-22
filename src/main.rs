// Allow `parking_lot::Mutex` without bringing the type in scope here.
extern crate parking_lot;

#[cfg(windows)]
pub mod etw;
pub mod config;
pub mod db;
pub mod debouncer;
pub mod process_cache;
pub mod server;

#[cfg(test)]
mod integration_tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::{mpsc, watch};
    use tokio::time::sleep;

    #[tokio::test]
    async fn full_pipeline_etw_to_db() {
        // 1. In-memory DB
        let db = Arc::new(db::Database::open_in_memory().unwrap());

        // 2. Config
        let cfg = Arc::new(config::Config::default());

        // 3. Process cache
        let cache = Arc::new(process_cache::ProcessCache::new(Duration::from_secs(30)));

        // 4. Channels
        let (raw_tx, raw_rx) = mpsc::channel::<debouncer::RawFileEvent>(64);
        let (record_tx, record_rx) = mpsc::channel::<db::FileChangeRecord>(16);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        // 5. Debouncer
        let debouncer = debouncer::Debouncer::new(1000, Duration::from_millis(50));
        let debouncer_handle = tokio::spawn(debouncer::run_debouncer(
            cfg.clone(),
            cache.clone(),
            debouncer.clone(),
            raw_rx,
            record_tx.clone(),
            shutdown_rx,
        ));

        // 6. DB writer
        let db_clone = db.clone();
        let writer_handle = tokio::spawn(async move {
            let mut rx = record_rx;
            while let Some(r) = rx.recv().await {
                db_clone.insert(&r).unwrap();
            }
        });

        // 7. Simulate a 1 MB write event from PID 1 (whatever that resolves to).
        let path = r"C:\fake\docker\layer.bin".to_string();
        for _ in 0..1000 {
            raw_tx
                .send(debouncer::RawFileEvent {
                    pid: 1,
                    file_path: path.clone(),
                    bytes_written: 1024,
                    category_hint: String::new(),
                })
                .await
                .unwrap();
        }

        // 8. Wait for the debouncer to flush (1.5x the debounce window so
        // the 500ms flush tick definitely fires).
        sleep(Duration::from_millis(200)).await;
        let _ = shutdown_tx.send(true);
        // Wait for the writer task to actually receive + insert the record.
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if db.get_recent_changes(10).unwrap().len() >= 1 {
                    break;
                }
                sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        let _ = debouncer_handle.await;
        drop(raw_tx);
        drop(record_tx);
        let _ = writer_handle.await;

        // 9. Verify
        let recent = db.get_recent_changes(10).unwrap();
        assert_eq!(recent.len(), 1, "expected exactly one debounced row");
        assert_eq!(recent[0].delta_bytes, 1_024_000);
        assert!(
            recent[0].category == "Docker" || recent[0].category.contains("test"),
            "category was {}",
            recent[0].category
        );
    }
}

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[cfg(windows)]
use crate::etw::{elevation_error_message, is_elevated, refresh_dos_devices};

use crate::config::{default_config_path, Config};
use crate::db::Database;
use crate::debouncer::{run_debouncer, Debouncer};
use crate::process_cache::ProcessCache;
use crate::server::{serve, AppState};

#[tokio::main]
async fn main() -> Result<()> {
    // 1. Tracing init
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,diskspy=info")),
        )
        .with_target(false)
        .init();

    info!("DiskSpy v0.1.0 starting…");

    // 2. Elevation check (Windows only). ETW kernel tracing needs it.
    #[cfg(windows)]
    {
        if !is_elevated() {
            eprintln!("{}", elevation_error_message());
            std::process::exit(1);
        }
    }

    // 3. Config
    let cfg_path = default_config_path();
    let config = Arc::new(
        Config::load(&cfg_path)
            .with_context(|| format!("loading config from {}", cfg_path.display()))?,
    );
    log_config(&config);

    // 4. Database
    let db_path = PathBuf::from("diskspy.db");
    let db = Arc::new(Database::open(&db_path)?);
    let deleted = db.delete_older_than(config.general.retention_days)?;
    if deleted > 0 {
        info!(deleted, "pruned old rows beyond retention");
    }

    // 5. Process cache
    let cache = Arc::new(ProcessCache::new(Duration::from_secs(30)));

    // 6. Channels
    let (raw_tx, raw_rx) = mpsc::channel::<debouncer::RawFileEvent>(4096);
    let (record_tx, record_rx) = mpsc::channel::<db::FileChangeRecord>(256);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // 7. Debouncer
    let debouncer = Debouncer::new(
        config.general.min_delta_bytes as i64,
        Duration::from_secs(config.general.debounce_seconds),
    );
    let debouncer_handle = tokio::spawn(run_debouncer(
        config.clone(),
        cache.clone(),
        debouncer.clone(),
        raw_rx,
        record_tx,
        shutdown_rx.clone(),
    ));

    // 8. DB writer task
    let db_writer = tokio::spawn(db_writer_task(db.clone(), record_rx));

    // 9. ETW consumer (Windows only) — spawn on its own blocking thread.
    #[cfg(windows)]
    {
        let raw_tx_clone = raw_tx.clone();
        let dos_devices = Arc::new(parking_lot::Mutex::new(refresh_dos_devices()));
        info!(devices = dos_devices.lock().len(), "scanned DOS devices");
        std::thread::spawn(move || {
            if let Err(e) = etw::run_etw_consumer(raw_tx_clone, dos_devices) {
                warn!(?e, "ETW consumer stopped with error");
            }
        });
    }

    // Non-Windows stub so the binary still compiles for tests.
    #[cfg(not(windows))]
    {
        warn!("ETW kernel tracing is only available on Windows; exiting.");
        return Ok(());
    }

    // 10. HTTP server
    let app_state = AppState {
        db: db.clone(),
        config: config.clone(),
        started_at: Instant::now(),
        db_path: db_path.clone(),
    };

    // 11. Ctrl+C handler — flush debouncer, close DB.
    let shutdown_for_signal = shutdown_tx.clone();
    let server_handle = tokio::spawn(async move {
        if let Err(e) = serve(app_state, config.general.dashboard_port).await {
            warn!(?e, "server stopped");
        }
        let _ = shutdown_for_signal.send(true);
    });

    tokio::signal::ctrl_c().await.ok();
    info!("Ctrl+C received — flushing…");

    // Trigger graceful shutdown.
    let _ = shutdown_tx.send(true);

    // Give the debouncer a moment to flush pending aggregations.
    let _ = tokio::time::timeout(Duration::from_secs(3), async {
        let _ = debouncer_handle.await;
    })
    .await;

    // Drop the raw_tx so the debouncer's recv() returns None.
    drop(raw_tx);

    // Wait for the DB writer.
    let _ = db_writer.await;
    let _ = server_handle.await;

    info!("DiskSpy stopped.");
    Ok(())
}

async fn db_writer_task(db: Arc<Database>, mut rx: mpsc::Receiver<db::FileChangeRecord>) {
    while let Some(record) = rx.recv().await {
        if let Err(e) = db.insert(&record) {
            warn!(?e, "failed to insert change record");
        }
    }
}

fn log_config(cfg: &Config) {
    info!(
        port = cfg.general.dashboard_port,
        min_delta = cfg.general.min_delta_bytes,
        debounce = cfg.general.debounce_seconds,
        retention_days = cfg.general.retention_days,
        drives = ?cfg.watch.drives,
        "Configuration loaded"
    );
}
