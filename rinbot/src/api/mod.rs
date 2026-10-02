use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    Arc,
};
use std::time::Instant;

use axum::{
    extract::{Path, Query, State},
    http::{header, Request, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tokio::sync::broadcast;

use crate::{
    adapters::sqlite::{SnapshotCounts, SqliteStore},
    runtime::config::Cli,
};

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<SqliteStore>,
    pub runtime: Arc<RuntimeState>,
    pub events: broadcast::Sender<ApiEvent>,
    pub config: Arc<Cli>,
}

pub struct RuntimeState {
    pub started_at: Instant,
    pub onebot_connected: AtomicBool,
    pub core_ready: AtomicBool,
    pub queue_depth: AtomicUsize,
    pub version: AtomicU64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ApiEvent {
    pub id: u64,
    pub version: u64,
    pub kind: String,
    pub payload: Value,
}

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
    name: &'static str,
    version: &'static str,
    onebot_connected: bool,
    timestamp_ms: u128,
}

#[derive(Serialize)]
struct StatusResponse {
    mode: &'static str,
    onebot_connected: bool,
    core_ready: bool,
    queue_depth: usize,
    queue_capacity: usize,
    uptime_seconds: u64,
    migration_version: i64,
}

#[derive(Serialize)]
struct SnapshotResponse {
    version: u64,
    status: StatusResponse,
    counts: SnapshotCounts,
}

#[derive(Serialize)]
struct ConfigResponse {
    host: String,
    port: u16,
    data_dir: String,
    ui_dir: String,
    onebot_ws_url: String,
    onebot_http_url: String,
    connect_onebot: bool,
    observe_only: bool,
    api_token_configured: bool,
    onebot_token_configured: bool,
}

#[derive(Deserialize)]
struct PageQuery {
    limit: Option<u32>,
    offset: Option<u32>,
}

#[derive(Deserialize)]
struct ReconcileRequest {
    outcome: String,
}

impl RuntimeState {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            onebot_connected: AtomicBool::new(false),
            core_ready: AtomicBool::new(true),
            queue_depth: AtomicUsize::new(0),
            version: AtomicU64::new(0),
        }
    }
}

impl AppState {
    pub fn publish(&self, kind: impl Into<String>, payload: Value) {
        let version = self.runtime.version.fetch_add(1, Ordering::AcqRel) + 1;
        let event = ApiEvent {
            id: version,
            version,
            kind: kind.into(),
            payload,
        };
        let _ = self.events.send(event);
    }

    fn status(&self, migration_version: i64) -> StatusResponse {
        StatusResponse {
            mode: "observe",
            onebot_connected: self.runtime.onebot_connected.load(Ordering::Acquire),
            core_ready: self.runtime.core_ready.load(Ordering::Acquire),
            queue_depth: self.runtime.queue_depth.load(Ordering::Acquire),
            queue_capacity: self.config.queue_capacity.max(1),
            uptime_seconds: self.runtime.started_at.elapsed().as_secs(),
            migration_version,
        }
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", axum::routing::get(health))
        .route("/api/status", axum::routing::get(status))
        .route("/api/snapshot", axum::routing::get(snapshot))
        .route("/api/config", axum::routing::get(config))
        .route("/api/messages", axum::routing::get(messages))
        .route("/api/outbox/unknown", axum::routing::get(unknown_outbox))
        .route(
            "/api/outbox/{id}/reconcile",
            axum::routing::post(reconcile_outbox),
        )
        .route("/api/events", axum::routing::get(events))
        .layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .with_state(state)
}

async fn authenticate(
    State(state): State<AppState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    if state.config.api_token.is_empty() {
        return next.run(request).await;
    }
    let supplied = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .or_else(|| {
            request
                .headers()
                .get(header::COOKIE)
                .and_then(|value| value.to_str().ok())
                .and_then(cookie_token)
        });
    let valid = supplied
        .map(|candidate| {
            candidate
                .as_bytes()
                .ct_eq(state.config.api_token.as_bytes())
                .into()
        })
        .unwrap_or(false);
    if valid {
        next.run(request).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

fn cookie_token(header_value: &str) -> Option<&str> {
    header_value
        .split(';')
        .map(str::trim)
        .find_map(|item| item.strip_prefix("rinbot_session="))
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    Json(HealthResponse {
        ok: true,
        name: "RInBot",
        version: env!("CARGO_PKG_VERSION"),
        onebot_connected: state.runtime.onebot_connected.load(Ordering::Acquire),
        timestamp_ms: current_time_ms(),
    })
}

async fn status(State(state): State<AppState>) -> impl IntoResponse {
    let migration_version = state.store.migration_version().unwrap_or_default();
    Json(state.status(migration_version))
}

async fn snapshot(State(state): State<AppState>) -> impl IntoResponse {
    let counts = state.store.counts().unwrap_or_default();
    let migration_version = state.store.migration_version().unwrap_or_default();
    Json(SnapshotResponse {
        version: state.runtime.version.load(Ordering::Acquire),
        status: state.status(migration_version),
        counts,
    })
}

async fn config(State(state): State<AppState>) -> impl IntoResponse {
    Json(ConfigResponse {
        host: state.config.host.clone(),
        port: state.config.port,
        data_dir: state.config.data_dir.display().to_string(),
        ui_dir: state.config.ui_dir.display().to_string(),
        onebot_ws_url: state.config.onebot_ws_url.clone(),
        onebot_http_url: state.config.onebot_http_url.clone(),
        connect_onebot: state.config.connect_onebot,
        observe_only: true,
        api_token_configured: !state.config.api_token.is_empty(),
        onebot_token_configured: !state.config.onebot_token.is_empty(),
    })
}

async fn messages(
    State(state): State<AppState>,
    Query(query): Query<PageQuery>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let offset = query.offset.unwrap_or(0);
    match state.store.messages(limit, offset) {
        Ok(messages) => {
            Json(json!({ "messages": messages, "limit": limit, "offset": offset })).into_response()
        }
        Err(error) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

async fn unknown_outbox(State(state): State<AppState>) -> impl IntoResponse {
    match state.store.unknown_outbox() {
        Ok(items) => Json(json!({ "items": items })).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

async fn reconcile_outbox(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<ReconcileRequest>,
) -> impl IntoResponse {
    let outcome = request.outcome;
    let sent = match outcome.as_str() {
        "sent" => true,
        "failed" => false,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "outcome must be sent or failed" })),
            )
                .into_response()
        }
    };
    match state.store.reconcile_outbox(&id, sent) {
        Ok(true) => {
            state.publish(
                "outbox.reconciled",
                json!({ "id": id.clone(), "outcome": outcome.clone() }),
            );
            Json(json!({ "ok": true, "id": id, "outcome": outcome })).into_response()
        }
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "unknown outbox item not found" })),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

async fn events(
    State(state): State<AppState>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let mut receiver = state.events.subscribe();
    let stream = async_stream::stream! {
        yield Ok(Event::default().event("hello").data("{}"));
        loop {
            match receiver.recv().await {
                Ok(value) => yield Ok(Event::default().id(value.id.to_string()).event(value.kind.clone()).json_data(value).unwrap_or_else(|_| Event::default().event("snapshot-required").data("{}"))),
                Err(broadcast::error::RecvError::Lagged(_)) => yield Ok(Event::default().event("snapshot-required").data("{}")),
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn current_time_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}
