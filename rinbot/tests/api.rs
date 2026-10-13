use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use axum::{body::Body, http::Request, middleware, routing::get, Router};
use rinbot::{
    adapters::sqlite::SqliteStore,
    api::{capture_http_incident_errors, router, ApiEvent, AppState, RuntimeState},
    domain::message::{IncomingMessage, MessageSegment},
    runtime::config::Cli,
};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::sync::broadcast;
use tower::ServiceExt;

struct FriendRequestGatewayOk;

struct PrivateWhitelistOk;

struct OwnerMessageGateway(Arc<AtomicUsize>);

struct IncidentMessageGateway(Arc<AtomicUsize>);

struct FakeStickerSyncGateway {
    calls: AtomicUsize,
    response: Vec<Value>,
}

struct FakeDailyMomentGateway {
    account_id: String,
    nickname: String,
    writes: Arc<AtomicUsize>,
}

struct ModelMustNotRunForEmptyDailyMomentSnapshot;

struct FixedDailyMomentPreviewModel;

#[async_trait]
impl rinbot::ports::language_model::LanguageModel for ModelMustNotRunForEmptyDailyMomentSnapshot {
    async fn complete(
        &self,
        _messages: &[rinbot::ports::language_model::ChatMessage],
        _temperature: f32,
        _max_tokens: u32,
    ) -> Result<rinbot::ports::language_model::Completion, rinbot::ports::language_model::ModelError>
    {
        panic!("empty manual preview must not call the model")
    }
}

#[async_trait]
impl rinbot::ports::language_model::LanguageModel for FixedDailyMomentPreviewModel {
    async fn complete(
        &self,
        _messages: &[rinbot::ports::language_model::ChatMessage],
        _temperature: f32,
        _max_tokens: u32,
    ) -> Result<rinbot::ports::language_model::Completion, rinbot::ports::language_model::ModelError>
    {
        Err(rinbot::ports::language_model::ModelError::InvalidResponse)
    }

    async fn complete_with_auto_tools(
        &self,
        _messages: &[rinbot::ports::language_model::ChatMessage],
        tools: &[rinbot::ports::language_model::FunctionTool],
        _model_override: Option<&str>,
        _temperature: f32,
        _max_tokens: u32,
    ) -> Result<rinbot::ports::language_model::Completion, rinbot::ports::language_model::ModelError>
    {
        assert!(tools.iter().any(|tool| tool.name == "submit_daily_moment"));
        Ok(rinbot::ports::language_model::Completion {
            content: String::new(),
            tool_calls: vec![rinbot::ports::language_model::FunctionToolCall {
                id: "daily-moment-submit".into(),
                name: "submit_daily_moment".into(),
                arguments: json!({
                    "decision":"publish",
                    "reason":"有具体的表达欲",
                    "content":"今天的风很舒服，想找个有树的地方慢慢走走。",
                    "imageIds":[],
                    "groupSummaries":[{"chatKey":"group:301","summary":"聊到周末出行"}],
                    "researchSources":[]
                })
                .to_string(),
            }],
            prompt_tokens: Some(12),
            completion_tokens: Some(9),
            total_tokens: Some(21),
            cached_prompt_tokens: None,
            raw_request: None,
            raw_response: None,
        })
    }
}

#[async_trait]
impl rinbot::ports::gateway::QzoneGateway for FakeDailyMomentGateway {
    async fn account_identity(
        &self,
    ) -> Result<(String, String), rinbot::ports::gateway::GatewayError> {
        Ok((self.account_id.clone(), self.nickname.clone()))
    }

    async fn qzone_read(
        &self,
        _action: rinbot::ports::gateway::QzoneReadAction,
        _params: Value,
    ) -> Result<Value, rinbot::ports::gateway::GatewayError> {
        Ok(json!({"msglist":[]}))
    }

    async fn qzone_write(
        &self,
        _action: rinbot::ports::gateway::QzoneWriteAction,
        _params: Value,
    ) -> Result<(), rinbot::ports::gateway::GatewayError> {
        Ok(())
    }

    async fn publish_daily_moment(
        &self,
        _params: Value,
    ) -> Result<String, rinbot::ports::gateway::GatewayError> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        Ok("confirmed-api-tid".into())
    }
}

#[async_trait]
impl rinbot::ports::gateway::DailyMomentImageReader for FakeDailyMomentGateway {
    async fn daily_moment_image(
        &self,
        _conversation_id: &str,
        _message_id: &str,
        _media_index: usize,
    ) -> Result<Option<String>, rinbot::ports::gateway::GatewayError> {
        Ok(None)
    }

    async fn daily_moment_upload_source(
        &self,
        _conversation_id: &str,
        _message_id: &str,
        _media_index: usize,
    ) -> Result<Option<String>, rinbot::ports::gateway::GatewayError> {
        Ok(None)
    }
}

#[async_trait]
impl rinbot::ports::gateway::StickerGateway for FakeStickerSyncGateway {
    async fn fetch_qq_favorites(
        &self,
        limit: usize,
    ) -> Result<Vec<Value>, rinbot::ports::gateway::GatewayError> {
        assert_eq!(limit, 500);
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.response.clone())
    }

    async fn add_message_image_to_favorites(
        &self,
        _conversation_id: &str,
        _message_id: &str,
        _media_index: usize,
    ) -> Result<rinbot::ports::gateway::FavoriteAddReceipt, rinbot::ports::gateway::GatewayError>
    {
        Err(rinbot::ports::gateway::GatewayError::UnsupportedAction(
            "add_custom_face".into(),
        ))
    }
}

#[async_trait]
impl rinbot::application::outbox::NotificationSender for IncidentMessageGateway {
    async fn send_owner_notification(
        &self,
        owner_uin: &str,
        _payload: serde_json::Value,
    ) -> Result<
        rinbot::ports::gateway::SendReceipt,
        rinbot::application::outbox::NotificationSendError,
    > {
        assert_eq!(owner_uin, "998877");
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(rinbot::ports::gateway::SendReceipt {
            message_id: "incident-notice-1".into(),
        })
    }
}

#[async_trait]
impl rinbot::application::outbox::NotificationSender for OwnerMessageGateway {
    async fn send_owner_notification(
        &self,
        owner_uin: &str,
        payload: serde_json::Value,
    ) -> Result<
        rinbot::ports::gateway::SendReceipt,
        rinbot::application::outbox::NotificationSendError,
    > {
        assert_eq!(owner_uin, "998877");
        assert!(payload["text"].as_str().unwrap().contains("12345"));
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(rinbot::ports::gateway::SendReceipt {
            message_id: "owner-notice-1".into(),
        })
    }
}

#[async_trait]
impl rinbot::application::identity_executor::PrivateWhitelist for PrivateWhitelistOk {
    async fn allow_private_user(&self, _user_id: &str) -> Result<(), String> {
        Ok(())
    }
}

#[async_trait]
impl rinbot::ports::gateway::FriendRequestGateway for FriendRequestGatewayOk {
    async fn decide_friend_request(
        &self,
        _request_flag: &str,
        _approve: bool,
        _remark: Option<&str>,
    ) -> Result<(), rinbot::ports::gateway::GatewayError> {
        Ok(())
    }
}

fn test_state(api_token: &str) -> (TempDir, AppState) {
    let directory = tempfile::tempdir().expect("temporary API data directory");
    let store = Arc::new(
        SqliteStore::open(directory.path().join("messages.sqlite")).expect("SQLite store"),
    );
    let (events, _) = broadcast::channel::<ApiEvent>(8);
    let incident = Arc::new(
        rinbot::incident::IncidentStore::open(
            directory.path().join("incident-pilot.sqlite"),
            rinbot::incident::IncidentSettings {
                enabled: true,
                graduated: true,
                notify_warnings: true,
                duplicate_window_minutes: 10,
                retention_days: 90,
                unknown_writes_block_chat: false,
            },
        )
        .expect("incident ledger"),
    );
    let config = Cli {
        unmigrated_enabled_features: Vec::new(),
        imported_runtime_config_loaded: false,
        feature_migration_audit_complete: false,
        enabled_feature_data_missing: Vec::new(),
        feature_data_audit_complete: false,
        model_timeout_ms: 90_000,
        identity_pilot_enabled: false,
        memory_handoff_enabled: false,
        memory_handoff_ttl_minutes: 1440,
        memory_handoff_max_chars: 4_000,
        multimodal_context_enabled: false,
        multimodal_context_graduated: false,
        identity_pilot_graduated: false,
        incoming_friend_request_enabled: false,
        incoming_friend_auto_whitelist: true,
        incoming_friend_max_pending: 50,
        group_invite_enabled: false,
        group_invite_auto_whitelist: true,
        group_invite_friends_only: true,
        group_invite_notify_owner: true,
        group_invite_owner_uin: None,
        web_search_enabled: false,
        web_search_provider: "bing".into(),
        web_search_url: "https://cn.bing.com/search".into(),
        web_search_max_results: 6,
        identity_owner_uin: None,
        imported_blocklist: Default::default(),
        imported_unified_tier: None,
        imported_group_random_percent: Default::default(),
        incident_pilot_enabled: false,
        incident_pilot_graduated: false,
        incident_owner_uin: None,
        incident_notify_warnings: true,
        incident_duplicate_window_minutes: 10,
        incident_unknown_writes_block_chat: false,
        incident_retention_days: 90,
        imported_qzone_interactions: None,
        imported_proactive: None,
        imported_vision_model: None,
        imported_vision_api_key: None,
        imported_asr_settings: None,
        imported_asr_api_key: None,
        imported_sticker_enabled: false,
        imported_sticker_collect_enabled: false,
        imported_sticker_auto_collect_enabled: false,
        imported_sticker_save_to_qq_favorites: false,
        imported_sticker_max_collect_per_hour: 10,
        imported_sticker_prompt_max_stickers: 10,
        imported_sticker_encourage: 1,
        imported_send_max_per_minute: 80,
        imported_send_max_per_hour: 500,
        imported_send_min_gap_ms: 1_000,
        imported_send_max_gap_ms: 3_000,
        imported_send_by_length_ms: 20,
        imported_send_hard_split_at: 4_000,
        host: "127.0.0.1".to_owned(),
        port: 3211,
        data_dir: directory.path().to_path_buf(),
        ui_dir: "ui/dist".into(),
        onebot_ws_url: "ws://127.0.0.1:3001/onebot/v11/ws".to_owned(),
        onebot_http_url: "http://127.0.0.1:3000".to_owned(),
        onebot_token: "onebot-secret".to_owned(),
        api_token: api_token.to_owned(),
        connect_onebot: false,
        active: false,
        model_base_url: String::new(),
        model_id: String::new(),
        model_api_key: String::new(),
        model_fallback_enabled: false,
        model_fallback_base_url: String::new(),
        model_fallback_id: String::new(),
        model_fallback_api_key: String::new(),
        model_fallback_timeout_ms: 90_000,
        time_control_json: r#"{"enabled":false,"schedule":{"mode":"deepseek-offpeak","windows":[]},"overrides":{}}"#.into(),
        system_prompt: "test".into(),
        allow_private: String::new(),
        allow_groups: String::new(),
        deny_private: String::new(),
        deny_groups: String::new(),
        allow_all_when_empty: false,
        random_response_percent: 100.0,
        trigger_keywords: String::new(),
        at_context_count: 80,
        keyword_context_count: 50,
        random_context_count: 30,
        all_context_count: 80,
        wake_delay_min_ms: 8_000,
        wake_delay_max_ms: 12_000,
        max_batch_wait_ms: 20_000,
        batch_limit: 100,
        batch_max_chars: 32_000,
        max_concurrent_runs: 2,
        model_temperature: 0.7,
        model_max_tokens: 1024,
        model_max_rounds: 12,
        queue_capacity: 256,
    };
    let asr_credentials =
        rinbot::application::model_settings::ModelCredentialStore::new_asr(directory.path());
    let incident_settings = incident.settings().expect("incident settings");
    (
        directory,
        AppState {
            store,
            identity: None,
            identity_executor: None,
            identity_notification_executor: None,
            incident: Some(incident),
            incident_settings,
            incident_error: None,
            incident_notification_executor: None,
            relationship: None,
            group_invite: None,
            group_invite_notification_executor: None,
            daily_moments: None,
            daily_moment_gateway: None,
            daily_moment_image_reader: None,
            daily_moment_model: None,
            daily_moment_persona: Default::default(),
            daily_moment_account_id: String::new(),
            daily_moment_account_nickname: String::new(),
            daily_moments_generation_ready: false,
            daily_moments_runtime_ready: false,
            qzone_interactions: None,
            qzone_runtime_ready: false,
            qzone_manual_run: None,
            relationship_runtime_ready: false,
            proactive: None,
            proactive_runtime_ready: false,
            sticker_runtime_ready: false,
            sticker_collection_runtime_ready: false,
            sticker_gateway: None,
            memory_consolidation_executor: None,
            memory_consolidation_store: None,
            memory_consolidation_runtime_ready: false,
            multimodal_context_runtime_ready: false,
            inbox_sender: None,
            runtime: Arc::new(RuntimeState::new()),
            events,
            config: Arc::new(config),
            asr_credentials,
        },
    )
}

#[test]
fn proactive_suppression_guards_track_overlapping_background_work() {
    let runtime = RuntimeState::new();
    assert_eq!(runtime.proactive_suppression_count(), 0);
    let first = runtime.suppress_proactive();
    assert_eq!(runtime.proactive_suppression_count(), 1);
    {
        let _second = runtime.suppress_proactive();
        assert_eq!(runtime.proactive_suppression_count(), 2);
    }
    assert_eq!(runtime.proactive_suppression_count(), 1);
    drop(first);
    assert_eq!(runtime.proactive_suppression_count(), 0);
}

fn insert_message(state: &AppState, id: &str) -> i64 {
    state
        .store
        .insert_incoming(&IncomingMessage {
            message_id: id.to_owned(),
            conversation_id: "private:100".to_owned(),
            sender_id: "100".to_owned(),
            sender_name: "测试用户".to_owned(),
            is_group: false,
            is_self: false,
            segments: vec![MessageSegment::Text {
                text: format!("message {id}"),
            }],
            timestamp: 1,
        })
        .unwrap()
        .unwrap()
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
async fn sticker_lifecycle_is_persisted_but_fail_closed_until_runtime_is_ready() {
    let (_directory, state) = test_state("sticker-admin");
    let app = router(state.clone());

    let (status, initial) = json_response(
        app.clone(),
        Request::get("/api/stickers/settings")
            .header("authorization", "Bearer sticker-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(initial["settings"]["enabled"], false);
    assert_eq!(initial["settings"]["graduated"], false);
    assert_eq!(initial["settings"]["promptMaxStickers"], 10);
    assert_eq!(initial["settings"]["encourage"], 1);
    assert_eq!(initial["runtimeReady"], false);

    let unauthorized = app
        .clone()
        .oneshot(
            Request::put("/api/stickers/settings/update")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"settings":{"enabled":false,"graduated":false}}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), http::StatusCode::UNAUTHORIZED);
    let disabled_settings =
        json!({"enabled":false,"graduated":false,"promptMaxStickers":10,"encourage":1});
    let (status, _) = json_response(
        app.clone(),
        Request::put("/api/stickers/settings/update")
            .header("authorization", "Bearer sticker-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"settings":disabled_settings,"confirmGraduation":false}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(
        state.store.runtime_setting("stickers").unwrap().unwrap(),
        disabled_settings
    );

    let invalid_preferences = app
        .clone()
        .oneshot(
            Request::put("/api/stickers/settings/update")
                .header("authorization", "Bearer sticker-admin")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"settings":{"enabled":false,"graduated":false,"promptMaxStickers":61,"encourage":1}}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid_preferences.status(), http::StatusCode::BAD_REQUEST);

    for (settings, confirm_graduation) in [
        (json!({"enabled":true,"graduated":false}), false),
        (json!({"enabled":false,"graduated":true}), true),
    ] {
        let (status, _) = json_response(
            app.clone(),
            Request::put("/api/stickers/settings/update")
                .header("authorization", "Bearer sticker-admin")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"settings":settings,"confirmGraduation":confirm_graduation}).to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, http::StatusCode::CONFLICT);
    }
    assert_eq!(
        state.store.runtime_setting("stickers").unwrap().unwrap(),
        disabled_settings
    );
}

#[tokio::test]
async fn sticker_collection_has_an_independent_authenticated_fail_closed_lifecycle() {
    let (_directory, state) = test_state("sticker-collect-admin");
    let app = router(state.clone());
    let (status, initial) = json_response(
        app.clone(),
        Request::get("/api/sticker-collection/settings")
            .header("authorization", "Bearer sticker-collect-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(initial["settings"]["enabled"], false);
    assert_eq!(initial["settings"]["graduated"], false);
    assert_eq!(initial["runtimeReady"], false);

    let disabled = json!({"enabled":false,"graduated":false});
    let (status, _) = json_response(
        app.clone(),
        Request::put("/api/sticker-collection/settings/update")
            .header("authorization", "Bearer sticker-collect-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"settings":disabled,"confirmGraduation":false}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(
        state
            .store
            .runtime_setting("stickerCollect")
            .unwrap()
            .unwrap(),
        disabled
    );

    for (settings, confirm_graduation) in [
        (json!({"enabled":true,"graduated":false}), false),
        (json!({"enabled":false,"graduated":true}), true),
    ] {
        let (status, _) = json_response(
            app.clone(),
            Request::put("/api/sticker-collection/settings/update")
                .header("authorization", "Bearer sticker-collect-admin")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"settings":settings,"confirmGraduation":confirm_graduation}).to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, http::StatusCode::CONFLICT);
    }
    assert_eq!(
        state
            .store
            .runtime_setting("stickerCollect")
            .unwrap()
            .unwrap(),
        disabled
    );
}

#[tokio::test]
async fn public_api_exposes_health_and_redacted_config() {
    let (_directory, state) = test_state("");
    let app = router(state.clone());

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
        app.clone(),
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
    assert_eq!(config["model_configured"], false);
    assert_eq!(config["time_control_enabled"], false);
    assert_eq!(config["api_token_configured"], false);
    assert_eq!(config["onebot_token_configured"], true);
    assert!(!config.to_string().contains("onebot-secret"));
}

#[tokio::test]
async fn relationship_archive_api_reports_lifecycle_and_empty_read_only_archive() {
    let (_directory, state) = test_state("");
    let app = router(state);
    let (status, body) = json_response(
        app.clone(),
        Request::get("/api/relationship-pilot/status")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(body["enabled"], false);
    assert_eq!(body["graduated"], false);
    assert_eq!(body["active"], false);
    assert_eq!(body["exists"], false);

    let (status, body) = json_response(
        app.clone(),
        Request::get("/api/relationships?limit=10&offset=0")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(body["items"], json!([]));
}

#[tokio::test]
async fn relationship_settings_are_authenticated_persisted_and_keep_graduation_independent() {
    let (_directory, mut state) = test_state("relationship-admin");
    let live = state.runtime.relationship_settings_handle();
    Arc::get_mut(&mut state.config).unwrap().active = true;
    let app = router(state.clone());

    let update = json!({
        "enabled": false,
        "graduated": true,
        "model": "relationship-fixture",
        "minNewMessages": 10,
        "minBatchMessages": 4,
        "minBatchAgeMinutes": 360,
        "maxEvidenceMessages": 32,
        "maxPeoplePerTurn": 2,
        "maxEvaluationsPerDay": 20,
        "familiarityRefreshMinutes": 5,
        "frictionHalfLifeHours": 48.0,
        "maxMemoryContext": 6
    });
    let unauthorized = app
        .clone()
        .oneshot(
            Request::post("/api/relationship-pilot/settings/update")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"settings":update,"confirmGraduation":false}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), http::StatusCode::UNAUTHORIZED);

    let enable_without_runtime = app
        .clone()
        .oneshot(
            Request::post("/api/relationship-pilot/settings/update")
                .header("authorization", "Bearer relationship-admin")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"settings":{
                        "enabled": true,
                        "graduated": false,
                        "minNewMessages": 8,
                        "minBatchMessages": 3,
                        "minBatchAgeMinutes": 360,
                        "maxEvidenceMessages": 32,
                        "maxPeoplePerTurn": 2,
                        "maxEvaluationsPerDay": 20,
                        "familiarityRefreshMinutes": 5,
                        "frictionHalfLifeHours": 48.0,
                        "maxMemoryContext": 6
                    },"confirmGraduation":false})
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(enable_without_runtime.status(), http::StatusCode::CONFLICT);

    let invalid = app
        .clone()
        .oneshot(
            Request::post("/api/relationship-pilot/settings/update")
                .header("authorization", "Bearer relationship-admin")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"settings":{"enabled":false,"graduated":true,"maxEvaluationsPerDay":201},"confirmGraduation":false})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), http::StatusCode::BAD_REQUEST);

    let unconfirmed_graduation = app
        .clone()
        .oneshot(
            Request::post("/api/relationship-pilot/settings/update")
                .header("authorization", "Bearer relationship-admin")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"settings":update,"confirmGraduation":false}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        unconfirmed_graduation.status(),
        http::StatusCode::BAD_REQUEST
    );

    let saved = app
        .clone()
        .oneshot(
            Request::post("/api/relationship-pilot/settings/update")
                .header("authorization", "Bearer relationship-admin")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"settings":update,"confirmGraduation":true}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(saved.status(), http::StatusCode::OK);
    assert!(!live.read().unwrap().enabled);
    assert!(live.read().unwrap().graduated);
    assert_eq!(
        state
            .store
            .runtime_setting("relationship_pilot")
            .unwrap()
            .unwrap(),
        update
    );
    let (_, fetched) = json_response(
        app.clone(),
        Request::get("/api/relationship-pilot/settings")
            .header("authorization", "Bearer relationship-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(fetched, update);

    let mut ungraduate = update.clone();
    ungraduate["graduated"] = json!(false);
    let revoked = app
        .oneshot(
            Request::post("/api/relationship-pilot/settings/update")
                .header("authorization", "Bearer relationship-admin")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"settings":ungraduate,"confirmGraduation":false}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(revoked.status(), http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn relationship_pilot_can_hot_enable_from_a_migrated_disabled_runtime() {
    let (_directory, mut state) = test_state("relationship-hot-toggle");
    Arc::get_mut(&mut state.config).unwrap().active = true;
    state.relationship_runtime_ready = true;
    let live = state.runtime.relationship_settings_handle();
    let app = router(state.clone());
    let settings = json!({
        "enabled": true,
        "graduated": false,
        "model": "relationship-fixture",
        "minNewMessages": 8,
        "minBatchMessages": 3,
        "minBatchAgeMinutes": 360,
        "maxEvidenceMessages": 32,
        "maxPeoplePerTurn": 2,
        "maxEvaluationsPerDay": 20,
        "familiarityRefreshMinutes": 5,
        "frictionHalfLifeHours": 48.0,
        "maxMemoryContext": 6
    });
    let (status, body) = json_response(
        app.clone(),
        Request::post("/api/relationship-pilot/settings/update")
            .header("authorization", "Bearer relationship-hot-toggle")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"settings":settings,"confirmGraduation":false}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(body["settings"]["enabled"], true);
    assert_eq!(live.read().unwrap().enabled, true);
    assert_eq!(
        state.store.runtime_setting("relationship_pilot").unwrap(),
        Some(settings)
    );

    let mut disabled = live.read().unwrap().clone();
    disabled.enabled = false;
    let (status, _) = json_response(
        app,
        Request::post("/api/relationship-pilot/settings/update")
            .header("authorization", "Bearer relationship-hot-toggle")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"settings":disabled,"confirmGraduation":false}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert!(!live.read().unwrap().enabled);
    assert!(!live.read().unwrap().graduated);
}

#[tokio::test]
async fn proactive_settings_are_authenticated_and_lifecycle_is_separate_from_operations() {
    let (_directory, state) = test_state("proactive-admin");
    let app = router(state.clone());

    let unauthorized = app
        .clone()
        .oneshot(
            Request::get("/api/proactive/settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), http::StatusCode::UNAUTHORIZED);

    let enable_without_scheduler = app
        .clone()
        .oneshot(
            Request::post("/api/proactive/lifecycle")
                .header("authorization", "Bearer proactive-admin")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"enabled":true,"graduated":false}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        enable_without_scheduler.status(),
        http::StatusCode::CONFLICT
    );

    let operational = json!({
        "settings": {
            "enabled": true,
            "graduated": true,
            "checkIntervalMinMs": 60000,
            "checkIntervalMaxMs": 120000,
            "idleThresholdMs": 300000,
            "probability": 0.75,
            "followUpEnabled": false,
            "selfWakeEnabled": false,
            "activeHours": [{"start":"22:00","end":"06:00"}]
        }
    });
    let saved = app
        .clone()
        .oneshot(
            Request::post("/api/proactive/settings/update")
                .header("authorization", "Bearer proactive-admin")
                .header("content-type", "application/json")
                .body(Body::from(operational.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(saved.status(), http::StatusCode::OK);
    let body = axum::body::to_bytes(saved.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["settings"]["enabled"], false);
    assert_eq!(body["settings"]["graduated"], false);
    assert_eq!(body["settings"]["probability"], 0.75);
    assert_eq!(body["settings"]["followUpEnabled"], false);
    assert_eq!(body["settings"]["activeHours"][0]["start"], "22:00");
    assert_eq!(
        state.store.runtime_setting("proactive").unwrap().unwrap()["enabled"],
        false
    );
    assert_eq!(state.runtime.proactive_settings().graduated, false);
}

#[tokio::test]
async fn memory_consolidation_lifecycle_is_authenticated_and_fail_closed() {
    let (_directory, state) = test_state("memory-consolidation-admin");
    let app = router(state.clone());
    let (status, initial) = json_response(
        app.clone(),
        Request::get("/api/memory-consolidation/status")
            .header("authorization", "Bearer memory-consolidation-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(initial["enabled"], false);
    assert_eq!(initial["graduated"], false);
    assert_eq!(initial["automaticSchedulingReady"], false);
    assert_eq!(initial["automaticSettings"]["minImpressions"], 4);
    assert_eq!(initial["automaticSettings"]["discoverMaxMembers"], 3);

    let unauthorized = app
        .clone()
        .oneshot(
            Request::post("/api/memory-consolidation/lifecycle")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"enabled":false,"graduated":false}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), http::StatusCode::UNAUTHORIZED);

    let (status, _) = json_response(
        app.clone(),
        Request::post("/api/memory-consolidation/lifecycle")
            .header("authorization", "Bearer memory-consolidation-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"enabled":true,"graduated":false}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::CONFLICT);

    let (status, _) = json_response(
        app.clone(),
        Request::post("/api/memory-consolidation/lifecycle")
            .header("authorization", "Bearer memory-consolidation-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"enabled":false,"graduated":true,"confirmGraduation":true}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::CONFLICT);

    let (status, _) = json_response(
        app.clone(),
        Request::post("/api/memory-consolidation/lifecycle")
            .header("authorization", "Bearer memory-consolidation-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"enabled":false,"graduated":false}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(
        state
            .store
            .runtime_setting("memoryConsolidate")
            .unwrap()
            .unwrap(),
        json!({"enabled":false,"graduated":false})
    );

    let (status, _) = json_response(
        app.clone(),
        Request::post("/api/memory-consolidation/run")
            .header("authorization", "Bearer memory-consolidation-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"chatKey":"group:7","userId":"123","confirm":false}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::BAD_REQUEST);

    let (status, _) = json_response(
        app.clone(),
        Request::post("/api/memory-consolidation/run")
            .header("authorization", "Bearer memory-consolidation-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"chatKey":"group:7","userId":"123","confirm":true}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::CONFLICT);
    state.runtime.set_memory_consolidation_settings(
        rinbot::application::memory_consolidation::MemoryConsolidationSettings {
            enabled: true,
            graduated: false,
            ..Default::default()
        },
    );
    let (status, _) = json_response(
        app,
        Request::post("/api/memory-consolidation/run")
            .header("authorization", "Bearer memory-consolidation-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"chatKey":"group:7","userId":"123","confirm":true}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::CONFLICT);
}

#[tokio::test]
async fn memory_consolidation_history_is_private_and_restore_appends_with_revision_guard() {
    let (directory, mut state) = test_state("memory-history-admin");
    let mut identity =
        rinbot::identity::IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
    identity
        .observe_message("seed-1", "group:7", "100", "小明", 1, 1)
        .unwrap();
    identity
        .observe_message("seed-2", "group:8", "100", "小明", 2, 2)
        .unwrap();
    let (name, previous, revision, source_watermark) = identity
        .global_consolidation_memory_with_evidence("100", "group:7")
        .unwrap()
        .unwrap();
    assert_eq!(name, "小明");
    let first = rinbot::identity::ConsolidationMemoryItem {
        content: "导入前的旧印象".into(),
        created_at: 10,
        observed_at: 10,
        origin: "consolidated".into(),
        source_chat_keys: vec!["group:7".into()],
    };
    identity
        .save_global_memory_consolidation(
            "100",
            "group:7",
            &previous,
            std::slice::from_ref(&first),
            &[],
            "test-seed",
            revision,
            source_watermark,
            10,
            5,
        )
        .unwrap();
    let (_, previous, revision, source_watermark) = identity
        .global_consolidation_memory_with_evidence("100", "group:7")
        .unwrap()
        .unwrap();
    let second = rinbot::identity::ConsolidationMemoryItem {
        content: "第二版印象".into(),
        created_at: 20,
        observed_at: 20,
        origin: "consolidated".into(),
        source_chat_keys: vec!["group:7".into()],
    };
    identity
        .save_global_memory_consolidation(
            "100",
            "group:7",
            &previous,
            &[first, second],
            &["message-2".into()],
            "test-seed",
            revision,
            source_watermark,
            20,
            5,
        )
        .unwrap();
    identity
        .import_node_memory_consolidation_backups(
            &[json!({
                "uin":"100",
                "backup_key":"100/1700000000000-node.json",
                "backed_up_at":15,
                "reason":"consolidation",
                "source_chat_key":"group:7",
                "person":{
                    "userId":"100",
                    "name":"小明",
                    "impressions":[{
                        "content":"Node 备份中的全局印象",
                        "createdAt":5,
                        "lastObservedAt":8,
                        "origin":"consolidated",
                        "sourceChatKeys":["group:7","group:8"]
                    }]
                }
            })],
            25,
        )
        .unwrap();
    state.identity = Some(Arc::new(std::sync::Mutex::new(identity)));
    let app = router(state.clone());

    let unauthorized = app
        .clone()
        .oneshot(
            Request::post("/api/memory-consolidation/history")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"chatKey":"group:7","userId":"100"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), http::StatusCode::UNAUTHORIZED);
    let unauthorized_backup_restore = app
        .clone()
        .oneshot(
            Request::post("/api/memory-consolidation/restore-node-backup")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chatKey":"group:7", "userId":"100",
                        "backupKey":"100/1700000000000-node.json",
                        "expectedCurrentRevision":2, "confirm":true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        unauthorized_backup_restore.status(),
        http::StatusCode::UNAUTHORIZED
    );

    let (status, history) = json_response(
        app.clone(),
        Request::post("/api/memory-consolidation/history")
            .header("authorization", "Bearer memory-history-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"chatKey":"group:7","userId":"100"}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(history["currentRevision"], 2);
    assert_eq!(history["revisions"][0]["revision"], 0);
    assert_eq!(history["revisions"][0]["impressions"], json!([]));
    assert_eq!(history["revisions"][1]["revision"], 1);
    assert_eq!(history["revisions"][1]["impressions"][0], "导入前的旧印象");
    assert_eq!(
        history["nodeBackups"][0]["backupKey"],
        "100/1700000000000-node.json"
    );
    assert_eq!(
        history["nodeBackups"][0]["impressions"][0]["sourceChatKeys"][1],
        "group:8"
    );

    let mut settings = state.runtime.memory_consolidation_settings();
    settings.enabled = true;
    state
        .runtime
        .set_memory_consolidation_settings(settings.clone());
    let app = router(state.clone());
    let (status, _) = json_response(
        app.clone(),
        Request::post("/api/memory-consolidation/restore")
            .header("authorization", "Bearer memory-history-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "chatKey":"group:7",
                    "userId":"100",
                    "targetRevision":1,
                    "expectedCurrentRevision":2,
                    "confirm":true
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::CONFLICT);
    settings.enabled = false;
    state
        .runtime
        .set_memory_consolidation_settings(settings.clone());

    let (status, restored) = json_response(
        router(state.clone()),
        Request::post("/api/memory-consolidation/restore")
            .header("authorization", "Bearer memory-history-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "chatKey":"group:7",
                    "userId":"100",
                    "targetRevision":1,
                    "expectedCurrentRevision":2,
                    "confirm":true
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(restored["revision"], 3);
    assert_eq!(
        state
            .identity
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .model_memory_query("group:7", Some("100"))
            .unwrap()
            .into_iter()
            .find(|entry| entry.content == "导入前的旧印象")
            .unwrap()
            .content,
        "导入前的旧印象"
    );

    let (status, _) = json_response(
        router(state.clone()),
        Request::post("/api/memory-consolidation/restore")
            .header("authorization", "Bearer memory-history-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "chatKey":"group:7",
                    "userId":"100",
                    "targetRevision":1,
                    "expectedCurrentRevision":2,
                    "confirm":true
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::CONFLICT);

    let (status, latest) = json_response(
        router(state.clone()),
        Request::post("/api/memory-consolidation/history")
            .header("authorization", "Bearer memory-history-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"chatKey":"group:8","userId":"100"}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(latest["currentRevision"], 3);
    let backup_key = latest["nodeBackups"][0]["backupKey"].as_str().unwrap();
    let backup_restore_request = || {
        Request::post("/api/memory-consolidation/restore-node-backup")
            .header("authorization", "Bearer memory-history-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "chatKey":"group:8", "userId":"100", "backupKey":backup_key,
                    "expectedCurrentRevision":3, "confirm":true
                })
                .to_string(),
            ))
            .unwrap()
    };
    settings.enabled = true;
    state
        .runtime
        .set_memory_consolidation_settings(settings.clone());
    let (status, _) = json_response(router(state.clone()), backup_restore_request()).await;
    assert_eq!(status, http::StatusCode::CONFLICT);
    settings.enabled = false;
    state.runtime.set_memory_consolidation_settings(settings);
    let (status, restored_backup) =
        json_response(router(state.clone()), backup_restore_request()).await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(restored_backup["revision"], 4);
    assert!(state
        .identity
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .model_memory_query("group:8", Some("100"))
        .unwrap()
        .iter()
        .any(|item| item.content == "Node 备份中的全局印象"));
    let (status, _) = json_response(router(state.clone()), backup_restore_request()).await;
    assert_eq!(status, http::StatusCode::CONFLICT);
}

#[tokio::test]
async fn model_memory_change_history_and_restore_require_admin_and_are_audited() {
    let (directory, mut state) = test_state("model-memory-audit-admin");
    let mut identity =
        rinbot::identity::IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
    identity
        .observe_message("g7-u1", "group:7", "100", "小明", 1, 1)
        .unwrap();
    identity
        .model_memory_append("100", "group:7", "小明", "已删除的印象", "append-audit", 2)
        .unwrap();
    identity
        .model_memory_remove(
            "group:7",
            Some("100"),
            None,
            Some("已删除的印象"),
            "remove-audit",
            3,
        )
        .unwrap();
    state.identity = Some(Arc::new(std::sync::Mutex::new(identity)));

    let (status, _) = json_response(
        router(state.clone()),
        Request::post("/api/model-memory/history")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"chatKey":"group:7","userId":"100"}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::UNAUTHORIZED);

    let (status, history) = json_response(
        router(state.clone()),
        Request::post("/api/model-memory/history")
            .header("authorization", "Bearer model-memory-audit-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"chatKey":"group:7","userId":"100"}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(history["mutations"][0]["operationId"], "remove-audit");
    assert_eq!(
        history["mutations"][0]["before"]["affected"][0]["entries"][0]["content"],
        "已删除的印象"
    );

    let (status, _) = json_response(
        router(state.clone()),
        Request::post("/api/model-memory/restore")
            .header("authorization", "Bearer model-memory-audit-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"removalOperationId":"remove-audit","restoreOperationId":"restore-audit","confirm":false}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::BAD_REQUEST);

    let (status, restored) = json_response(
        router(state.clone()),
        Request::post("/api/model-memory/restore")
            .header("authorization", "Bearer model-memory-audit-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"removalOperationId":"remove-audit","restoreOperationId":"restore-audit","confirm":true}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(restored["restoredCount"], 1);
    let mutation_history = state
        .identity
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .model_memory_mutation_history("group:7", Some("100"), 20)
        .unwrap();
    assert!(mutation_history
        .iter()
        .any(|item| item.operation_id == "remove-audit" && item.restored));
    assert!(mutation_history
        .iter()
        .any(|item| item.operation_id == "restore-audit" && item.action == "restore"));
}

#[tokio::test]
async fn multimodal_context_lifecycle_is_authenticated_and_fail_closed() {
    let (_directory, mut state) = test_state("multimodal-admin");
    state.runtime.set_multimodal_context_settings(
        rinbot::application::multimodal_context::MultimodalContextSettings {
            enabled: true,
            graduated: false,
        },
    );
    let app = router(state.clone());
    let (status, initial) = json_response(
        app.clone(),
        Request::get("/api/multimodal-context/status")
            .header("authorization", "Bearer multimodal-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(initial["enabled"], true);
    assert_eq!(initial["active"], false);
    assert_eq!(initial["runtimeReady"], false);
    assert_eq!(initial["stickerImageReadAvailable"], false);

    let mut config = (*state.config).clone();
    config.active = true;
    state.config = Arc::new(config);
    state.sticker_runtime_ready = true;
    let mut model = state.runtime.model_settings();
    model.vision_model = Some(rinbot::ports::language_model::VisionModelRuntimeConfig {
        enabled: true,
        base_url: "https://vision.example.invalid".into(),
        api_key: "private-test-key".into(),
        model: "vision-test-model".into(),
        timeout: std::time::Duration::from_secs(30),
    });
    state.runtime.set_model_settings(model);
    let app = router(state.clone());
    let (_, ready_status) = json_response(
        app.clone(),
        Request::get("/api/multimodal-context/status")
            .header("authorization", "Bearer multimodal-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(ready_status["stickerImageReadAvailable"], true);

    let unauthorized = app
        .clone()
        .oneshot(
            Request::post("/api/multimodal-context/lifecycle")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"enabled":false,"graduated":false}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), http::StatusCode::UNAUTHORIZED);

    let (status, _) = json_response(
        app.clone(),
        Request::post("/api/multimodal-context/lifecycle")
            .header("authorization", "Bearer multimodal-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"enabled":true,"graduated":false}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::CONFLICT);

    let (status, _) = json_response(
        app.clone(),
        Request::post("/api/multimodal-context/lifecycle")
            .header("authorization", "Bearer multimodal-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"enabled":false,"graduated":false}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(
        state
            .store
            .runtime_setting("multimodalContext")
            .unwrap()
            .unwrap(),
        json!({"enabled":false,"graduated":false})
    );
}

#[tokio::test]
async fn proactive_task_page_and_cancel_require_auth_and_explicit_confirmation() {
    let (directory, mut state) = test_state("proactive-admin");
    let mut ledger = rinbot::application::proactive::ProactiveStore::open(
        directory.path().join("proactive.sqlite"),
        true,
    )
    .unwrap()
    .unwrap();
    let task = ledger
        .schedule_wake("group:321", "selfWake", 60_000, "继续话题", false)
        .unwrap();
    state.proactive = Some(Arc::new(std::sync::Mutex::new(ledger)));
    let app = router(state);

    let unauthorized = app
        .clone()
        .oneshot(
            Request::get("/api/proactive/wakes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), http::StatusCode::UNAUTHORIZED);

    let listed = app
        .clone()
        .oneshot(
            Request::get("/api/proactive/wakes")
                .header("authorization", "Bearer proactive-admin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(listed.status(), http::StatusCode::OK);
    let body = axum::body::to_bytes(listed.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["items"][0]["id"], task.id);

    let not_confirmed = app
        .clone()
        .oneshot(
            Request::post(format!("/api/proactive/wakes/{}/cancel", task.id))
                .header("authorization", "Bearer proactive-admin")
                .header("content-type", "application/json")
                .body(Body::from(json!({"confirm":false}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(not_confirmed.status(), http::StatusCode::BAD_REQUEST);

    let cancelled = app
        .oneshot(
            Request::post(format!("/api/proactive/wakes/{}/cancel", task.id))
                .header("authorization", "Bearer proactive-admin")
                .header("content-type", "application/json")
                .body(Body::from(json!({"confirm":true}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cancelled.status(), http::StatusCode::OK);
}

#[tokio::test]
async fn qzone_interaction_settings_are_authenticated_persisted_and_fail_closed_until_runtime_ready(
) {
    let (_directory, state) = test_state("qzone-settings-token");
    let app = router(state.clone());
    let settings = rinbot::application::qzone_interactions::QzoneInteractionsSettings::default();
    let payload = json!({"settings":settings});

    let (status, _) = json_response(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri("/api/qzone-interactions/settings/update")
            .header("content-type", "application/json")
            .body(Body::from(payload.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::UNAUTHORIZED);

    let invalid = rinbot::application::qzone_interactions::QzoneInteractionsSettings {
        feed_interval_minutes: 2,
        ..Default::default()
    };
    let (status, _) = json_response(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri("/api/qzone-interactions/settings/update")
            .header("content-type", "application/json")
            .header("authorization", "Bearer qzone-settings-token")
            .body(Body::from(json!({"settings":invalid}).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::BAD_REQUEST);

    let mut settings =
        rinbot::application::qzone_interactions::QzoneInteractionsSettings::default();
    settings.feed_interval_minutes = 45;
    let (status, response) = json_response(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri("/api/qzone-interactions/settings/update")
            .header("content-type", "application/json")
            .header("authorization", "Bearer qzone-settings-token")
            .body(Body::from(json!({"settings":settings}).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(response["settings"]["feedIntervalMinutes"], 45);
    assert_eq!(
        state
            .runtime
            .qzone_interactions_settings()
            .feed_interval_minutes,
        45
    );

    let mut enable = state.runtime.qzone_interactions_settings();
    enable.enabled = true;
    let (status, _) = json_response(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/qzone-interactions/settings/update")
            .header("content-type", "application/json")
            .header("authorization", "Bearer qzone-settings-token")
            .body(Body::from(json!({"settings":enable}).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::CONFLICT);
    assert!(!state.runtime.qzone_interactions_settings().enabled);
}

#[tokio::test]
async fn qzone_audit_endpoint_requires_admin_authentication() {
    let (_directory, state) = test_state("qzone-audit-token");
    let app = router(state);
    let (status, _) = json_response(
        app.clone(),
        Request::get("/api/qzone-interactions/audit?limit=20")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::UNAUTHORIZED);

    let (status, _) = json_response(
        app,
        Request::get("/api/qzone-interactions/audit?limit=20")
            .header("authorization", "Bearer qzone-audit-token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn relationship_detail_api_reads_archived_person_events_flags_and_cursors() {
    let (directory, mut state) = test_state("");
    let path = directory.path().join("relationship-pilot.sqlite");
    let mut ledger = rinbot::relationship::RelationshipStore::open(&path).unwrap();
    ledger
        .import_legacy_records(
            "fixture",
            &[json!({
                "uin":"12345","familiarity":0.7,"affinity":0.2,"friction":0.1,
                "friction_updated_at":20,"message_count":42,"active_days":7,
                "direct_interactions":3,"chat_count":2,"last_interaction_at":18,
                "last_evaluated_at":21,"evaluator_version":"ev1","reducer_version":"r1",
                "policy_version":"p1","created_at":1,"updated_at":22
            })],
            &[json!({
                "id":"event-a","uin":"12345","event_type":"boundary_cross","polarity":-1,
                "strength":0.8,"confidence":0.9,"summary":"需要保持距离","evidence_ids":"[\"m1\"]",
                "source_chat_keys":"[\"group:99\"]","dedupe_key":"d1","evaluator_version":"ev1",
                "delta_affinity":-0.1,"delta_friction":0.2,"created_at":20,"applied_at":21
            })],
            &[json!({"id":"flag-a","uin":"12345","type":"boundary_violation","status":"open",
                "source_event_id":"event-a","resolved_by_event_id":"","created_at":21,"resolved_at":0})],
            &[json!({"uin":"12345","chat_key":"group:99","last_message_id":17})],
            &[],
            30,
        )
        .unwrap();
    let evaluation_id = ledger
        .begin_evaluation("12345", 4, &["group:99".into()], "test-model", 23)
        .unwrap();
    ledger
        .set_evaluation_request(
            &evaluation_id,
            &json!({"messages":[{"role":"user","content":"private prompt sentinel"}]}),
        )
        .unwrap();
    ledger
        .finish_evaluation_with_trace(
            &evaluation_id,
            "failed",
            &json!({"promptTokens":12,"completionTokens":3,"totalTokens":15,"cachedTokens":4,"providerSecret":"must-not-leak"}),
            "provider response contains private data",
            &json!({"content":"private response sentinel","toolCalls":[]}),
            24,
        )
        .unwrap();
    drop(ledger);
    state.relationship = Some(Arc::new(std::sync::Mutex::new(
        rinbot::relationship::RelationshipStore::open_existing_read_only(&path).unwrap(),
    )));
    let (status, body) = json_response(
        router(state),
        Request::get("/api/relationships/12345")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(body["person"]["messageCount"], 42);
    assert_eq!(body["events"][0]["summary"], "需要保持距离");
    assert_eq!(body["flags"][0]["status"], "open");
    assert_eq!(body["cursors"]["group:99"], 17);
    assert_eq!(body["evaluationCount"], 1);
    assert_eq!(body["evaluations"][0]["status"], "failed");
    assert_eq!(body["evaluations"][0]["usage"]["totalTokens"], 15);
    assert_eq!(body["evaluations"][0]["usage"]["cachedTokens"], 4);
    assert_eq!(body["evaluations"][0]["sourceChatKeys"][0], "group:99");
    assert!(!body.to_string().contains("private prompt sentinel"));
    assert!(!body.to_string().contains("private response sentinel"));
    assert!(!body.to_string().contains("providerSecret"));
    assert_eq!(body["evaluations"][0]["hasError"], true);
    assert!(body["evaluations"][0].get("error").is_none());
    assert!(body["evaluations"][0]["usage"]
        .get("providerSecret")
        .is_none());
}

#[tokio::test]
async fn relationship_evaluation_trace_requires_admin_auth_and_is_person_scoped() {
    let (directory, mut state) = test_state("trace-test-token");
    let path = directory.path().join("relationship-pilot.sqlite");
    let mut ledger = rinbot::relationship::RelationshipStore::open(&path).unwrap();
    ledger
        .import_legacy_records("trace-fixture", &[], &[], &[], &[], &[], 10)
        .unwrap();
    let id = ledger
        .begin_evaluation("12345", 1, &["private:12345".into()], "model-x", 11)
        .unwrap();
    ledger
        .set_evaluation_request(&id, &json!({"messages":["private prompt"]}))
        .unwrap();
    let session = json!({"id":"session-private","kind":"relationship-review","status":"done"});
    ledger.set_evaluation_session(&id, &session).unwrap();
    ledger
        .finish_evaluation_with_session_trace(
            &id,
            "done",
            &json!({"promptTokens":1}),
            "",
            &json!({"rawProviderResponse":{"choices":[{"message":{"content":"private answer"}}]}}),
            &session,
            12,
        )
        .unwrap();
    drop(ledger);
    state.relationship = Some(Arc::new(std::sync::Mutex::new(
        rinbot::relationship::RelationshipStore::open_existing_read_only(&path).unwrap(),
    )));
    let uri = format!("/api/relationships/12345/evaluations/{id}/trace");
    let (status, _) = json_response(
        router(state.clone()),
        Request::get(&uri).body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::UNAUTHORIZED);

    let (status, body) = json_response(
        router(state.clone()),
        Request::get(&uri)
            .header("authorization", "Bearer trace-test-token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(body["request"]["messages"][0], "private prompt");
    assert_eq!(body["session"]["kind"], "relationship-review");
    assert_eq!(body["session"]["status"], "done");
    assert_eq!(
        body["response"]["rawProviderResponse"]["choices"][0]["message"]["content"],
        "private answer"
    );

    let (status, _) = json_response(
        router(state),
        Request::get(format!("/api/relationships/54321/evaluations/{id}/trace"))
            .header("authorization", "Bearer trace-test-token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn status_reports_the_runtime_mode_without_exposing_model_secrets() {
    let (_directory, mut state) = test_state("");
    let mut config = (*state.config).clone();
    config.active = true;
    config.model_base_url = "https://model.example.invalid".into();
    config.model_id = "test-model".into();
    config.model_api_key = "must-not-leak".into();
    state.config = Arc::new(config);
    let mut model = state.runtime.model_settings();
    model.base_url = "https://model.example.invalid".into();
    model.model = "test-model".into();
    model.api_key = "must-not-leak".into();
    state.runtime.set_model_settings(model);
    let app = router(state);
    let (_, status) = json_response(
        app.clone(),
        Request::builder()
            .uri("/api/status")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let (_, config) = json_response(
        app,
        Request::builder()
            .uri("/api/config")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status["mode"], "active");
    assert_eq!(config["observe_only"], false);
    assert_eq!(config["model_configured"], true);
    assert!(!config.to_string().contains("must-not-leak"));
    assert!(!config.to_string().contains("model.example.invalid"));
}

#[tokio::test]
async fn identity_archive_reads_only_imported_people_and_friend_requests_with_auth() {
    let (_directory, state) = test_state("identity-admin-token");
    let source_dir = tempfile::tempdir().unwrap();
    let source_path = source_dir.path().join("identity.sqlite");
    let source = rusqlite::Connection::open(&source_path).unwrap();
    source.execute_batch(
        "CREATE TABLE people(uin TEXT PRIMARY KEY, primary_name TEXT, is_friend INTEGER, message_count INTEGER);
         CREATE TABLE identity_sources(uin TEXT, chat_key TEXT);
         CREATE TABLE identity_aliases(uin TEXT, chat_key TEXT, alias TEXT);
         CREATE TABLE incoming_friend_requests(id TEXT PRIMARY KEY, uin TEXT, status TEXT, comment TEXT, request_flag TEXT);
         INSERT INTO people VALUES('100','fixture person',1,9);
         INSERT INTO identity_sources VALUES('100','private:100');
         INSERT INTO identity_aliases VALUES('100','private:100','fixture alias');
         INSERT INTO incoming_friend_requests VALUES('fr_fixture','200','pending','hello','archived-private-flag');",
    )
    .unwrap();
    drop(source);
    state
        .store
        .import_legacy_domain_database(
            &source_path,
            "identity",
            &[
                "people",
                "identity_sources",
                "identity_aliases",
                "incoming_friend_requests",
            ],
        )
        .unwrap();

    let app = router(state);
    let (status, _) = json_response(
        app.clone(),
        Request::builder()
            .uri("/api/identity/people")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::UNAUTHORIZED);

    let (status, people) = json_response(
        app.clone(),
        Request::builder()
            .uri("/api/identity/people?limit=5&offset=0")
            .header("authorization", "Bearer identity-admin-token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(people["source"], "imported_legacy_archive");
    assert_eq!(people["read_only"], true);
    assert_eq!(people["total"], 1);
    assert_eq!(people["items"][0]["uin"], "100");

    let (status, requests) = json_response(
        app,
        Request::builder()
            .uri("/api/identity/incoming-friend-requests")
            .header("authorization", "Bearer identity-admin-token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(requests["items"][0]["status"], "pending");
    assert!(requests["items"][0].get("request_flag").is_none());
    assert!(!requests.to_string().contains("archived-private-flag"));
}

#[tokio::test]
async fn identity_people_api_merges_live_index_without_erasing_archived_friend_state() {
    let (directory, mut state) = test_state("identity-merge-token");
    Arc::make_mut(&mut state.config).identity_pilot_enabled = true;
    let source_dir = tempfile::tempdir().unwrap();
    let source_path = source_dir.path().join("identity.sqlite");
    let source = rusqlite::Connection::open(&source_path).unwrap();
    source.execute_batch(
        "CREATE TABLE people(uin TEXT PRIMARY KEY, primary_name TEXT, is_friend INTEGER, message_count INTEGER, profile_json TEXT);
         CREATE TABLE identity_sources(uin TEXT, chat_key TEXT);
         CREATE TABLE identity_aliases(uin TEXT, chat_key TEXT, alias TEXT);
         CREATE TABLE incoming_friend_requests(id TEXT PRIMARY KEY, uin TEXT, status TEXT, comment TEXT, request_flag TEXT);
         INSERT INTO people VALUES('100','archived name',1,9,'{\"note\":\"keep\"}');
         INSERT INTO identity_sources VALUES('100','private:100');
         INSERT INTO identity_aliases VALUES('100','private:100','old alias');",
    )
    .unwrap();
    drop(source);
    state
        .store
        .import_legacy_domain_database(
            &source_path,
            "identity",
            &[
                "people",
                "identity_sources",
                "identity_aliases",
                "incoming_friend_requests",
            ],
        )
        .unwrap();

    let mut identity =
        rinbot::identity::IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
    identity
        .observe_message("new-1", "private:100", "100", "new name", 20, 20)
        .unwrap();
    identity
        .observe_message("new-2", "private:200", "200", "new person", 30, 30)
        .unwrap();
    let identity = Arc::new(std::sync::Mutex::new(identity));
    state.identity = Some(identity.clone());
    state.identity_executor = Some(Arc::new(
        rinbot::application::identity_executor::IdentityActionExecutor::new(
            identity,
            Arc::new(FriendRequestGatewayOk),
            false,
            false,
            None,
        ),
    ));

    let (status, people) = json_response(
        router(state),
        Request::builder()
            .uri("/api/identity/people?limit=10&offset=0")
            .header("authorization", "Bearer identity-merge-token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(people["source"], "rust_identity_module");
    assert_eq!(people["total"], 2);
    let person = people["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|person| person["uin"] == "100")
        .unwrap();
    assert_eq!(person["primary_name"], "new name");
    assert_eq!(person["is_friend"], 1);
    assert_eq!(person["profile_json"], "{\"note\":\"keep\"}");
}

#[tokio::test]
async fn identity_profile_edits_require_auth_and_explicit_confirmation() {
    let (directory, mut state) = test_state("identity-profile-token");
    Arc::make_mut(&mut state.config).identity_pilot_enabled = true;
    Arc::make_mut(&mut state.config).active = true;
    let mut identity =
        rinbot::identity::IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
    identity
        .observe_message("m1", "private:123", "123", "原始昵称", 10, 10)
        .unwrap();
    state.identity = Some(Arc::new(std::sync::Mutex::new(identity)));
    let app = router(state);
    let body = json!({
        "confirm":true,
        "primaryName":"手动名称",
        "isFriend":true,
        "profile":{"标签":"常用联系人"}
    });
    let (status, _) = json_response(
        app.clone(),
        Request::put("/api/identity/people/123/profile")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::UNAUTHORIZED);

    let (status, _) = json_response(
        app.clone(),
        Request::put("/api/identity/people/123/profile")
            .header("authorization", "Bearer identity-profile-token")
            .header("content-type", "application/json")
            .body(Body::from(json!({"confirm":false}).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::BAD_REQUEST);

    let (status, result) = json_response(
        app.clone(),
        Request::put("/api/identity/people/123/profile")
            .header("authorization", "Bearer identity-profile-token")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(result["ok"], true);
    let (status, listed) = json_response(
        app,
        Request::get("/api/identity/people?limit=10&offset=0")
            .header("authorization", "Bearer identity-profile-token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(listed["items"][0]["primary_name"], "手动名称");
    assert_eq!(listed["items"][0]["is_friend"], 1);
    assert_eq!(
        listed["items"][0]["profile_json"],
        "{\"标签\":\"常用联系人\"}"
    );
}

#[tokio::test]
async fn identity_person_merge_requires_admin_auth_and_confirmation() {
    let (directory, mut state) = test_state("identity-merge-admin-token");
    Arc::make_mut(&mut state.config).identity_pilot_enabled = true;
    Arc::make_mut(&mut state.config).active = true;
    let legacy_dir = tempfile::tempdir().unwrap();
    let legacy_path = legacy_dir.path().join("identity.sqlite");
    let legacy = rusqlite::Connection::open(&legacy_path).unwrap();
    legacy.execute_batch(
        "CREATE TABLE people(uin TEXT PRIMARY KEY,primary_name TEXT,is_friend INTEGER,message_count INTEGER);
         INSERT INTO people VALUES('200','旧档案来源',0,7);",
    ).unwrap();
    drop(legacy);
    state
        .store
        .import_legacy_domain_database(&legacy_path, "identity", &["people"])
        .unwrap();
    let mut identity =
        rinbot::identity::IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
    identity
        .observe_message("target", "private:100", "100", "目标", 10, 10)
        .unwrap();
    identity
        .observe_message("source", "private:200", "200", "来源", 20, 20)
        .unwrap();
    state.identity = Some(Arc::new(std::sync::Mutex::new(identity)));
    let app = router(state);
    let request = json!({"targetUin":"100","confirm":true});
    let (status, _) = json_response(
        app.clone(),
        Request::post("/api/identity/people/200/merge")
            .header("content-type", "application/json")
            .body(Body::from(request.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::UNAUTHORIZED);
    let (status, _) = json_response(
        app.clone(),
        Request::post("/api/identity/people/200/merge")
            .header("authorization", "Bearer identity-merge-admin-token")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"targetUin":"100","confirm":false}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::BAD_REQUEST);
    let (status, result) = json_response(
        app.clone(),
        Request::post("/api/identity/people/200/merge")
            .header("authorization", "Bearer identity-merge-admin-token")
            .header("content-type", "application/json")
            .body(Body::from(request.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(result["targetUin"], "100");
    let (status, people) = json_response(
        app,
        Request::get("/api/identity/people?limit=10&offset=0")
            .header("authorization", "Bearer identity-merge-admin-token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(people["total"], 1);
    assert_eq!(people["items"][0]["uin"], "100");
    assert_eq!(people["items"][0]["message_count"], 2);
}

#[tokio::test]
async fn live_identity_request_api_is_authenticated_and_never_returns_onebot_flag() {
    let (directory, mut state) = test_state("identity-live-token");
    let mut identity =
        rinbot::identity::IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
    identity
        .record_request("12345", "private-onebot-capability", "hello", 10, 100)
        .unwrap();
    state.identity = Some(Arc::new(std::sync::Mutex::new(identity)));
    let app = router(state);

    let (status, _) = json_response(
        app.clone(),
        Request::builder()
            .uri("/api/identity/incoming-friend-requests")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::UNAUTHORIZED);

    let (status, response) = json_response(
        app.clone(),
        Request::builder()
            .uri("/api/identity/incoming-friend-requests?status=pending")
            .header("authorization", "Bearer identity-live-token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(response["source"], "rust_identity_module");
    assert_eq!(response["items"][0]["userId"], "12345");
    assert_eq!(response["items"][0]["status"], "pending");
    assert!(!response.to_string().contains("private-onebot-capability"));
}

#[tokio::test]
async fn friend_request_decision_route_requires_admin_token_and_returns_durable_state() {
    let (directory, mut state) = test_state("identity-write-token");
    let mut identity =
        rinbot::identity::IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
    let (request, _) = identity
        .record_request("12345", "private-capability", "", 10, 100)
        .unwrap();
    let identity = Arc::new(std::sync::Mutex::new(identity));
    state.identity = Some(identity.clone());
    state.identity_executor = Some(Arc::new(
        rinbot::application::identity_executor::IdentityActionExecutor::new(
            identity,
            Arc::new(FriendRequestGatewayOk),
            true,
            false,
            None,
        ),
    ));
    let app = router(state);
    let uri = format!(
        "/api/identity/incoming-friend-requests/{}/decision",
        request.id
    );

    let (status, _) = json_response(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri(&uri)
            .header("content-type", "application/json")
            .body(Body::from(r#"{"decision":"approve","remark":"hello"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::UNAUTHORIZED);

    let (status, response) = json_response(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("authorization", "Bearer identity-write-token")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"decision":"approve","remark":"hello"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(response["execution"], "approved");
    assert_eq!(response["request"]["status"], "approved");
    assert!(!response.to_string().contains("private-capability"));
}

#[tokio::test]
async fn private_whitelist_sync_persists_and_applies_live_idempotently() {
    let (_directory, state) = test_state("whitelist-token");
    state
        .runtime
        .allow_private_user(&state.store, "12345")
        .await
        .unwrap();
    state
        .runtime
        .allow_private_user(&state.store, "12345")
        .await
        .unwrap();
    assert_eq!(state.runtime.behavior().access.allow_private, ["12345"]);
    let persisted = state.store.runtime_setting("behavior").unwrap().unwrap();
    assert_eq!(persisted["access"]["allow_private"], json!(["12345"]));
    assert!(state
        .runtime
        .allow_private_user(&state.store, "not-a-qq")
        .await
        .is_err());
}

#[tokio::test]
async fn approved_group_policy_matches_node_whitelist_and_default_probability() {
    let (_directory, state) = test_state("group-whitelist-token");
    let mut behavior = state.runtime.behavior();
    behavior.access.deny_groups = vec!["98765".into()];
    state.runtime.set_behavior(behavior);
    state
        .runtime
        .allow_group_after_invite(&state.store, "98765")
        .await
        .unwrap();
    let behavior = state.runtime.behavior();
    assert_eq!(behavior.access.allow_groups, ["98765"]);
    assert!(behavior.access.deny_groups.is_empty());
    assert!(!behavior.triggers.unified_tier);
    assert_eq!(behavior.triggers.group_random_percent["98765"], 15.0);
    let persisted = state.store.runtime_setting("behavior").unwrap().unwrap();
    assert_eq!(persisted["access"]["allow_groups"], json!(["98765"]));
    assert_eq!(persisted["triggers"]["group_random_percent"]["98765"], 15.0);
    state
        .runtime
        .allow_group_after_invite(&state.store, "98765")
        .await
        .unwrap();
    assert_eq!(state.runtime.behavior().access.allow_groups, ["98765"]);
    assert!(state
        .runtime
        .allow_group_after_invite(&state.store, "not-a-qq")
        .await
        .is_err());
}

#[tokio::test]
async fn already_allowed_group_does_not_change_node_global_probability_policy() {
    let (_directory, state) = test_state("group-whitelist-existing-token");
    let mut behavior = state.runtime.behavior();
    behavior.access.allow_groups = vec!["98765".into()];
    behavior.access.deny_groups = vec!["98765".into()];
    behavior.triggers.unified_tier = true;
    behavior.triggers.group_random_percent.clear();
    state.runtime.set_behavior(behavior);
    let before = state.store.runtime_setting("behavior").unwrap();

    state
        .runtime
        .allow_group_after_invite(&state.store, "98765")
        .await
        .unwrap();

    let after_behavior = state.runtime.behavior();
    assert_eq!(after_behavior.access.allow_groups, ["98765"]);
    assert_eq!(after_behavior.access.deny_groups, ["98765"]);
    assert!(after_behavior.triggers.unified_tier);
    assert!(!after_behavior
        .triggers
        .group_random_percent
        .contains_key("98765"));
    assert_eq!(state.store.runtime_setting("behavior").unwrap(), before);
}

#[tokio::test]
async fn group_invite_notification_retry_requires_auth_confirmation_and_definite_failure() {
    let (directory, mut state) = test_state("group-invite-notify-token");
    let mut config = (*state.config).clone();
    config.group_invite_owner_uin = Some("998877".into());
    config.group_invite_enabled = true;
    state.config = Arc::new(config);
    let mut ledger =
        rinbot::group_invite::GroupInviteStore::open(directory.path().join("group-invite.sqlite"))
            .unwrap();
    let (_, _, request_id) = ledger
        .observe_request(
            "12345",
            "67890",
            "invite",
            "check access",
            "邀请人不在私聊白名单（不是已确认好友）",
            rinbot::domain::event::FriendRequestFlag::new("one-time-group-flag".into()),
            false,
            true,
            100,
        )
        .unwrap();
    ledger
        .begin_notification(&request_id, "failed-attempt", "998877", 110)
        .unwrap();
    ledger
        .complete_notification(
            &request_id,
            "failed-attempt",
            "failed",
            "",
            "explicit failure",
            120,
        )
        .unwrap();
    let (direct_id, inserted) = ledger
        .record_direct_entry("12345", "2676659414", "998877", 500, true, false, true, 130)
        .unwrap();
    assert!(inserted);
    ledger
        .begin_direct_notification(&direct_id, "direct-failed-attempt", "998877", 140)
        .unwrap();
    ledger
        .complete_direct_notification(
            &direct_id,
            "direct-failed-attempt",
            "failed",
            "",
            "explicit failure",
            150,
        )
        .unwrap();
    let ledger = Arc::new(std::sync::Mutex::new(ledger));
    let sends = Arc::new(AtomicUsize::new(0));
    state.group_invite = Some(ledger.clone());
    state.group_invite_notification_executor = Some(Arc::new(
        rinbot::application::group_invite::GroupInviteNotificationExecutor::new(
            ledger,
            Arc::new(OwnerMessageGateway(sends.clone())),
            true,
        ),
    ));
    let uri = format!("/api/group-invites/{request_id}/notification/retry");
    let app = router(state);
    let (list_status, list) = json_response(
        app.clone(),
        Request::builder()
            .uri("/api/group-invites?limit=20&offset=0")
            .header("authorization", "Bearer group-invite-notify-token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(list_status, http::StatusCode::OK);
    assert_eq!(list["items"][0]["notifyState"], "failed");
    assert_eq!(list["directEntries"][0]["notifyState"], "failed");
    assert!(!list.to_string().contains("one-time-group-flag"));
    let (unauthorized, _) = json_response(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri(&uri)
            .header("content-type", "application/json")
            .body(Body::from(r#"{"confirm":true}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(unauthorized, http::StatusCode::UNAUTHORIZED);
    let (unconfirmed, _) = json_response(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri(&uri)
            .header("authorization", "Bearer group-invite-notify-token")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"confirm":false}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(unconfirmed, http::StatusCode::CONFLICT);
    let (status, response) = json_response(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri(&uri)
            .header("authorization", "Bearer group-invite-notify-token")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"confirm":true}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(response["request"]["notifyState"], "sent");
    assert_eq!(sends.load(Ordering::SeqCst), 1);
    assert!(!response.to_string().contains("one-time-group-flag"));
    let direct_uri = format!("/api/group-invites/direct/{direct_id}/notification/retry");
    let (direct_unauthorized, _) = json_response(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri(&direct_uri)
            .header("content-type", "application/json")
            .body(Body::from(r#"{"confirm":true}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(direct_unauthorized, http::StatusCode::UNAUTHORIZED);
    let (direct_unconfirmed, _) = json_response(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri(&direct_uri)
            .header("authorization", "Bearer group-invite-notify-token")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"confirm":false}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(direct_unconfirmed, http::StatusCode::CONFLICT);
    let (direct_status, direct_response) = json_response(
        app,
        Request::builder()
            .method("POST")
            .uri(&direct_uri)
            .header("authorization", "Bearer group-invite-notify-token")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"confirm":true}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(direct_status, http::StatusCode::OK);
    assert_eq!(direct_response["entry"]["notifyState"], "sent");
    assert_eq!(sends.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn group_invite_operational_settings_are_persisted_authenticated_and_fail_closed() {
    let (directory, mut state) = test_state("group-invite-settings-admin");
    let app = router(state.clone());

    let (read_status, read) = json_response(
        app.clone(),
        Request::get("/api/group-invites/settings")
            .header("authorization", "Bearer group-invite-settings-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(read_status, http::StatusCode::OK);
    assert_eq!(read["settings"]["enabled"], false);
    assert_eq!(read["runtimeReady"], false);

    let payload = json!({"settings":{
        "enabled":false,
        "friendsOnly":false,
        "autoWhitelist":false,
        "notifyOwner":false
    }});
    let (unauthorized, _) = json_response(
        app.clone(),
        Request::post("/api/group-invites/settings/update")
            .header("content-type", "application/json")
            .body(Body::from(payload.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(unauthorized, http::StatusCode::UNAUTHORIZED);

    let enabled_payload = json!({"settings":{
        "enabled":true,
        "friendsOnly":true,
        "autoWhitelist":true,
        "notifyOwner":true
    }});
    let (not_ready, _) = json_response(
        app.clone(),
        Request::post("/api/group-invites/settings/update")
            .header("authorization", "Bearer group-invite-settings-admin")
            .header("content-type", "application/json")
            .body(Body::from(enabled_payload.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(not_ready, http::StatusCode::CONFLICT);

    let (saved, response) = json_response(
        app.clone(),
        Request::post("/api/group-invites/settings/update")
            .header("authorization", "Bearer group-invite-settings-admin")
            .header("content-type", "application/json")
            .body(Body::from(payload.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(saved, http::StatusCode::OK);
    assert_eq!(response["settings"]["autoWhitelist"], false);
    assert_eq!(
        state.runtime.group_invite_settings(),
        rinbot::application::group_invite::GroupInviteSettings {
            enabled: false,
            friends_only: false,
            auto_whitelist: false,
            notify_owner: false,
        }
    );
    assert_eq!(
        state
            .store
            .runtime_setting("group_invite")
            .unwrap()
            .unwrap(),
        payload["settings"]
    );
    let (notifications_disabled, _) = json_response(
        app.clone(),
        Request::post("/api/group-invites/no-record/notification/retry")
            .header("authorization", "Bearer group-invite-settings-admin")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"confirm":true}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(notifications_disabled, http::StatusCode::CONFLICT);

    let mut config = (*state.config).clone();
    config.active = true;
    config.group_invite_enabled = true;
    state.config = Arc::new(config);
    state.group_invite = Some(Arc::new(std::sync::Mutex::new(
        rinbot::group_invite::GroupInviteStore::open(directory.path().join("group-invite.sqlite"))
            .unwrap(),
    )));
    let (disconnected_status, disconnected) = json_response(
        router(state.clone()),
        Request::get("/api/group-invites/settings")
            .header("authorization", "Bearer group-invite-settings-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(disconnected_status, http::StatusCode::OK);
    assert_eq!(disconnected["runtimeReady"], false);
    let (disconnected_enable, _) = json_response(
        router(state.clone()),
        Request::post("/api/group-invites/settings/update")
            .header("authorization", "Bearer group-invite-settings-admin")
            .header("content-type", "application/json")
            .body(Body::from(enabled_payload.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(disconnected_enable, http::StatusCode::CONFLICT);
    state
        .runtime
        .onebot_connected
        .store(true, Ordering::Release);
    let ready_app = router(state.clone());
    let (enabled, response) = json_response(
        ready_app,
        Request::post("/api/group-invites/settings/update")
            .header("authorization", "Bearer group-invite-settings-admin")
            .header("content-type", "application/json")
            .body(Body::from(enabled_payload.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(enabled, http::StatusCode::OK);
    assert_eq!(response["runtimeReady"], true);
    assert!(state.runtime.group_invite_settings().enabled);
}

#[tokio::test]
async fn daily_moments_archive_is_admin_authenticated_and_unknown_publication_resolution_is_audited(
) {
    let (directory, mut state) = test_state("daily-moments-admin");
    let source = directory.path().join("daily-moments.json");
    std::fs::write(
        &source,
        r#"{"version":1,"records":[
          {"id":"moment-old","dayKey":"2026-10-01","source":"scheduled","status":"published","publishAttempted":true,"content":"private historical post","tid":"qzone-old"},
          {"id":"moment-new","dayKey":"2026-10-02","source":"scheduled","status":"publish-unknown","publishAttempted":true,"content":"possibly published once"}
        ],"scheduleSlots":[{"id":"slot-1","dayKey":"2026-10-02","status":"publish-unknown","reason":"manual check"}]}"#,
    )
    .unwrap();
    let mut moments = rinbot::daily_moments::DailyMomentsStore::open(
        directory.path().join("daily-moments.sqlite"),
    )
    .unwrap();
    moments.import_node_archive(&source).unwrap();
    moments
        .persist_generation_decision(
            "2026-10-03/18:00-19:00/1",
            "2026-10-03",
            "draft",
            &json!({
                "decision":"publish",
                "reason":"有具体想法",
                "content":"等待人工审核的草稿",
                "imageIds":[],
                "groupSummaries":[],
                "researchSources":[]
            }),
        )
        .unwrap();
    state.daily_moments = Some(Arc::new(tokio::sync::Mutex::new(moments)));
    let app = router(state);

    let (unauthorized, _) = json_response(
        app.clone(),
        Request::get("/api/daily-moments/archive?limit=10&offset=0")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(unauthorized, http::StatusCode::UNAUTHORIZED);

    let (status, page) = json_response(
        app.clone(),
        Request::get("/api/daily-moments/archive?limit=10&offset=0")
            .header("authorization", "Bearer daily-moments-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(page["total"], 3);
    assert_eq!(page["items"].as_array().unwrap().len(), 3);
    let generated_record = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["id"] == "rust-2026-10-03/18:00-19:00/1")
        .unwrap();
    assert_eq!(generated_record["status"], "preview");
    assert_eq!(generated_record["decision"], "publish");
    assert_eq!(generated_record["publishAttempted"], false);
    assert_eq!(generated_record["content"], "等待人工审核的草稿");
    assert_eq!(page["scheduleTotal"], 1);
    assert_eq!(page["scheduleSlots"][0]["id"], "slot-1");
    assert_eq!(page["generationDecisionTotal"], 1);
    assert_eq!(page["generationDecisions"][0]["outcome"], "draft");
    assert_eq!(
        page["generationDecisions"][0]["decision"]["content"],
        "等待人工审核的草稿"
    );
    assert_eq!(page["source"], "rust_daily_moments_archive");
    assert_eq!(page["readOnly"], false);
    assert_eq!(
        page["availableArchiveOperations"][0],
        "resolve_publish_unknown"
    );
    assert_eq!(page["runtimeReady"], false);
    assert!(page.to_string().contains("possibly published once"));
    let (unauthorized_resolution, _) = json_response(
        app.clone(),
        Request::post("/api/daily-moments/records/moment-new/resolve")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"confirm":true,"outcome":"missed","note":"检查"}"#,
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(unauthorized_resolution, http::StatusCode::UNAUTHORIZED);
    let (unconfirmed, _) = json_response(
        app.clone(),
        Request::post("/api/daily-moments/records/moment-new/resolve")
            .header("authorization", "Bearer daily-moments-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"confirm":false,"outcome":"missed","note":"检查"}"#,
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(unconfirmed, http::StatusCode::CONFLICT);
    let (missing_tid, _) = json_response(
        app.clone(),
        Request::post("/api/daily-moments/records/moment-new/resolve")
            .header("authorization", "Bearer daily-moments-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"confirm":true,"outcome":"sent","note":"检查"}"#,
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(missing_tid, http::StatusCode::BAD_REQUEST);
    let (resolved, resolution) = json_response(
        app.clone(),
        Request::post("/api/daily-moments/records/moment-new/resolve")
            .header("authorization", "Bearer daily-moments-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"confirm":true,"outcome":"missed","note":"已核对空间动态列表，无对应内容"}"#,
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(resolved, http::StatusCode::OK);
    assert_eq!(resolution["record"]["status"], "publish-missed");
    assert_eq!(
        resolution["record"]["operatorResolution"]["outcome"],
        "missed"
    );
    let (duplicate_resolution, _) = json_response(
        app.clone(),
        Request::post("/api/daily-moments/records/moment-new/resolve")
            .header("authorization", "Bearer daily-moments-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"confirm":true,"outcome":"missed","note":"二次尝试"}"#,
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(duplicate_resolution, http::StatusCode::CONFLICT);
    let (_, refreshed) = json_response(
        app.clone(),
        Request::get("/api/daily-moments/archive?status=publish-missed&limit=1&offset=0")
            .header("authorization", "Bearer daily-moments-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(refreshed["items"][0]["status"], "publish-missed");
    let (_, next_page) = json_response(
        app,
        Request::get("/api/daily-moments/archive?limit=1&offset=2")
            .header("authorization", "Bearer daily-moments-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(next_page["items"][0]["id"], "moment-old");
    assert!(next_page.to_string().contains("private historical post"));
}

#[tokio::test]
async fn daily_moments_settings_are_imported_validated_persisted_and_gate_runtime_enablement() {
    let (_directory, state) = test_state("daily-moments-settings-admin");
    let app = router(state.clone());
    let (unauthorized, _) = json_response(
        app.clone(),
        Request::get("/api/settings/daily-moments")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(unauthorized, http::StatusCode::UNAUTHORIZED);

    let (read_status, current) = json_response(
        app.clone(),
        Request::get("/api/settings/daily-moments")
            .header("authorization", "Bearer daily-moments-settings-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(read_status, http::StatusCode::OK);
    assert_eq!(current["settings"]["hour"], 23);
    assert_eq!(current["runtimeReady"], false);
    assert_eq!(current["generationAvailable"], false);

    let mut settings =
        serde_json::to_value(rinbot::daily_moments::DailyMomentsSettings::default()).unwrap();
    settings["hour"] = serde_json::json!(18);
    settings["scheduleWindows"] = serde_json::json!([
        {"start":"18:00","end":"20:00","count":2}
    ]);
    let (saved_status, saved) = json_response(
        app.clone(),
        Request::post("/api/settings/daily-moments/update")
            .header("authorization", "Bearer daily-moments-settings-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"settings":settings.clone()}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(saved_status, http::StatusCode::OK);
    assert_eq!(saved["settings"]["hour"], 18);
    assert_eq!(saved["runtimeReady"], false);
    assert_eq!(
        state
            .store
            .runtime_setting("daily_moments")
            .unwrap()
            .unwrap(),
        settings
    );

    settings["enabled"] = serde_json::json!(false);
    state
        .runtime
        .set_daily_moments_settings(rinbot::daily_moments::DailyMomentsSettings {
            enabled: true,
            ..Default::default()
        });
    let mut disabled_events = state.runtime.subscribe_daily_moments_disabled();
    let (disable_status, disabled) = json_response(
        app.clone(),
        Request::post("/api/settings/daily-moments/update")
            .header("authorization", "Bearer daily-moments-settings-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"settings":settings.clone()}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(disable_status, http::StatusCode::OK);
    assert_eq!(disabled["settings"]["enabled"], false);
    assert!(disabled_events.try_recv().is_ok());

    settings["enabled"] = serde_json::json!(true);
    let (enable_status, _) = json_response(
        app.clone(),
        Request::post("/api/settings/daily-moments/update")
            .header("authorization", "Bearer daily-moments-settings-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"settings":settings}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(enable_status, http::StatusCode::CONFLICT);

    let mut invalid =
        serde_json::to_value(rinbot::daily_moments::DailyMomentsSettings::default()).unwrap();
    invalid["scheduleWindows"] = serde_json::json!([
        {"start":"18:00","end":"18:03","count":1}
    ]);
    let (invalid_status, _) = json_response(
        app,
        Request::post("/api/settings/daily-moments/update")
            .header("authorization", "Bearer daily-moments-settings-admin")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"settings":invalid}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(invalid_status, http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn daily_moment_manual_publish_is_authenticated_and_fail_closed_until_runtime_ready() {
    let (directory, mut state) = test_state("daily-moment-manual-publish-admin");
    let mut moments = rinbot::daily_moments::DailyMomentsStore::open(
        directory.path().join("daily-moments.sqlite"),
    )
    .unwrap();
    moments
        .persist_generation_decision(
            "2026-10-07/fixed/23:30",
            "2026-10-07",
            "draft",
            &json!({
                "decision":"publish",
                "reason":"测试草稿",
                "content":"尚未发布的内容",
                "imageIds":[],
                "groupSummaries":[],
                "researchSources":[]
            }),
        )
        .unwrap();
    state.daily_moments = Some(Arc::new(tokio::sync::Mutex::new(moments)));
    state
        .runtime
        .onebot_connected
        .store(true, Ordering::Release);
    state
        .runtime
        .set_daily_moments_settings(rinbot::daily_moments::DailyMomentsSettings {
            enabled: true,
            ..Default::default()
        });
    Arc::make_mut(&mut state.config).active = true;
    // The API must not reach a gateway or claim the durable draft until the
    // production publish gate is explicitly opened after feature acceptance.
    let app = router(state.clone());
    let body = Body::from(r#"{"confirm":true}"#);
    let (unauthorized, _) = json_response(
        app.clone(),
        Request::post("/api/daily-moments/records/rust-2026-10-07%2Ffixed%2F23%3A30/publish")
            .header("content-type", "application/json")
            .body(body)
            .unwrap(),
    )
    .await;
    assert_eq!(unauthorized, http::StatusCode::UNAUTHORIZED);

    let (blocked, response) = json_response(
        app,
        Request::post("/api/daily-moments/records/rust-2026-10-07%2Ffixed%2F23%3A30/publish")
            .header("authorization", "Bearer daily-moment-manual-publish-admin")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"confirm":true}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(blocked, http::StatusCode::CONFLICT);
    assert_eq!(response["runtimeReady"], false);

    let store = state.daily_moments.as_ref().unwrap().lock().await;
    let record = store
        .record("rust-2026-10-07/fixed/23:30")
        .unwrap()
        .unwrap();
    assert_eq!(record["status"], "preview");
    assert_eq!(record["publishAttempted"], false);
}

#[tokio::test]
async fn daily_moment_manual_preview_is_authenticated_and_publish_mode_is_fail_closed() {
    let (_directory, state) = test_state("daily-moment-run-admin");
    let app = router(state);
    let (unauthorized, _) = json_response(
        app.clone(),
        Request::post("/api/daily-moments/run")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"publish":false}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(unauthorized, http::StatusCode::UNAUTHORIZED);

    let (publish_status, publish_response) = json_response(
        app.clone(),
        Request::post("/api/daily-moments/run")
            .header("authorization", "Bearer daily-moment-run-admin")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"publish":true,"confirm":true}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(publish_status, http::StatusCode::CONFLICT);
    assert_eq!(publish_response["publishAttempted"], false);

    let (preview_status, preview_response) = json_response(
        app,
        Request::post("/api/daily-moments/run")
            .header("authorization", "Bearer daily-moment-run-admin")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"publish":false}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(preview_status, http::StatusCode::CONFLICT);
    assert_eq!(preview_response["generationReady"], false);
}

#[tokio::test]
async fn daily_moment_manual_preview_persists_empty_snapshot_as_skip_without_model_call() {
    let (directory, mut state) = test_state("daily-moment-preview-success-admin");
    let moments = rinbot::daily_moments::DailyMomentsStore::open(
        directory.path().join("daily-moments.sqlite"),
    )
    .unwrap();
    state.daily_moments = Some(Arc::new(tokio::sync::Mutex::new(moments)));
    state.daily_moment_model = Some(Arc::new(ModelMustNotRunForEmptyDailyMomentSnapshot));
    state.daily_moments_generation_ready = true;
    Arc::make_mut(&mut state.config).active = true;
    let app = router(state.clone());
    let (status, response) = json_response(
        app,
        Request::post("/api/daily-moments/run")
            .header("authorization", "Bearer daily-moment-preview-success-admin")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"publish":false}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(response["status"], "skipped");
    assert_eq!(response["publishAttempted"], false);

    let ledger = state.daily_moments.as_ref().unwrap().lock().await;
    let slot_id = response["slotId"].as_str().unwrap();
    assert_eq!(
        ledger.schedule_slot(slot_id).unwrap().unwrap()["status"],
        "skipped"
    );
    assert_eq!(
        ledger
            .record(response["id"].as_str().unwrap())
            .unwrap()
            .unwrap()["source"],
        "manual-preview"
    );
}

#[tokio::test]
async fn daily_moment_manual_preview_persists_model_draft_without_publishing() {
    let (directory, mut state) = test_state("daily-moment-preview-draft-admin");
    let moments = rinbot::daily_moments::DailyMomentsStore::open(
        directory.path().join("daily-moments.sqlite"),
    )
    .unwrap();
    state.daily_moments = Some(Arc::new(tokio::sync::Mutex::new(moments)));
    state.daily_moment_model = Some(Arc::new(FixedDailyMomentPreviewModel));
    state.daily_moments_generation_ready = true;
    state.daily_moment_account_id = "998877".into();
    Arc::make_mut(&mut state.config).active = true;
    state
        .runtime
        .set_behavior(rinbot::application::policy::BehaviorSettings {
            access: rinbot::application::policy::AccessPolicy {
                allow_groups: vec!["301".into()],
                ..Default::default()
            },
            triggers: rinbot::application::policy::ContextTriggerConfig::default(),
        });
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    for (message_id, text) in [
        ("moment-1", "周末想出去走走"),
        ("moment-2", "天气预报说会有风"),
    ] {
        let inbox_id = state
            .store
            .insert_incoming(&IncomingMessage {
                message_id: message_id.into(),
                conversation_id: "group:301".into(),
                sender_id: "200".into(),
                sender_name: "群成员".into(),
                is_group: true,
                is_self: false,
                segments: vec![MessageSegment::Text { text: text.into() }],
                timestamp: now,
            })
            .unwrap()
            .unwrap();
        state.store.set_inbox_state(inbox_id, "acked").unwrap();
    }
    state
        .runtime
        .set_daily_moments_settings(rinbot::daily_moments::DailyMomentsSettings {
            min_messages_per_group: 1,
            max_research_calls: 0,
            ..Default::default()
        });

    let (status, response) = json_response(
        router(state.clone()),
        Request::post("/api/daily-moments/run")
            .header("authorization", "Bearer daily-moment-preview-draft-admin")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"publish":false}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK, "{response}");
    assert_eq!(response["status"], "preview");
    assert_eq!(response["publishAttempted"], false);
    assert_eq!(response["record"]["status"], "preview");
    assert_eq!(response["record"]["source"], "manual-preview");
    assert_eq!(
        response["record"]["content"],
        "今天的风很舒服，想找个有树的地方慢慢走走。"
    );
    assert_eq!(response["record"]["accountId"], "998877");

    let ledger = state.daily_moments.as_ref().unwrap().lock().await;
    let slot = ledger
        .schedule_slot(response["slotId"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(slot["status"], "draft-ready");
    assert_eq!(slot["source"], "manual-preview");
}

#[tokio::test]
async fn daily_moment_manual_publish_rechecks_identity_and_claims_external_write_once() {
    let (directory, mut state) = test_state("daily-moment-publish-live-gate");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let settings = rinbot::daily_moments::DailyMomentsSettings {
        enabled: true,
        hour: 0,
        minute: 0,
        ..Default::default()
    };
    let mut ledger = rinbot::daily_moments::DailyMomentsStore::open(
        directory.path().join("daily-moments.sqlite"),
    )
    .unwrap();
    ledger
        .update_schedule_for_settings(&settings, now, || 0.0, false)
        .unwrap();
    let slot = ledger.claim_due_schedule_slot(now).unwrap().unwrap();
    let slot_id = slot["id"].as_str().unwrap();
    let day_key = slot["dayKey"].as_str().unwrap();
    let record_id = format!("rust-{slot_id}");
    ledger
        .persist_generation_decision(
            slot_id,
            day_key,
            "draft",
            &json!({
                "decision":"publish",
                "reason":"测试人工审核发布",
                "content":"这是一条受控测试动态",
                "imageIds":[],
                "groupSummaries":[{"chatKey":"group:123","summary":"公开话题"}],
                "researchSources":[]
            }),
        )
        .unwrap();
    assert!(ledger
        .persist_publish_context(
            &record_id,
            "123456789",
            &rinbot::application::daily_moments::daily_moment_persona_hash(
                &state.daily_moment_persona,
                "测试机器人",
            ),
            rinbot::application::daily_moments::DAILY_MOMENT_PROMPT_VERSION,
        )
        .unwrap());
    assert!(ledger
        .complete_generated_slot(slot_id, "draft-ready", &record_id)
        .unwrap());
    state.daily_moments = Some(Arc::new(tokio::sync::Mutex::new(ledger)));
    state.daily_moments_runtime_ready = true;
    state
        .runtime
        .onebot_connected
        .store(true, Ordering::Release);
    state.runtime.set_daily_moments_settings(settings);
    let mut behavior = state.runtime.behavior();
    behavior.access.allow_groups = vec!["123".into()];
    state.runtime.set_behavior(behavior);
    Arc::make_mut(&mut state.config).active = true;
    let writes = Arc::new(AtomicUsize::new(0));
    let gateway = Arc::new(FakeDailyMomentGateway {
        account_id: "123456789".into(),
        nickname: "测试机器人".into(),
        writes: writes.clone(),
    });
    let encoded_id = record_id.replace('/', "%2F");
    let route = format!("/api/daily-moments/records/{encoded_id}/publish");
    let wrong_account_writes = Arc::new(AtomicUsize::new(0));
    let wrong_account_gateway = Arc::new(FakeDailyMomentGateway {
        account_id: "987654321".into(),
        nickname: "测试机器人".into(),
        writes: wrong_account_writes.clone(),
    });
    state.daily_moment_gateway = Some(wrong_account_gateway.clone());
    state.daily_moment_image_reader = Some(wrong_account_gateway);
    let (account_mismatch, _) = json_response(
        router(state.clone()),
        Request::post(&route)
            .header("authorization", "Bearer daily-moment-publish-live-gate")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"confirm":true}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(account_mismatch, http::StatusCode::CONFLICT);
    assert_eq!(wrong_account_writes.load(Ordering::SeqCst), 0);

    state.daily_moment_gateway = Some(gateway.clone());
    state.daily_moment_image_reader = Some(gateway.clone());
    state.daily_moment_persona.self_nickname = "人设已变更".into();
    let (persona_mismatch, _) = json_response(
        router(state.clone()),
        Request::post(&route)
            .header("authorization", "Bearer daily-moment-publish-live-gate")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"confirm":true}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(persona_mismatch, http::StatusCode::CONFLICT);
    assert_eq!(writes.load(Ordering::SeqCst), 0);

    state.daily_moment_persona = Default::default();
    let mut behavior = state.runtime.behavior();
    behavior.access.allow_groups.clear();
    state.runtime.set_behavior(behavior);
    let (source_mismatch, _) = json_response(
        router(state.clone()),
        Request::post(&route)
            .header("authorization", "Bearer daily-moment-publish-live-gate")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"confirm":true}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(source_mismatch, http::StatusCode::CONFLICT);
    assert_eq!(writes.load(Ordering::SeqCst), 0);

    let mut behavior = state.runtime.behavior();
    behavior.access.allow_groups = vec!["123".into()];
    state.runtime.set_behavior(behavior);
    let app = router(state.clone());
    let (published, response) = json_response(
        app.clone(),
        Request::post(&route)
            .header("authorization", "Bearer daily-moment-publish-live-gate")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"confirm":true}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(published, http::StatusCode::OK);
    assert_eq!(response["status"], "published");
    assert_eq!(response["tid"], "confirmed-api-tid");
    assert_eq!(writes.load(Ordering::SeqCst), 1);

    let daily_store = state.daily_moments.as_ref().unwrap().clone();
    let _busy_ledger = daily_store.lock().await;
    let (busy, busy_response) = json_response(
        app.clone(),
        Request::post(&route)
            .header("authorization", "Bearer daily-moment-publish-live-gate")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"confirm":true}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(busy, http::StatusCode::CONFLICT);
    assert!(busy_response["error"]
        .as_str()
        .unwrap()
        .contains("账本正忙"));
    drop(_busy_ledger);

    let (replay, _) = json_response(
        app,
        Request::post(&route)
            .header("authorization", "Bearer daily-moment-publish-live-gate")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"confirm":true}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(replay, http::StatusCode::CONFLICT);
    assert_eq!(writes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn private_whitelist_retry_is_admin_authenticated_and_does_not_repeat_qq_action() {
    let (directory, mut state) = test_state("whitelist-retry-token");
    let mut identity =
        rinbot::identity::IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
    let (request, _) = identity
        .record_request("12345", "one-time-flag", "", 10, 100)
        .unwrap();
    let action = identity
        .begin_decision(&request.id, "approve", "admin", 120)
        .unwrap();
    identity
        .complete_decision(&request.id, &action.attempt_id, "approved", "", 130)
        .unwrap();
    let identity = Arc::new(std::sync::Mutex::new(identity));
    state.identity = Some(identity.clone());
    state.identity_executor = Some(Arc::new(
        rinbot::application::identity_executor::IdentityActionExecutor::new(
            identity,
            Arc::new(FriendRequestGatewayOk),
            true,
            true,
            Some(Arc::new(PrivateWhitelistOk)),
        ),
    ));
    let uri = format!(
        "/api/identity/incoming-friend-requests/{}/whitelist/retry",
        request.id
    );
    let app = router(state);

    let (status, _) = json_response(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri(&uri)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::UNAUTHORIZED);

    let (status, response) = json_response(
        app,
        Request::builder()
            .method("POST")
            .uri(&uri)
            .header("authorization", "Bearer whitelist-retry-token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(response["request"]["status"], "approved");
    assert_eq!(response["request"]["whitelistApplied"], true);
}

#[tokio::test]
async fn owner_notification_retry_is_authenticated_durable_and_single_send() {
    let (directory, mut state) = test_state("identity-notify-token");
    let mut identity =
        rinbot::identity::IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
    let (request, _) = identity
        .record_request(
            "12345",
            "private-notification-flag",
            "请联系管理员",
            10,
            100,
        )
        .unwrap();
    let identity = Arc::new(std::sync::Mutex::new(identity));
    state.identity = Some(identity.clone());
    let sends = Arc::new(AtomicUsize::new(0));
    state.identity_notification_executor = Some(Arc::new(
        rinbot::application::identity_notification::IdentityNotificationExecutor::new(
            identity,
            Arc::new(OwnerMessageGateway(sends.clone())),
            true,
        ),
    ));
    let mut config = (*state.config).clone();
    config.identity_owner_uin = Some("998877".into());
    state.config = Arc::new(config);
    let uri = format!(
        "/api/identity/incoming-friend-requests/{}/notification/retry",
        request.id
    );
    let app = router(state);

    let (status, _) = json_response(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri(&uri)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::UNAUTHORIZED);

    let (status, response) = json_response(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri(&uri)
            .header("authorization", "Bearer identity-notify-token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(response["request"]["notificationState"], "sent");
    assert_eq!(
        response["request"]["notificationMessageId"],
        "owner-notice-1"
    );

    let (status, _) = json_response(
        app,
        Request::builder()
            .method("POST")
            .uri(&uri)
            .header("authorization", "Bearer identity-notify-token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::CONFLICT);
    assert_eq!(sends.load(Ordering::SeqCst), 1);
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

#[tokio::test]
async fn qzone_manual_run_requires_auth_confirmation_and_ready_serial_scheduler() {
    let (directory, mut state) = test_state("qzone-admin-token");
    let (manual_tx, mut manual_rx) = tokio::sync::mpsc::channel(1);
    state.qzone_manual_run = Some(manual_tx);
    state.qzone_interactions = Some(Arc::new(tokio::sync::Mutex::new(
        rinbot::qzone_interactions::QzoneInteractionsStore::open(
            &directory.path().join("qzone-interactions.sqlite"),
        )
        .unwrap(),
    )));
    Arc::get_mut(&mut state.config).unwrap().active = true;
    let app = router(state.clone());

    let (status, _) = json_response(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri("/api/qzone-interactions/run")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"kind":"all","confirm":true}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::UNAUTHORIZED);
    assert!(manual_rx.try_recv().is_err());

    let (status, response) = json_response(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri("/api/qzone-interactions/run")
            .header("authorization", "Bearer qzone-admin-token")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"kind":"all","confirm":false}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::CONFLICT);
    assert!(response["error"].as_str().unwrap().contains("显式确认"));
    assert!(manual_rx.try_recv().is_err());

    let (status, _) = json_response(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri("/api/qzone-interactions/run")
            .header("authorization", "Bearer qzone-admin-token")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"kind":"all","confirm":true}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::CONFLICT);
    assert!(manual_rx.try_recv().is_err());

    state.qzone_runtime_ready = true;
    state
        .runtime
        .onebot_connected
        .store(true, Ordering::Release);
    let mut settings = state.runtime.qzone_interactions_settings();
    settings.enabled = true;
    state.runtime.set_qzone_interactions_settings(settings);
    let app = router(state);

    let (status, response) = json_response(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri("/api/qzone-interactions/run")
            .header("authorization", "Bearer qzone-admin-token")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"kind":"feed","confirm":true}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::ACCEPTED);
    assert_eq!(response["queued"], true);

    let (status, _) = json_response(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/qzone-interactions/run")
            .header("authorization", "Bearer qzone-admin-token")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"kind":"reply","confirm":true}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::CONFLICT);
    assert_eq!(manual_rx.try_recv().unwrap(), "feed");
}

#[tokio::test]
async fn memory_consolidation_manual_run_fails_closed_before_runtime_readiness() {
    let (_directory, state) = test_state("memory-admin-token");
    let mut settings =
        rinbot::application::memory_consolidation::MemoryConsolidationSettings::default();
    settings.enabled = true;
    state.runtime.set_memory_consolidation_settings(settings);
    assert!(!state.memory_consolidation_runtime_ready);

    let app = router(state);
    let (status, response) = json_response(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri("/api/memory-consolidation/run")
            .header("authorization", "Bearer memory-admin-token")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"chatKey":"private:123456","userId":"123456","confirm":true}"#,
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::CONFLICT);
    assert!(response["error"]
        .as_str()
        .unwrap()
        .contains("未调用模型、未修改记忆"));

    let (batch_status, batch_response) = json_response(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/memory-consolidation/run")
            .header("authorization", "Bearer memory-admin-token")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"chatKey":"group:123456","userIds":["123456","234567"],"confirm":true}"#,
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(batch_status, http::StatusCode::CONFLICT);
    assert!(batch_response["error"]
        .as_str()
        .unwrap()
        .contains("未调用模型、未修改记忆"));
}

#[tokio::test]
async fn memory_consolidation_batch_validates_all_ids_and_accepts_more_than_twenty_people() {
    let (_directory, mut state) = test_state("memory-batch-admin-token");
    let mut settings =
        rinbot::application::memory_consolidation::MemoryConsolidationSettings::default();
    settings.enabled = true;
    state.runtime.set_memory_consolidation_settings(settings);
    state.memory_consolidation_runtime_ready = true;
    let app = router(state);

    for body in [
        json!({"chatKey":"group:123456","userIds":[],"confirm":true}),
        json!({"chatKey":"group:123456","userIds":["123456","bad"],"confirm":true}),
        json!({"chatKey":"group:123456","userId":"123456","userIds":["234567"],"confirm":true}),
    ] {
        let (status, _) = json_response(
            app.clone(),
            Request::post("/api/memory-consolidation/run")
                .header("authorization", "Bearer memory-batch-admin-token")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await;
        assert_eq!(status, http::StatusCode::BAD_REQUEST);
    }

    let twenty_one_ids = (100_000..100_021)
        .map(|user_id| user_id.to_string())
        .collect::<Vec<_>>();
    let (status, response) = json_response(
        app,
        Request::post("/api/memory-consolidation/run")
            .header("authorization", "Bearer memory-batch-admin-token")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"chatKey":"group:123456","userIds":twenty_one_ids,"confirm":true})
                    .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::SERVICE_UNAVAILABLE);
    assert!(response["error"].as_str().unwrap().contains("执行器不可用"));
}

#[tokio::test]
async fn memory_consolidation_manual_batch_returns_accepted_and_publishes_terminal_state() {
    let (directory, mut state) = test_state("memory-admin");
    let mut identity_store =
        rinbot::identity::IdentityStore::open(directory.path().join("identity.sqlite"))
            .expect("identity store");
    identity_store
        .observe_message("m1", "group:123", "123", "测试甲", 1, 1)
        .unwrap();
    identity_store
        .observe_message("m2", "group:123", "234", "测试乙", 1, 1)
        .unwrap();
    identity_store
        .import_legacy_global_memories(
            &[
                json!({
                    "uin":"123","name":"测试甲","chat_key":"group:123",
                    "content":"已有甲印象","created_at":1,"observed_at":1,
                    "origin":"manual","source_file":"legacy/memory/people/123.json"
                }),
                json!({
                    "uin":"234","name":"测试乙","chat_key":"group:123",
                    "content":"已有乙印象","created_at":1,"observed_at":1,
                    "origin":"manual","source_file":"legacy/memory/people/234.json"
                }),
            ],
            1,
        )
        .unwrap();
    for index in 0..20 {
        state
            .store
            .insert_incoming(&IncomingMessage {
                message_id: format!("unindexed-{index}"),
                conversation_id: "group:123".into(),
                sender_id: "345".into(),
                sender_name: "未索引活跃成员".into(),
                is_group: true,
                is_self: false,
                segments: vec![MessageSegment::Text {
                    text: "最近在学习新技术".into(),
                }],
                timestamp: index,
            })
            .unwrap()
            .unwrap();
    }
    let identity = Arc::new(std::sync::Mutex::new(identity_store));
    state.identity = Some(identity.clone());
    state.memory_consolidation_runtime_ready = true;
    state.memory_consolidation_executor = Some(Arc::new(
        rinbot::application::memory_consolidation::MemoryConsolidationExecutor::new(
            state.store.clone(),
            identity,
            Arc::new(FixedDailyMomentPreviewModel),
            Some("123".into()),
        ),
    ));
    state.runtime.set_memory_consolidation_settings(
        rinbot::application::memory_consolidation::MemoryConsolidationSettings {
            enabled: true,
            ..Default::default()
        },
    );

    let request = Request::post("/api/memory-consolidation/run")
        .header("content-type", "application/json")
        .header("authorization", "Bearer memory-admin")
        .body(Body::from(
            json!({"chatKey":"group:123","confirm":true}).to_string(),
        ))
        .unwrap();
    let (status, accepted) = json_response(router(state.clone()), request).await;
    assert_eq!(status, http::StatusCode::ACCEPTED);
    assert_eq!(accepted["accepted"], true);
    assert_eq!(accepted["state"], "running");

    let app = router(state);
    for _ in 0..50 {
        let request = Request::get("/api/memory-consolidation/status")
            .header("authorization", "Bearer memory-admin")
            .body(Body::empty())
            .unwrap();
        let (current_status, current) = json_response(app.clone(), request).await;
        if current["manualRun"]["state"] != "running" {
            assert_eq!(current_status, http::StatusCode::OK, "body: {current}");
            assert_eq!(current["automaticSchedulingReady"], true);
            assert_eq!(
                current["manualRun"]["state"], "completed",
                "status: {current}"
            );
            assert_eq!(current["manualRun"]["requested"], 3);
            assert_eq!(current["manualRun"]["failed"].as_array().unwrap().len(), 3);
            assert_eq!(current["manualRun"]["failed"][2]["userId"], "345");
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("manual batch did not publish a terminal status");
}

#[tokio::test]
async fn outbox_reconciliation_requires_explicit_admin_token_even_on_loopback() {
    let (_directory, state) = test_state("");
    state
        .store
        .enqueue_outbox("out-unauth", "private:100", &json!({ "text": "hello" }))
        .unwrap();
    state.store.mark_outbox_sending("out-unauth").unwrap();
    state
        .store
        .finish_outbox("out-unauth", "unknown", None, Some("timeout"))
        .unwrap();

    let (status, list) = json_response(
        router(state.clone()),
        Request::builder()
            .uri("/api/outbox/unknown")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(list["items"][0]["id"], "out-unauth");
    assert_eq!(list["items"][0]["conversation_id"], "private:100");
    assert_eq!(list["items"][0]["payload"], r#"{"text":"hello"}"#);

    let (status, _) = json_response(
        router(state.clone()),
        Request::builder()
            .method("POST")
            .uri("/api/outbox/out-unauth/reconcile")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"outcome":"sent"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::UNAUTHORIZED);
    assert_eq!(state.store.unknown_outbox().unwrap().len(), 1);

    let (_directory, state) = test_state("admin-secret");
    state
        .store
        .enqueue_outbox("out-auth", "private:100", &json!({ "text": "hello" }))
        .unwrap();
    state.store.mark_outbox_sending("out-auth").unwrap();
    state
        .store
        .finish_outbox("out-auth", "unknown", None, Some("timeout"))
        .unwrap();
    let (status, response) = json_response(
        router(state.clone()),
        Request::builder()
            .method("POST")
            .uri("/api/outbox/out-auth/reconcile")
            .header("authorization", "Bearer admin-secret")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"outcome":"failed"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(response["outcome"], "failed");
    assert!(state.store.unknown_outbox().unwrap().is_empty());
}

#[tokio::test]
async fn inbox_tasks_are_paginated_and_only_queued_tasks_can_be_cancelled_with_admin_auth() {
    let (_directory, state) = test_state("task-admin");
    let queued_id = insert_message(&state, "queued-task");
    let processing_id = insert_message(&state, "processing-task");
    assert!(state
        .store
        .claim_inbox(processing_id, "worker", 30_000)
        .unwrap());

    let (status, list) = json_response(
        router(state.clone()),
        Request::builder()
            .uri("/api/tasks?limit=1&offset=0")
            .header("authorization", "Bearer task-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(list["scope"], "message_inbox");
    assert_eq!(list["items"].as_array().unwrap().len(), 1);

    let (status, _) = json_response(
        router(state.clone()),
        Request::builder()
            .method("POST")
            .uri(format!("/api/tasks/inbox-{queued_id}/cancel"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::UNAUTHORIZED);
    assert!(state
        .store
        .queued_inbox_ids(0, 10)
        .unwrap()
        .contains(&queued_id));

    let (status, response) = json_response(
        router(state.clone()),
        Request::builder()
            .method("POST")
            .uri(format!("/api/tasks/inbox-{queued_id}/cancel"))
            .header("authorization", "Bearer task-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(response["ok"], true);
    assert!(!state
        .store
        .queued_inbox_ids(0, 10)
        .unwrap()
        .contains(&queued_id));

    let (status, _) = json_response(
        router(state),
        Request::builder()
            .method("POST")
            .uri(format!("/api/tasks/inbox-{processing_id}/cancel"))
            .header("authorization", "Bearer task-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::CONFLICT);
}

#[tokio::test]
async fn queue_settings_require_admin_auth_validate_and_persist_live_updates() {
    let (_directory, state) = test_state("queue-admin");
    let app = router(state.clone());
    let settings = json!({
        "wake_delay_min_ms": 250,
        "wake_delay_max_ms": 750,
        "max_batch_wait_ms": 5000,
        "batch_limit": 25,
        "batch_max_chars": 16000,
        "max_concurrent_runs": 3
    });
    let unauthorized = app
        .clone()
        .oneshot(
            Request::post("/api/settings/queue/update")
                .header("content-type", "application/json")
                .body(Body::from(settings.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), http::StatusCode::UNAUTHORIZED);

    let response = app
        .clone()
        .oneshot(
            Request::post("/api/settings/queue/update")
                .header("authorization", "Bearer queue-admin")
                .header("content-type", "application/json")
                .body(Body::from(settings.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(state.runtime.queue_settings().batch_limit, 25);
    assert_eq!(state.runtime.queue_settings().max_concurrent_runs, 3);
    assert_eq!(
        state.store.runtime_setting("queue").unwrap().unwrap(),
        settings
    );

    let invalid = json!({
        "wake_delay_min_ms": 900,
        "wake_delay_max_ms": 100,
        "max_batch_wait_ms": 5000,
        "batch_limit": 25,
        "batch_max_chars": 16000,
        "max_concurrent_runs": 3
    });
    let response = app
        .oneshot(
            Request::post("/api/settings/queue/update")
                .header("authorization", "Bearer queue-admin")
                .header("content-type", "application/json")
                .body(Body::from(invalid.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), http::StatusCode::BAD_REQUEST);
    assert_eq!(state.runtime.queue_settings().batch_limit, 25);
}

#[tokio::test]
async fn time_control_settings_require_admin_auth_validate_persist_and_apply_live() {
    let (_directory, state) = test_state("time-control-admin");
    let app = router(state.clone());
    let settings = json!({
        "enabled": true,
        "schedule": {"mode":"always","windows":[]},
        "overrides": {}
    });
    let unauthorized = app
        .clone()
        .oneshot(
            Request::post("/api/settings/time-control/update")
                .header("content-type", "application/json")
                .body(Body::from(settings.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), http::StatusCode::UNAUTHORIZED);

    let invalid = app.clone().oneshot(
        Request::post("/api/settings/time-control/update")
            .header("authorization", "Bearer time-control-admin")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"enabled":true,"schedule":{"mode":"custom","windows":[{"days":[8],"start":"25:00","end":"25:00"}]},"overrides":{}}"#))
            .unwrap(),
    ).await.unwrap();
    assert_eq!(invalid.status(), http::StatusCode::BAD_REQUEST);

    let saved = app
        .clone()
        .oneshot(
            Request::post("/api/settings/time-control/update")
                .header("authorization", "Bearer time-control-admin")
                .header("content-type", "application/json")
                .body(Body::from(settings.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(saved.status(), http::StatusCode::OK);
    assert!(state.runtime.time_control().enabled);
    assert_eq!(
        state
            .store
            .runtime_setting("time_control")
            .unwrap()
            .unwrap(),
        settings
    );

    let (_, fetched) = json_response(
        app,
        Request::get("/api/settings/time-control")
            .header("authorization", "Bearer time-control-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(fetched, settings);
}

#[tokio::test]
async fn web_search_settings_require_admin_auth_validate_persist_and_apply_live() {
    let (_directory, state) = test_state("web-search-admin");
    let live = state.runtime.web_search_settings_handle();
    let app = router(state.clone());
    let settings = json!({
        "enabled": true,
        "provider": "bing",
        "search_url": "https://cn.bing.com/search",
        "max_results": 8
    });

    let unauthorized = app
        .clone()
        .oneshot(
            Request::post("/api/settings/web-search/update")
                .header("content-type", "application/json")
                .body(Body::from(settings.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), http::StatusCode::UNAUTHORIZED);

    let invalid = app
        .clone()
        .oneshot(
            Request::post("/api/settings/web-search/update")
                .header("authorization", "Bearer web-search-admin")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "enabled": true,
                        "provider": "bing",
                        "search_url": "https://attacker.invalid/search",
                        "max_results": 8
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), http::StatusCode::BAD_REQUEST);
    assert!(!live.read().unwrap().enabled);

    let unsupported_provider = app
        .clone()
        .oneshot(
            Request::post("/api/settings/web-search/update")
                .header("authorization", "Bearer web-search-admin")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "enabled": true,
                        "provider": "deepseek",
                        "search_url": "https://cn.bing.com/search",
                        "max_results": 8
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unsupported_provider.status(), http::StatusCode::BAD_REQUEST);
    assert!(!live.read().unwrap().enabled);

    let saved = app
        .clone()
        .oneshot(
            Request::post("/api/settings/web-search/update")
                .header("authorization", "Bearer web-search-admin")
                .header("content-type", "application/json")
                .body(Body::from(settings.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(saved.status(), http::StatusCode::OK);
    assert_eq!(live.read().unwrap().max_results, 8);
    assert_eq!(
        state.store.runtime_setting("web_search").unwrap().unwrap(),
        settings
    );

    let (_, fetched) = json_response(
        app,
        Request::get("/api/settings/web-search")
            .header("authorization", "Bearer web-search-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(fetched, settings);
}

#[tokio::test]
async fn asr_settings_require_admin_auth_persist_live_and_never_return_key() {
    let (directory, state) = test_state("asr-admin");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let runtime = state.runtime.asr_settings_handle();
    let app = router(state.clone());
    let settings = json!({
        "enabled": true,
        "provider": "openai",
        "base_url": "https://speech.example.invalid/v1",
        "model": "whisper-test",
        "language": "zh",
        "max_per_hour": 9,
        "timeout_ms": 120000
    });
    let payload = json!({"settings":settings,"api_key":"private-asr-key","clear_api_key":false});

    let unauthorized = app
        .clone()
        .oneshot(
            Request::post("/api/settings/asr/update")
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), http::StatusCode::UNAUTHORIZED);

    let unsupported = json!({
        "settings": {"enabled":true,"provider":"aliyun","base_url":"","model":"","language":"","max_per_hour":9,"timeout_ms":120000}
    });
    let rejected = app
        .clone()
        .oneshot(
            Request::post("/api/settings/asr/update")
                .header("authorization", "Bearer asr-admin")
                .header("content-type", "application/json")
                .body(Body::from(unsupported.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status(), http::StatusCode::BAD_REQUEST);

    let (status, response) = json_response(
        app.clone(),
        Request::post("/api/settings/asr/update")
            .header("authorization", "Bearer asr-admin")
            .header("content-type", "application/json")
            .body(Body::from(payload.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK, "{response}");
    assert_eq!(response["configured"], true);
    assert_eq!(response["apiKeyConfigured"], true);
    assert!(!response.to_string().contains("private-asr-key"));
    assert_eq!(
        runtime.read().unwrap().settings,
        serde_json::from_value(settings.clone()).unwrap()
    );
    assert_eq!(
        state.store.runtime_setting("asr").unwrap().unwrap(),
        settings
    );

    let secret_path = directory.path().join("asr-credentials.json");
    assert!(secret_path.is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(secret_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let (_, readback) = json_response(
        app,
        Request::get("/api/settings/asr")
            .header("authorization", "Bearer asr-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(readback["settings"], settings);
    assert!(!readback.to_string().contains("private-asr-key"));
}

#[tokio::test]
async fn behavior_settings_require_admin_auth_reject_open_access_and_apply_live() {
    let (_directory, state) = test_state("behavior-admin");
    let pipeline_view = state.runtime.behavior_handle();
    let app = router(state.clone());
    let settings = json!({
        "access": {
            "allow_groups": ["98765"],
            "allow_private": ["12345"],
            "deny_groups": [],
            "deny_private": ["22222"],
            "allow_all_when_empty": false,
            "blocked_group_users": {"98765": ["33333"]}
        },
        "triggers": {
            "random_percent": 12.5,
            "keywords": ["帮助", "状态"],
            "at_count": 80,
            "keyword_count": 40,
            "random_count": 20,
            "all_count": 100,
            "unified_tier": false,
            "group_random_percent": {"98765": 0.0}
        }
    });
    let unauthorized = app
        .clone()
        .oneshot(
            Request::post("/api/settings/behavior/update")
                .header("content-type", "application/json")
                .body(Body::from(settings.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), http::StatusCode::UNAUTHORIZED);

    let unsafe_settings = json!({
        "access": {"allow_groups":[],"allow_private":[],"deny_groups":[],"deny_private":[],"allow_all_when_empty":true},
        "triggers": {"random_percent":150.0,"keywords":[],"at_count":1,"keyword_count":1,"random_count":1,"all_count":1}
    });
    let invalid = app
        .clone()
        .oneshot(
            Request::post("/api/settings/behavior/update")
                .header("authorization", "Bearer behavior-admin")
                .header("content-type", "application/json")
                .body(Body::from(unsafe_settings.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), http::StatusCode::BAD_REQUEST);

    let saved = app
        .clone()
        .oneshot(
            Request::post("/api/settings/behavior/update")
                .header("authorization", "Bearer behavior-admin")
                .header("content-type", "application/json")
                .body(Body::from(settings.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(saved.status(), http::StatusCode::OK);
    let active = state.runtime.behavior();
    assert_eq!(active.access.allow_private, vec!["12345"]);
    assert_eq!(active.access.blocked_group_users["98765"], ["33333"]);
    assert_eq!(active.triggers.random_percent, 12.5);
    assert!(!active.triggers.unified_tier);
    assert_eq!(active.triggers.group_random_percent["98765"], 0.0);
    assert_eq!(
        pipeline_view.read().unwrap().access.allow_groups,
        vec!["98765"]
    );
    assert_eq!(
        state.store.runtime_setting("behavior").unwrap().unwrap(),
        settings
    );

    let (_, fetched) = json_response(
        app,
        Request::get("/api/settings/behavior")
            .header("authorization", "Bearer behavior-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(fetched, settings);
}

#[tokio::test]
async fn incident_ledger_api_is_readable_and_mutations_require_admin_confirmation() {
    let (_directory, state) = test_state("incident-admin");
    let app = router(state.clone());
    let incident = state
        .incident
        .as_ref()
        .unwrap()
        .capture(&rinbot::incident::IncidentInput {
            code: "TEST_FAILURE".into(),
            category: "external_write".into(),
            severity: "critical".into(),
            source: "api-test".into(),
            chat_key: "group:98765".into(),
            session_id: String::new(),
            operation_id: "op-test".into(),
            message: "authorization=Bearer should-not-leak".into(),
            details: json!({"api_key":"private"}),
            now_ms: 1_800_000_000_000,
        })
        .unwrap()
        .unwrap();

    let (_, status) = json_response(
        app.clone(),
        Request::get("/api/incident-pilot/status")
            .header("authorization", "Bearer incident-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status["enabled"], true);
    assert_eq!(status["graduated"], true);
    assert_eq!(status["critical"], 1);

    let (_, listed) = json_response(
        app.clone(),
        Request::get("/api/incidents?state=open&severity=critical")
            .header("authorization", "Bearer incident-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(listed["items"][0]["id"], incident.id);
    assert!(!listed.to_string().contains("should-not-leak"));
    assert!(!listed.to_string().contains("private\""));

    let unauthorized = app
        .clone()
        .oneshot(
            Request::post(format!("/api/incidents/{}/acknowledge", incident.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), http::StatusCode::UNAUTHORIZED);
    let acknowledged = app
        .clone()
        .oneshot(
            Request::post(format!("/api/incidents/{}/acknowledge", incident.id))
                .header("authorization", "Bearer incident-admin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(acknowledged.status(), http::StatusCode::OK);

    let resolved = app
        .clone()
        .oneshot(
            Request::post(format!("/api/incidents/{}/resolve", incident.id))
                .header("authorization", "Bearer incident-admin")
                .header("content-type", "application/json")
                .body(Body::from(json!({"resolution":"已核实"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resolved.status(), http::StatusCode::OK);

    let deleted_without_confirmation = app
        .clone()
        .oneshot(
            Request::delete(format!("/api/incidents/{}", incident.id))
                .header("authorization", "Bearer incident-admin")
                .header("content-type", "application/json")
                .body(Body::from(json!({"confirm":false}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        deleted_without_confirmation.status(),
        http::StatusCode::CONFLICT
    );
    let unauthorized_delete = app
        .clone()
        .oneshot(
            Request::delete(format!("/api/incidents/{}", incident.id))
                .header("content-type", "application/json")
                .body(Body::from(json!({"confirm":true}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized_delete.status(), http::StatusCode::UNAUTHORIZED);
    let deleted = app
        .oneshot(
            Request::delete(format!("/api/incidents/{}", incident.id))
                .header("authorization", "Bearer incident-admin")
                .header("content-type", "application/json")
                .body(Body::from(json!({"confirm":true}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.status(), http::StatusCode::OK);
}

#[tokio::test]
async fn incident_startup_failure_keeps_status_and_read_pages_available() {
    let (_directory, mut state) = test_state("incident-admin");
    state.incident = None;
    state.incident_settings.enabled = true;
    state.incident_error = Some("ledger database is unavailable".into());
    let app = router(state);

    let (status_code, status) = json_response(
        app.clone(),
        Request::get("/api/incident-pilot/status")
            .header("authorization", "Bearer incident-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status_code, http::StatusCode::OK);
    assert_eq!(status["enabled"], true);
    assert_eq!(status["active"], false);
    assert_eq!(status["exists"], false);
    assert_eq!(status["error"], "ledger database is unavailable");

    let (settings_code, settings) = json_response(
        app.clone(),
        Request::get("/api/incident-pilot/settings")
            .header("authorization", "Bearer incident-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(settings_code, http::StatusCode::OK);
    assert_eq!(settings["runtimeReady"], false);
    assert_eq!(settings["settings"]["enabled"], true);

    let (incidents_code, incidents) = json_response(
        app.clone(),
        Request::get("/api/incidents?limit=20")
            .header("authorization", "Bearer incident-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(incidents_code, http::StatusCode::OK);
    assert_eq!(incidents["items"], json!([]));
    assert_eq!(incidents["limit"], 20);

    let (controls_code, controls) = json_response(
        app,
        Request::get("/api/incidents/chat-controls")
            .header("authorization", "Bearer incident-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(controls_code, http::StatusCode::OK);
    assert_eq!(controls["items"], json!([]));
}

#[tokio::test]
async fn incident_lifecycle_is_authenticated_persistent_and_preserves_history_when_disabled() {
    let (_directory, state) = test_state("incident-admin");
    let incident = state
        .incident
        .as_ref()
        .unwrap()
        .capture(&rinbot::incident::IncidentInput {
            code: "LIFECYCLE_FIXTURE".into(),
            category: "internal".into(),
            severity: "error".into(),
            source: "api-test".into(),
            chat_key: "group:98765".into(),
            session_id: String::new(),
            operation_id: String::new(),
            message: "保留的异常记录".into(),
            details: json!({}),
            now_ms: 1_800_000_000_000,
        })
        .unwrap()
        .unwrap();
    let app = router(state.clone());

    let unauthorized = app
        .clone()
        .oneshot(
            Request::post("/api/incident-pilot/lifecycle")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"enabled":false,"graduated":true}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), http::StatusCode::UNAUTHORIZED);

    let disabled = app
        .clone()
        .oneshot(
            Request::post("/api/incident-pilot/lifecycle")
                .header("authorization", "Bearer incident-admin")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"enabled":false,"graduated":true}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(disabled.status(), http::StatusCode::OK);
    let settings = state
        .store
        .runtime_setting("incident_pilot")
        .unwrap()
        .unwrap();
    assert_eq!(settings["enabled"], false);
    assert_eq!(settings["graduated"], true);

    let (status_code, status) = json_response(
        app.clone(),
        Request::get("/api/incident-pilot/status")
            .header("authorization", "Bearer incident-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        status_code,
        http::StatusCode::OK,
        "status response: {status}"
    );
    assert_eq!(status["enabled"], false);
    assert_eq!(status["graduated"], true);
    assert_eq!(status["open"], 1);

    let listed = json_response(
        app.clone(),
        Request::get("/api/incidents?limit=10")
            .header("authorization", "Bearer incident-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .1;
    assert_eq!(listed["items"][0]["id"], incident.id);

    let reenable = app
        .oneshot(
            Request::post("/api/incident-pilot/lifecycle")
                .header("authorization", "Bearer incident-admin")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"enabled":true,"graduated":true}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reenable.status(), http::StatusCode::CONFLICT);
    assert_eq!(
        state.incident.as_ref().unwrap().settings().unwrap().enabled,
        false
    );
}

#[tokio::test]
async fn incident_operational_settings_are_authenticated_validated_and_do_not_change_lifecycle() {
    let (_directory, state) = test_state("incident-admin");
    let app = router(state.clone());
    let body = json!({
        "settings": {
            "notifyWarnings": false,
            "duplicateWindowMinutes": 25,
            "retentionDays": 120,
            "unknownWritesBlockChat": true
        }
    });
    let unauthorized = app
        .clone()
        .oneshot(
            Request::post("/api/incident-pilot/settings/update")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), http::StatusCode::UNAUTHORIZED);

    let invalid = app
        .clone()
        .oneshot(
            Request::post("/api/incident-pilot/settings/update")
                .header("authorization", "Bearer incident-admin")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"settings":{
                        "notifyWarnings":false,
                        "duplicateWindowMinutes":0,
                        "retentionDays":120,
                        "unknownWritesBlockChat":true
                    }})
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), http::StatusCode::BAD_REQUEST);

    let saved = app
        .oneshot(
            Request::post("/api/incident-pilot/settings/update")
                .header("authorization", "Bearer incident-admin")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(saved.status(), http::StatusCode::OK);
    let current = state.incident.as_ref().unwrap().settings().unwrap();
    assert!(current.enabled);
    assert!(current.graduated);
    assert!(!current.notify_warnings);
    assert_eq!(current.duplicate_window_minutes, 25);
    assert_eq!(current.retention_days, 120);
    assert!(current.unknown_writes_block_chat);
    let persisted = state
        .store
        .runtime_setting("incident_pilot")
        .unwrap()
        .unwrap();
    assert_eq!(persisted["enabled"], true);
    assert_eq!(persisted["graduated"], true);
    assert_eq!(persisted["duplicateWindowMinutes"], 25);
    assert_eq!(persisted["retentionDays"], 120);
}

#[tokio::test]
async fn memory_handoff_archive_requires_admin_auth_and_explicit_clear_confirmation() {
    let (_directory, state) = test_state("memory-admin");
    state
        .store
        .save_conversation_handoff(
            "group:98765",
            &rinbot::application::handoff::ConversationHandoff {
                topic: "受保护的交接主题".into(),
                summary: "后续待核实".into(),
                last_reply: "已确认发送".into(),
                updated_at: 1_000,
                expires_at: 4_000_000_000_000,
                ..Default::default()
            },
        )
        .unwrap();
    let app = router(state.clone());
    let unauthorized = app
        .clone()
        .oneshot(
            Request::get("/api/memory/handoffs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), http::StatusCode::UNAUTHORIZED);

    let (status, listed) = json_response(
        app.clone(),
        Request::get("/api/memory/handoffs")
            .header("authorization", "Bearer memory-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(listed["items"][0]["chatKey"], "group:98765");
    assert_eq!(listed["items"][0]["handoff"]["topic"], "受保护的交接主题");

    let unauthorized_update = app
        .clone()
        .oneshot(
            Request::put("/api/memory/handoffs/group:98765")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"expectedUpdatedAt":1000,"topic":"未授权"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized_update.status(), http::StatusCode::UNAUTHORIZED);

    let stale_update = app
        .clone()
        .oneshot(
            Request::put("/api/memory/handoffs/group:98765")
                .header("authorization", "Bearer memory-admin")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"expectedUpdatedAt":999,"topic":"旧编辑器"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stale_update.status(), http::StatusCode::CONFLICT);

    let updated = app
        .clone()
        .oneshot(
            Request::put("/api/memory/handoffs/group:98765")
                .header("authorization", "Bearer memory-admin")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "expectedUpdatedAt":1000,
                        "topic":"  新话题  ",
                        "summary":"已核实\n继续下一步",
                        "facts":["重复事实","重复事实"],
                        "ttlMinutes":30
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(updated.status(), http::StatusCode::OK);
    let saved = state
        .store
        .conversation_handoff("group:98765", 0)
        .unwrap()
        .unwrap();
    assert_eq!(saved.topic, "新话题");
    assert_eq!(saved.summary, "已核实 继续下一步");
    assert_eq!(saved.facts, vec!["重复事实"]);
    assert_eq!(saved.last_reply, "已确认发送");
    assert!(saved.expires_at - saved.updated_at <= 30 * 60_000);

    let not_confirmed = app
        .clone()
        .oneshot(
            Request::delete("/api/memory/handoffs/group:98765")
                .header("authorization", "Bearer memory-admin")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"confirm":false}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(not_confirmed.status(), http::StatusCode::CONFLICT);
    assert_eq!(state.store.conversation_handoff_count().unwrap(), 1);

    let cleared = app
        .clone()
        .oneshot(
            Request::delete("/api/memory/handoffs/group:98765")
                .header("authorization", "Bearer memory-admin")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"confirm":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cleared.status(), http::StatusCode::OK);
    assert_eq!(state.store.conversation_handoff_count().unwrap(), 0);
}

#[tokio::test]
async fn incident_notification_retry_requires_auth_confirmation_and_rejects_unknown() {
    let (_directory, mut state) = test_state("incident-admin");
    let incidents = state.incident.as_ref().unwrap();
    let failed = incidents
        .capture(&rinbot::incident::IncidentInput {
            code: "FAILED_ALERT".into(),
            category: "internal".into(),
            severity: "critical".into(),
            source: "api-test".into(),
            chat_key: "group:12345".into(),
            session_id: String::new(),
            operation_id: String::new(),
            message: "definite delivery failure".into(),
            details: json!({}),
            now_ms: 1_800_000_010_000,
        })
        .unwrap()
        .unwrap();
    incidents.begin_notification(&failed.id).unwrap().unwrap();
    incidents
        .finish_notification(&failed.id, "failed", "not connected", 1_800_000_010_001)
        .unwrap();
    let unknown = incidents
        .capture(&rinbot::incident::IncidentInput {
            code: "UNKNOWN_ALERT".into(),
            category: "internal".into(),
            severity: "critical".into(),
            source: "api-test".into(),
            chat_key: "group:12345".into(),
            session_id: String::new(),
            operation_id: String::new(),
            message: "ambiguous delivery".into(),
            details: json!({}),
            now_ms: 1_800_000_020_000,
        })
        .unwrap()
        .unwrap();
    incidents.begin_notification(&unknown.id).unwrap().unwrap();
    incidents
        .finish_notification(&unknown.id, "unknown", "timeout", 1_800_000_020_001)
        .unwrap();
    let sends = Arc::new(AtomicUsize::new(0));
    state.incident_notification_executor = Some(Arc::new(
        rinbot::application::incident_notification::IncidentNotificationExecutor::new(
            incidents.clone(),
            Arc::new(IncidentMessageGateway(sends.clone())),
            "998877".into(),
            true,
        ),
    ));
    let app = router(state);

    let unauthorized = app
        .clone()
        .oneshot(
            Request::post(format!("/api/incidents/{}/notification/retry", failed.id))
                .header("content-type", "application/json")
                .body(Body::from(json!({"confirm":true}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), http::StatusCode::UNAUTHORIZED);
    let unconfirmed = app
        .clone()
        .oneshot(
            Request::post(format!("/api/incidents/{}/notification/retry", failed.id))
                .header("authorization", "Bearer incident-admin")
                .header("content-type", "application/json")
                .body(Body::from(json!({"confirm":false}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unconfirmed.status(), http::StatusCode::CONFLICT);
    let retried = app
        .clone()
        .oneshot(
            Request::post(format!("/api/incidents/{}/notification/retry", failed.id))
                .header("authorization", "Bearer incident-admin")
                .header("content-type", "application/json")
                .body(Body::from(json!({"confirm":true}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(retried.status(), http::StatusCode::OK);
    assert_eq!(sends.load(Ordering::SeqCst), 1);
    let unknown_retry = app
        .oneshot(
            Request::post(format!("/api/incidents/{}/notification/retry", unknown.id))
                .header("authorization", "Bearer incident-admin")
                .header("content-type", "application/json")
                .body(Body::from(json!({"confirm":true}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unknown_retry.status(), http::StatusCode::CONFLICT);
    assert_eq!(sends.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn incident_chat_controls_require_admin_auth_and_versioned_confirmation() {
    let (_directory, mut state) = test_state("incident-admin");
    let (wake_sender, mut wake_receiver) = tokio::sync::mpsc::channel(4);
    state.inbox_sender = Some(wake_sender);
    let backlog_id = state
        .store
        .insert_incoming(&IncomingMessage {
            message_id: "chat-control-backlog".into(),
            conversation_id: "group:12345".into(),
            sender_id: "67890".into(),
            sender_name: "测试用户".into(),
            is_group: true,
            is_self: false,
            segments: vec![MessageSegment::Text {
                text: "暂停期间消息".into(),
            }],
            timestamp: 1_800_000_000_000,
        })
        .unwrap()
        .unwrap();
    assert!(state
        .store
        .claim_inbox(backlog_id, "held-before-control", 30_000)
        .unwrap());
    assert!(state
        .store
        .complete_inbox(
            backlog_id,
            "held-before-control",
            "held",
            Some("manual block")
        )
        .unwrap());
    state
        .store
        .set_emergency_chat_block(
            "group:12345",
            "机器人已退出该群",
            "onebot-group-decrease",
            1_800_000_000_001,
        )
        .unwrap();
    let app = router(state.clone());
    let path = "/api/incidents/chat-controls/group:12345";
    let (_, initial) = json_response(
        app.clone(),
        Request::get(path)
            .header("authorization", "Bearer incident-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(initial["control"]["mode"], "blocked");
    assert_eq!(initial["decision"]["reason"], "机器人已退出该群");
    assert_eq!(initial["control"]["version"], 0);

    let unauthorized = app
        .clone()
        .oneshot(
            Request::put(path)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"mode":"blocked","expectedVersion":0}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), http::StatusCode::UNAUTHORIZED);

    let blocked = app.clone().oneshot(Request::put(path)
        .header("authorization", "Bearer incident-admin").header("content-type", "application/json")
        .body(Body::from(json!({"mode":"blocked","reason":"维护中","expectedVersion":0,"backlogAction":"keep"}).to_string())).unwrap()).await.unwrap();
    assert_eq!(blocked.status(), http::StatusCode::OK);
    let stale = app
        .clone()
        .oneshot(
            Request::put(path)
                .header("authorization", "Bearer incident-admin")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"mode":"auto","expectedVersion":0}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stale.status(), http::StatusCode::CONFLICT);
    let unconfirmed = app
        .clone()
        .oneshot(
            Request::put(path)
                .header("authorization", "Bearer incident-admin")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"mode":"continue","expectedVersion":1,"backlogAction":"keep"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unconfirmed.status(), http::StatusCode::CONFLICT);
    let continued = app.oneshot(Request::put(path)
        .header("authorization", "Bearer incident-admin").header("content-type", "application/json")
        .body(Body::from(json!({"mode":"continue","expectedVersion":1,"backlogAction":"recent","confirm":true}).to_string())).unwrap()).await.unwrap();
    assert_eq!(continued.status(), http::StatusCode::OK);
    assert_eq!(
        state
            .store
            .emergency_chat_block_reason("group:12345")
            .unwrap(),
        None
    );
    assert!(
        matches!(wake_receiver.recv().await, Some(rinbot::application::router::WakeTarget::Conversation(ref key)) if key == "group:12345")
    );
    assert_eq!(
        state
            .store
            .inbox_tasks(10, 0)
            .unwrap()
            .iter()
            .find(|task| task.id == backlog_id)
            .unwrap()
            .state,
        "queued"
    );
}

#[tokio::test]
async fn model_settings_keep_keys_private_bound_to_endpoint_and_live() {
    let (_directory, state) = test_state("model-admin");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            &state.config.data_dir,
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    let live_handle = state.runtime.model_settings_handle();
    let app = router(state.clone());
    let settings = json!({
        "public": {
            "base_url":"https://model.example.invalid/v1",
            "model":"test-model",
            "system_prompt":"优先使用中文回答。",
            "temperature":0.6,
            "max_tokens":2048,
            "max_tool_rounds":7,
            "timeout_ms":60000,
            "fallback":{
                "enabled":true,
                "base_url":"https://backup.example.invalid/v1",
                "model":"backup-model",
                "timeout_ms":45000
            },
            "vision_model":{
                "enabled":true,
                "base_url":"https://vision.example.invalid/v1",
                "model":"vision-model",
                "timeout_ms":55000
            }
        },
        "api_key":"private-model-key",
        "clear_api_key":false,
        "fallback_api_key":"private-fallback-key",
        "clear_fallback_api_key":false,
        "vision_api_key":"private-vision-key",
        "clear_vision_api_key":false
    });
    let unauthorized = app
        .clone()
        .oneshot(
            Request::post("/api/settings/model/update")
                .header("content-type", "application/json")
                .body(Body::from(settings.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), http::StatusCode::UNAUTHORIZED);

    let invalid = json!({
        "public": {"base_url":"https://user:pass@model.example.invalid/v1","model":"m","system_prompt":"p","temperature":0.5,"max_tokens":512,"timeout_ms":60000},
        "api_key":"private-model-key","clear_api_key":false
    });
    let rejected = app
        .clone()
        .oneshot(
            Request::post("/api/settings/model/update")
                .header("authorization", "Bearer model-admin")
                .header("content-type", "application/json")
                .body(Body::from(invalid.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status(), http::StatusCode::BAD_REQUEST);

    let (status, response) = json_response(
        app.clone(),
        Request::post("/api/settings/model/update")
            .header("authorization", "Bearer model-admin")
            .header("content-type", "application/json")
            .body(Body::from(settings.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK, "{response}");
    assert_eq!(response["api_key_configured"], true);
    assert_eq!(response["fallback_api_key_configured"], true);
    assert_eq!(response["vision_api_key_configured"], true);
    assert!(!response.to_string().contains("private-model-key"));
    assert!(!response.to_string().contains("private-fallback-key"));
    assert!(!response.to_string().contains("private-vision-key"));
    assert_eq!(
        live_handle.read().unwrap().base_url,
        "https://model.example.invalid/v1"
    );
    assert_eq!(live_handle.read().unwrap().max_tool_rounds, 7);
    assert_eq!(live_handle.read().unwrap().api_key, "private-model-key");
    let live_fallback = live_handle.read().unwrap().fallback.clone().unwrap();
    assert!(live_fallback.enabled);
    assert_eq!(live_fallback.model, "backup-model");
    assert_eq!(live_fallback.api_key, "private-fallback-key");
    let live_vision = live_handle.read().unwrap().vision_model.clone().unwrap();
    assert!(live_vision.enabled);
    assert_eq!(live_vision.model, "vision-model");
    assert_eq!(live_vision.api_key, "private-vision-key");
    let persisted = state.store.runtime_setting("model").unwrap().unwrap();
    assert_eq!(persisted["max_tool_rounds"], 7);
    assert!(!persisted.to_string().contains("private-model-key"));
    assert!(!persisted.to_string().contains("private-fallback-key"));
    assert!(!persisted.to_string().contains("private-vision-key"));

    let credentials =
        rinbot::application::model_settings::ModelCredentialStore::new(&state.config.data_dir);
    assert_eq!(
        credentials
            .key_for("https://model.example.invalid/v1")
            .unwrap()
            .as_deref(),
        Some("private-model-key")
    );
    let fallback_credentials =
        rinbot::application::model_settings::ModelCredentialStore::new_fallback(
            &state.config.data_dir,
        );
    assert_eq!(
        fallback_credentials
            .key_for("https://backup.example.invalid/v1")
            .unwrap()
            .as_deref(),
        Some("private-fallback-key")
    );
    let vision_credentials = rinbot::application::model_settings::ModelCredentialStore::new_vision(
        &state.config.data_dir,
    );
    assert_eq!(
        vision_credentials
            .key_for("https://vision.example.invalid/v1")
            .unwrap()
            .as_deref(),
        Some("private-vision-key")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(
                state
                    .config
                    .data_dir
                    .join("model-fallback-credentials.json")
            )
            .unwrap()
            .permissions()
            .mode()
                & 0o777,
            0o600
        );
    }
    let (_, fetched) = json_response(
        app.clone(),
        Request::get("/api/settings/model")
            .header("authorization", "Bearer model-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(fetched["settings"]["model"], "test-model");
    assert_eq!(fetched["api_key_configured"], true);
    assert_eq!(fetched["settings"]["fallback"]["enabled"], true);
    assert_eq!(fetched["fallback_api_key_configured"], true);
    assert_eq!(fetched["settings"]["vision_model"]["enabled"], true);
    assert_eq!(fetched["vision_api_key_configured"], true);
    assert!(!fetched.to_string().contains("private-model-key"));
    assert!(!fetched.to_string().contains("private-vision-key"));

    let changed_host_without_key = json!({
        "public": {"base_url":"https://other.example.invalid/v1","model":"other","system_prompt":"p","temperature":0.5,"max_tokens":512,"timeout_ms":60000},
        "clear_api_key":false
    });
    let rejected = app
        .oneshot(
            Request::post("/api/settings/model/update")
                .header("authorization", "Bearer model-admin")
                .header("content-type", "application/json")
                .body(Body::from(changed_host_without_key.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status(), http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn sticker_archive_api_requires_admin_auth_and_never_returns_media_urls() {
    let (directory, state) = test_state("sticker-admin");
    let archive = directory.path().join("sticker-import.json");
    std::fs::write(
        &archive,
        r#"[{"id":"sticker-1","desc":"开心表情","localNote":"群聊用","tags":["开心"],"url":"https://private.example.invalid/image?token=secret","useCount":4}]"#,
    )
    .unwrap();
    let mut stickers =
        rinbot::stickers::StickerStore::open(directory.path().join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
    stickers.import_node_archive(&archive).unwrap();
    drop(stickers);
    let app = router(state);

    let (status, _) = json_response(
        app.clone(),
        Request::get("/api/stickers?q=开心&limit=10&offset=0")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::UNAUTHORIZED);

    let (status, response) = json_response(
        app,
        Request::get("/api/stickers?q=开心&limit=10&offset=0")
            .header("authorization", "Bearer sticker-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(response["items"][0]["description"], "开心表情");
    assert_eq!(response["items"][0]["useCount"], 4.0);
    assert_eq!(response["runtimeReady"], false);
    assert_eq!(response["readOnly"], true);
    assert!(!response.to_string().contains("private.example.invalid"));
    assert!(!response.to_string().contains("token=secret"));
}

#[tokio::test]
async fn sticker_archive_without_imported_data_stays_empty_and_does_not_create_a_store() {
    let (directory, state) = test_state("sticker-empty-admin");
    let database = directory.path().join("stickers.sqlite");
    let app = router(state);
    let (status, response) = json_response(
        app,
        Request::get("/api/stickers")
            .header("authorization", "Bearer sticker-empty-admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(response["items"], json!([]));
    assert_eq!(response["source"], "no_imported_sticker_archive");
    assert_eq!(response["runtimeReady"], false);
    assert!(!database.exists());
}

#[tokio::test]
async fn sticker_operator_api_creates_edits_previews_and_removes_only_local_records() {
    let (directory, state) = test_state("sticker-operator-admin");
    let database = directory.path().join("stickers.sqlite");
    let mut store = rinbot::stickers::StickerStore::open(&database, true)
        .unwrap()
        .unwrap();
    let archive = directory.path().join("qq-sticker-seed.json");
    std::fs::write(
        &archive,
        r#"[{"id":"qq-entry","source":"qq","desc":"QQ 收藏"}]"#,
    )
    .unwrap();
    store.import_node_archive(&archive).unwrap();
    drop(store);
    let app = router(state);
    let create_body = json!({
        "imageDataUrl":"data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==",
        "desc":"初始说明", "localNote":"本地备注", "tags":["测试"], "usage":"聊天"
    })
    .to_string();

    let (status, _) = json_response(
        app.clone(),
        Request::post("/api/stickers")
            .header("content-type", "application/json")
            .body(Body::from(create_body.clone()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::UNAUTHORIZED);

    let (status, created) = json_response(
        app.clone(),
        Request::post("/api/stickers")
            .header("authorization", "Bearer sticker-operator-admin")
            .header("content-type", "application/json")
            .body(Body::from(create_body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::CREATED);
    assert_eq!(created["entry"]["source"], "manual");
    assert_eq!(created["entry"]["hasImage"], true);
    let manual_id = created["entry"]["id"].as_str().unwrap().to_owned();

    let (status, _) = json_response(
        app.clone(),
        Request::put(format!("/api/stickers/{manual_id}"))
            .header("authorization", "Bearer sticker-operator-admin")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"desc":"已修改","tags":["更新","中文"]}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);

    let image_response = app
        .clone()
        .oneshot(
            Request::get(format!("/api/stickers/{manual_id}/image"))
                .header("authorization", "Bearer sticker-operator-admin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(image_response.status(), http::StatusCode::OK);
    assert_eq!(
        image_response.headers()[http::header::CONTENT_TYPE],
        "image/gif"
    );
    assert!(!axum::body::to_bytes(image_response.into_body(), 1024)
        .await
        .unwrap()
        .is_empty());

    let (status, no_confirmation) = json_response(
        app.clone(),
        Request::delete("/api/stickers/qq-entry")
            .header("authorization", "Bearer sticker-operator-admin")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"confirm":false}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::CONFLICT);
    assert!(no_confirmation["error"].as_str().unwrap().contains("确认"));

    let (status, hidden) = json_response(
        app.clone(),
        Request::delete("/api/stickers/qq-entry")
            .header("authorization", "Bearer sticker-operator-admin")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"confirm":true}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(hidden["hidden"], true);
    assert_eq!(hidden["removed"], true);
    let verify = rinbot::stickers::StickerStore::open(&database, false)
        .unwrap()
        .unwrap();
    assert!(verify.get_by_id(&"qq-entry").unwrap().unwrap().hidden);
    assert!(verify.get_by_id(&manual_id).unwrap().is_some());
    drop(verify);

    let (status, removed) = json_response(
        app,
        Request::delete(format!("/api/stickers/{manual_id}"))
            .header("authorization", "Bearer sticker-operator-admin")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"confirm":true}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(removed["hidden"], false);
    assert!(!directory
        .path()
        .join("sticker-assets")
        .join(format!("{manual_id}.gif"))
        .exists());
}

#[tokio::test]
async fn qq_sticker_sync_requires_auth_confirmation_and_runtime_readiness() {
    let (directory, mut state) = test_state("sticker-sync-admin");
    let archive = directory.path().join("sticker-sync-seed.json");
    std::fs::write(&archive, r#"[{"id":"old","source":"qq"}]"#).unwrap();
    let mut store =
        rinbot::stickers::StickerStore::open(directory.path().join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
    store.import_node_archive(&archive).unwrap();
    drop(store);
    let gateway = Arc::new(FakeStickerSyncGateway {
        calls: AtomicUsize::new(0),
        response: vec![json!({
            "emoji_id":"new",
            "url":"https://p.qpic.cn/qq_expression/resource"
        })],
    });
    state.sticker_gateway = Some(gateway.clone());
    let app = router(state.clone());

    let (status, _) = json_response(
        app.clone(),
        Request::post("/api/stickers/sync")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"confirm":true}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::UNAUTHORIZED);
    let (status, _) = json_response(
        app.clone(),
        Request::post("/api/stickers/sync")
            .header("authorization", "Bearer sticker-sync-admin")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"confirm":true}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::CONFLICT);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);

    state.sticker_runtime_ready = true;
    let app_disabled = router(state.clone());
    let (status, _) = json_response(
        app_disabled,
        Request::post("/api/stickers/sync")
            .header("authorization", "Bearer sticker-sync-admin")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"confirm":true}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::CONFLICT);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);

    state
        .runtime
        .set_sticker_settings(rinbot::stickers::StickerSettings {
            enabled: true,
            graduated: false,
            ..Default::default()
        });
    let app = router(state);
    let (status, _) = json_response(
        app.clone(),
        Request::post("/api/stickers/sync")
            .header("authorization", "Bearer sticker-sync-admin")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"confirm":false}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::BAD_REQUEST);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);

    let (status, response) = json_response(
        app,
        Request::post("/api/stickers/sync")
            .header("authorization", "Bearer sticker-sync-admin")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"confirm":true}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
    assert_eq!(response["summary"]["added"], 1);
    let synced =
        rinbot::stickers::StickerStore::open(directory.path().join("stickers.sqlite"), false)
            .unwrap()
            .unwrap();
    let fresh = synced.list("new", 10, 0).unwrap().remove(0);
    assert!(!fresh.sendable);
}

#[tokio::test]
async fn api_handler_panics_are_captured_without_exposing_panic_text() {
    let (directory, mut state) = test_state("");
    let app = Router::new()
        .route("/panic", get(panic_http_handler))
        .route(
            "/failure",
            get(|| async { http::StatusCode::INTERNAL_SERVER_ERROR }),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            capture_http_incident_errors,
        ));
    let response = app
        .clone()
        .oneshot(Request::get("/panic").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::INTERNAL_SERVER_ERROR);
    let incidents = state
        .incident
        .as_ref()
        .unwrap()
        .list(None, None, None, 10)
        .unwrap();
    let incident = incidents
        .iter()
        .find(|incident| incident.code == "HTTP_HANDLER_PANIC")
        .unwrap();
    assert_eq!(incident.category, "http");
    assert_eq!(incident.severity, "critical");
    assert_eq!(incident.details["method"], "GET");
    assert_eq!(incident.details["path"], "/panic");
    assert!(!incident.message.contains("private panic detail"));

    let response = app
        .oneshot(Request::get("/failure").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), http::StatusCode::INTERNAL_SERVER_ERROR);
    let incidents = state
        .incident
        .as_ref()
        .unwrap()
        .list(None, None, None, 10)
        .unwrap();
    assert!(incidents.iter().any(|incident| {
        incident.code == "HTTP_INTERNAL_ERROR" && incident.details["path"] == "/failure"
    }));

    let disabled_path = directory.path().join("disabled-incident.sqlite");
    state.incident = Some(Arc::new(
        rinbot::incident::IncidentStore::open(
            &disabled_path,
            rinbot::incident::IncidentSettings {
                enabled: false,
                graduated: false,
                notify_warnings: false,
                duplicate_window_minutes: 10,
                retention_days: 90,
                unknown_writes_block_chat: false,
            },
        )
        .unwrap(),
    ));
    let disabled_app = Router::new()
        .route(
            "/failure",
            get(|| async { http::StatusCode::INTERNAL_SERVER_ERROR }),
        )
        .layer(middleware::from_fn_with_state(
            state,
            capture_http_incident_errors,
        ));
    let response = disabled_app
        .oneshot(Request::get("/failure").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), http::StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        !disabled_path.exists(),
        "disabled incident module is side-effect free"
    );
}

async fn panic_http_handler() -> axum::response::Response {
    panic!("private panic detail must not be exposed")
}
