use std::sync::Arc;

use axum::{body::Body, http::Request};
use rinbot::{
    adapters::sqlite::SqliteStore,
    api::{router, ApiEvent, AppState, RuntimeState},
    runtime::config::Cli,
};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::sync::broadcast;
use tower::ServiceExt;

fn test_state(api_token: &str) -> (TempDir, AppState) {
    let directory = tempfile::tempdir().expect("temporary API data directory");
    let store = Arc::new(
        SqliteStore::open(directory.path().join("messages.sqlite")).expect("SQLite store"),
    );
    let (events, _) = broadcast::channel::<ApiEvent>(8);
    let config = Cli {
        host: "127.0.0.1".to_owned(),
        port: 3211,
        data_dir: directory.path().to_path_buf(),
        ui_dir: "ui/dist".into(),
        onebot_ws_url: "ws://127.0.0.1:3001/onebot/v11/ws".to_owned(),
        onebot_http_url: "http://127.0.0.1:3000".to_owned(),
        onebot_token: "onebot-secret".to_owned(),
        api_token: api_token.to_owned(),
        connect_onebot: false,
        queue_capacity: 256,
    };
    (
        directory,
        AppState {
            store,
            runtime: Arc::new(RuntimeState::new()),
            events,
            config: Arc::new(config),
        },
    )
}

async fn json_response(app: axum::Router, request: Request<Body>) -> (http::StatusCode, Value) {
    let response = app.oneshot(request).await.expect("API response");
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    let value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).expect("JSON response")
    };
    (status, value)
}

#[tokio::test]
async fn public_api_exposes_health_and_redacted_config() {
    let (_directory, state) = test_state("");
    let app = router(state);

    let (status, health) = json_response(
        app.clone(),
        Request::builder()
            .uri("/api/health")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(health["name"], "RInBot");
    assert_eq!(health["ok"], true);

    let (status, config) = json_response(
        app,
        Request::builder()
            .uri("/api/config")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(config["port"], 3211);
    assert_eq!(config["connect_onebot"], false);
    assert_eq!(config["observe_only"], true);
    assert_eq!(config["api_token_configured"], false);
    assert_eq!(config["onebot_token_configured"], true);
    assert!(!config.to_string().contains("onebot-secret"));
}

#[tokio::test]
async fn protected_api_requires_bearer_token_and_snapshot_tracks_events() {
    let (_directory, state) = test_state("api-secret");
    let app = router(state.clone());

    let (status, _) = json_response(
        app.clone(),
        Request::builder()
            .uri("/api/snapshot")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::UNAUTHORIZED);

    state.publish("test.event", json!({ "source": "api-test" }));
    let (status, snapshot) = json_response(
        app,
        Request::builder()
            .uri("/api/snapshot")
            .header("authorization", "Bearer api-secret")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(snapshot["version"], 1);
    assert_eq!(snapshot["status"]["mode"], "observe");
}
