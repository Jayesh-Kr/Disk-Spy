//! Axum HTTP server for the DiskSpy dashboard.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;
use tracing::info;

use crate::config::Config;
use crate::db::{Database, FileChangeRecord};

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Database>,
    pub config: Arc<Config>,
    pub started_at: Instant,
    pub db_path: PathBuf,
}

#[derive(Serialize)]
pub struct StatusResponse {
    pub status: &'static str,
    pub uptime_seconds: u64,
    pub events_today: i64,
    pub db_size_mb: f64,
}

#[derive(Serialize)]
pub struct ConfigResponse {
    pub dashboard_port: u16,
    pub min_delta_bytes: u64,
    pub debounce_seconds: u64,
    pub retention_days: u32,
    pub drives: Vec<String>,
    pub exclude_paths: Vec<String>,
    pub exclude_processes: Vec<String>,
    pub labels: Vec<(String, String)>,
}

#[derive(Serialize)]
pub struct ApiError {
    pub error: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(self)).into_response()
    }
}

#[derive(Deserialize)]
pub struct LimitParam {
    #[serde(default = "default_limit_100")]
    pub limit: u32,
}

#[derive(Deserialize)]
pub struct DaysParam {
    #[serde(default = "default_days_1")]
    pub days: u32,
}

fn default_limit_100() -> u32 { 100 }
fn default_days_1() -> u32 { 1 }

pub async fn status(State(state): State<AppState>) -> Json<StatusResponse> {
    let events_today = state.db.count_today().unwrap_or(0);
    let db_size_mb = Database::size_mb(&state.db_path);
    Json(StatusResponse {
        status: "running",
        uptime_seconds: state.started_at.elapsed().as_secs(),
        events_today,
        db_size_mb,
    })
}

pub async fn changes(
    State(state): State<AppState>,
    Query(p): Query<LimitParam>,
) -> Result<Json<Vec<FileChangeRecord>>, ApiError> {
    state.db.get_recent_changes(p.limit).map(Json).map_err(|e| ApiError { error: e.to_string() })
}

pub async fn top_growers(
    State(state): State<AppState>,
    Query(p): Query<DaysParam>,
) -> Result<Json<Vec<crate::db::TopGrower>>, ApiError> {
    state.db.get_top_growers(p.days).map(Json).map_err(|e| ApiError { error: e.to_string() })
}

pub async fn daily(
    State(state): State<AppState>,
    Query(p): Query<DaysParam>,
) -> Result<Json<Vec<crate::db::DailyGrowth>>, ApiError> {
    state.db.get_daily_growth(p.days).map(Json).map_err(|e| ApiError { error: e.to_string() })
}

pub async fn largest_files(
    State(state): State<AppState>,
    Query(p): Query<DaysParam>,
) -> Result<Json<Vec<crate::db::FileGrowth>>, ApiError> {
    state.db.get_largest_files(p.days).map(Json).map_err(|e| ApiError { error: e.to_string() })
}

pub async fn config(State(state): State<AppState>) -> Json<ConfigResponse> {
    Json(ConfigResponse {
        dashboard_port: state.config.general.dashboard_port,
        min_delta_bytes: state.config.general.min_delta_bytes,
        debounce_seconds: state.config.general.debounce_seconds,
        retention_days: state.config.general.retention_days,
        drives: state.config.watch.drives.clone(),
        exclude_paths: state.config.watch.exclude_paths.clone(),
        exclude_processes: state.config.watch.exclude_processes.clone(),
        labels: state.config.labels.0.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
    })
}

#[derive(Deserialize)]
pub struct ExcludeProcessBody {
    pub process_name: String,
}

#[derive(Serialize)]
pub struct OkResponse { pub ok: bool }

pub async fn add_exclude_process(
    State(mut state): State<AppState>,
    Json(body): Json<ExcludeProcessBody>,
) -> Result<Json<OkResponse>, ApiError> {
    let cfg = Arc::make_mut(&mut state.config);
    let lower = body.process_name.to_lowercase();
    if !cfg.watch.exclude_processes.iter().any(|p| p.to_lowercase() == lower) {
        cfg.watch.exclude_processes.push(body.process_name);
    }
    Ok(Json(OkResponse { ok: true }))
}

pub async fn dashboard() -> Response {
    let html = include_str!("../assets/dashboard.html");
    ([(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/status", get(status))
        .route("/api/changes", get(changes))
        .route("/api/top-growers", get(top_growers))
        .route("/api/daily", get(daily))
        .route("/api/largest-files", get(largest_files))
        .route("/api/config", get(config))
        .route("/api/config/exclude-process", post(add_exclude_process))
        .route("/", get(dashboard))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

pub async fn serve(state: AppState, port: u16) -> std::io::Result<()> {
    let app = router(state);
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    info!("Dashboard available at http://localhost:{}", port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await
}