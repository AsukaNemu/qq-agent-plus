use std::{
    net::SocketAddr,
    process::{Child, Command, Stdio},
    time::Duration,
};

use axum::{
    extract::{Path, State},
    routing::post,
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use rinbot::{
    adapters::sqlite::SqliteStore,
    domain::message::{IncomingMessage, MessageSegment},
    runtime::config::{ImportedNodeRuntimeConfig, IMPORTED_CONFIG_FILE},
};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot},
    time::{sleep, timeout},
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

#[derive(Clone)]
struct OneBotMock {
    sent: mpsc::UnboundedSender<Value>,
    group_decisions: mpsc::UnboundedSender<Value>,
}

async fn onebot_action(
    Path(action): Path<String>,
    State(state): State<OneBotMock>,
    Json(body): Json<Value>,
) -> Json<Value> {
    match action.as_str() {
        "get_login_info" => Json(json!({
            "status":"ok", "retcode":0,
            "data":{"user_id":"900", "nickname":"RInBot"}
        })),
        "get_friend_list" => Json(json!({
            "status":"ok", "retcode":0,
            "data":[{"user_id":"400","nickname":"同步好友"}]
        })),
        "send_private_msg" => {
            let _ = state.sent.send(body);
            Json(json!({"status":"ok", "retcode":0, "data":{"message_id":9001}}))
        }
        "set_group_add_request" => {
            let _ = state.group_decisions.send(body);
            Json(json!({"status":"ok", "retcode":0, "data":{}}))
        }
        _ => Json(json!({"status":"failed", "retcode":100, "wording":"unsupported mock action"})),
    }
}

async fn model_completion(
    State(requests): State<mpsc::UnboundedSender<Value>>,
    Json(request): Json<Value>,
) -> Json<Value> {
    let _ = requests.send(request);
    Json(json!({
        "choices":[{"message":{"content":"本机集成测试回复"}}],
        "usage":{"prompt_tokens":7,"completion_tokens":5}
    }))
}

async fn free_address() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn wait_ready(client: &reqwest::Client, url: &str) -> Value {
    timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(response) = client.get(url).send().await {
                if response.status().is_success() {
                    return response.json().await.unwrap_or(Value::Null);
                }
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("RInBot did not become ready")
}

#[tokio::test]
async fn active_binary_processes_allowlisted_message_through_mock_model_and_napcat() {
    let temp: TempDir = tempfile::tempdir().unwrap();
    let ui_dir = temp.path().join("ui");
    std::fs::create_dir_all(&ui_dir).unwrap();
    std::fs::write(
        ui_dir.join("index.html"),
        "<!doctype html><title>isolated RInBot UI</title>",
    )
    .unwrap();

    let onebot_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let onebot_addr = onebot_listener.local_addr().unwrap();
    let (sent_tx, mut sent_rx) = mpsc::unbounded_channel();
    let (group_decision_tx, mut group_decision_rx) = mpsc::unbounded_channel();
    let onebot_app = Router::new()
        .route("/{action}", post(onebot_action))
        .with_state(OneBotMock {
            sent: sent_tx,
            group_decisions: group_decision_tx,
        });
    let onebot_server = tokio::spawn(async move {
        axum::serve(onebot_listener, onebot_app).await.unwrap();
    });

    let model_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_addr = model_listener.local_addr().unwrap();
    let (model_tx, mut model_rx) = mpsc::unbounded_channel();
    let model_app = Router::new()
        .route("/v1/chat/completions", post(model_completion))
        .with_state(model_tx);
    let model_server = tokio::spawn(async move {
        axum::serve(model_listener, model_app).await.unwrap();
    });

    let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_addr = ws_listener.local_addr().unwrap();
    let (connected_tx, connected_rx) = oneshot::channel();
    let (event_tx, event_rx) = oneshot::channel();
    let (group_settings_tx, group_settings_rx) = oneshot::channel();
    let ws_server = tokio::spawn(async move {
        let (stream, _) = ws_listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _ = connected_tx.send(());
        let _ = event_rx.await;
        for message_id in [701, 702] {
            websocket
                .send(Message::Text(
                    json!({
                        "post_type":"message", "message_type":"private", "self_id":900,
                        "message_id":message_id, "user_id":100, "time":1791178000,
                        "sender":{"user_id":100,"nickname":"test"},
                        "message":[{"type":"text","data":{"text":"你好"}}]
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            sleep(Duration::from_millis(20)).await;
        }
        websocket
            .send(Message::Text(
                json!({
                    "post_type":"message", "message_type":"group", "self_id":900,
                    "message_id":704, "group_id":300, "user_id":555, "time":1791178000,
                    "sender":{"user_id":555,"nickname":"屏蔽用户"},
                    "message":[{"type":"text","data":{"text":"屏蔽消息不得落库"}}]
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "post_type":"message", "message_type":"group", "self_id":900,
                    "message_id":703, "group_id":300, "user_id":900, "time":1791178000,
                    "sender":{"user_id":900,"nickname":"RInBot"},
                    "message":[{"type":"text","data":{"text":"自身回声不建立人物档案"}}]
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "post_type":"request", "request_type":"friend", "user_id":200,
                    "flag":"onebot-private-request-capability", "comment":"你好，请加好友"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        sleep(Duration::from_millis(100)).await;
        websocket
            .send(Message::Text(
                json!({
                    "post_type":"notice", "notice_type":"friend_add", "user_id":200
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "post_type":"request", "request_type":"group", "sub_type":"invite",
                    "group_id":301, "user_id":100, "time":1791178000,
                    "flag":"isolated-group-invite-capability", "comment":"集成测试群邀请"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let _ = group_settings_rx.await;
        websocket
            .send(Message::Text(
                json!({
                    "post_type":"notice", "notice_type":"group_increase",
                    "group_id":302, "user_id":900, "operator_id":100, "time":1791178001
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "post_type":"request", "request_type":"group", "sub_type":"invite",
                    "group_id":304, "user_id":777, "time":1791178003,
                    "flag":"dynamic-friends-only-capability", "comment":"热更新好友限制"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "post_type":"notice", "notice_type":"group_increase",
                    "group_id":303, "user_id":900, "operator_id":100, "time":1791178002
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        // Keep the WS open until the test has observed the send; otherwise reconnects
        // would produce duplicate fixture events and obscure the one-event assertion.
        let mut sink = websocket;
        let _ = sink.next().await;
    });

    let app_addr = free_address().await;
    let app_url = format!("http://{app_addr}");
    let app_port = app_addr.port().to_string();
    let ws_url = format!("ws://{ws_addr}/onebot/v11/ws");
    let onebot_http_url = format!("http://{onebot_addr}");
    let model_base_url = format!("http://{model_addr}/v1");
    let data_dir = temp.path().join("runtime-data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let mut identity_history =
        rinbot::identity::IdentityStore::open(data_dir.join("identity.sqlite"))
            .expect("create preexisting identity history");
    identity_history
        .record_request("300", "startup-replay-capability", "startup pending", 20, 1)
        .expect("persist a never-attempted owner notification before startup");
    drop(identity_history);
    let core_history = SqliteStore::open(data_dir.join("rinbot.sqlite"))
        .expect("create preexisting core message history");
    let seeded_inbox = core_history
        .insert_incoming(&IncomingMessage {
            message_id: "persisted-before-crash".into(),
            conversation_id: "private:400".into(),
            sender_id: "400".into(),
            sender_name: "历史用户".into(),
            is_group: false,
            is_self: false,
            segments: vec![MessageSegment::Text {
                text: "进程在索引前退出".into(),
            }],
            timestamp: 1_791_178_000_000,
        })
        .expect("persist message before identity indexing")
        .expect("new seeded message id");
    assert!(core_history
        .claim_inbox(seeded_inbox, "seed-observed", 30_000)
        .expect("claim seeded historical message"));
    core_history
        .complete_inbox(seeded_inbox, "seed-observed", "observed", None)
        .expect("leave seeded message durably observed by core");
    let imported_config = ImportedNodeRuntimeConfig {
        schema_version: 1,
        unmigrated_enabled_features: Some(Vec::new()),
        enabled_feature_data_missing: Some(Vec::new()),
        identity_pilot_enabled: Some(true),
        memory_handoff_enabled: Some(true),
        multimodal_context_enabled: Some(false),
        multimodal_context_graduated: Some(false),
        memory_consolidate_enabled: Some(false),
        memory_consolidate_min_impressions: None,
        memory_consolidate_max_per_member: None,
        memory_consolidate_discover_min_messages: None,
        memory_consolidate_discover_max_members: None,
        memory_consolidate_min_interval_ms: Some(21_600_000),
        memory_handoff_ttl_minutes: Some(1440),
        memory_handoff_max_chars: Some(2_000),
        identity_pilot_graduated: Some(true),
        incoming_friend_request_enabled: Some(true),
        incoming_friend_auto_whitelist: Some(true),
        incoming_friend_max_pending: Some(20),
        group_invite_enabled: Some(true),
        // Node gates both request-approved and direct-entry whitelist updates.
        group_invite_auto_whitelist: Some(false),
        group_invite_friends_only: Some(true),
        group_invite_notify_owner: Some(true),
        group_invite_owner_uin: Some("998877".into()),
        web_search_enabled: Some(false),
        web_search_provider: Some("bing".into()),
        web_search_url: Some("https://cn.bing.com/search".into()),
        web_search_max_results: Some(6),
        identity_owner_uin: Some("998877".into()),
        admin_owner_uin: Some("998877".into()),
        incident_pilot_enabled: Some(true),
        incident_pilot_graduated: Some(true),
        incident_owner_uin: Some("998877".into()),
        incident_notify_warnings: Some(true),
        incident_duplicate_window_minutes: Some(10),
        incident_unknown_writes_block_chat: Some(false),
        incident_retention_days: Some(90),
        relationship_pilot: None,
        qzone_interactions: None,
        proactive: None,
        daily_moments: None,
        daily_moments_persona: None,
        sticker_enabled: Some(false),
        sticker_collect_enabled: Some(false),
        sticker_auto_collect_enabled: Some(false),
        sticker_save_to_qq_favorites: Some(false),
        sticker_max_collect_per_hour: Some(10),
        sticker_prompt_max_stickers: Some(10),
        sticker_encourage: Some(1),
        send_max_per_minute: Some(80),
        send_max_per_hour: Some(500),
        send_min_gap_ms: Some(1_000),
        send_max_gap_ms: Some(3_000),
        send_by_length_ms: Some(20),
        send_hard_split_at: Some(4_000),
        blocklist: Some([("300".into(), vec!["555".into()])].into_iter().collect()),
        unified_tier: Some(true),
        group_random_percent: Some(Default::default()),
        onebot_ws_url: Some(ws_url.clone()),
        onebot_http_url: Some(onebot_http_url.clone()),
        onebot_token: Some(String::new()),
        model_base_url: Some(model_base_url.clone()),
        model_id: Some("mock-model-from-import".into()),
        model_api_key: Some("fixture-key-only".into()),
        model_timeout_ms: Some(30_000),
        model_fallback: None,
        model_fallback_api_key: None,
        model_vision: None,
        model_vision_api_key: None,
        asr: None,
        asr_api_key: None,
        system_prompt: Some("导入配置中的测试角色".into()),
        allow_private: Some("100,400".into()),
        allow_groups: Some("300".into()),
        deny_private: Some(String::new()),
        deny_groups: Some("303".into()),
        random_response_percent: Some(100.0),
        trigger_keywords: Some(String::new()),
        at_context_count: Some(12),
        keyword_context_count: Some(8),
        random_context_count: Some(5),
        all_context_count: Some(20),
        wake_delay_min_ms: Some(100),
        wake_delay_max_ms: Some(100),
        max_batch_wait_ms: Some(5000),
        batch_limit: Some(10),
        batch_max_chars: Some(12_000),
        max_concurrent_runs: Some(2),
        time_control_json: Some(
            r#"{"enabled":false,"schedule":{"mode":"always","windows":[]},"overrides":{}}"#.into(),
        ),
        model_temperature: Some(0.4),
        model_max_tokens: Some(128),
        model_max_rounds: Some(7),
    };
    std::fs::write(
        data_dir.join(IMPORTED_CONFIG_FILE),
        serde_json::to_vec(&imported_config).unwrap(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            data_dir.join(IMPORTED_CONFIG_FILE),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    let child = Command::new(env!("CARGO_BIN_EXE_RInBot"))
        .args([
            "--host",
            "127.0.0.1",
            "--port",
            &app_port,
            "--data-dir",
            data_dir.to_str().unwrap(),
            "--ui-dir",
            ui_dir.to_str().unwrap(),
            "--active",
            "--connect-onebot",
            "--api-token",
            "active-runtime-admin-token",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn RInBot integration process");
    let mut child = ChildGuard(child);

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        reqwest::header::HeaderValue::from_static("Bearer active-runtime-admin-token"),
    );
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap();
    let status = wait_ready(&client, &format!("{app_url}/api/health")).await;
    assert_eq!(status["ok"], true);
    let frontend = client.get(&app_url).send().await.unwrap();
    assert_eq!(frontend.status(), reqwest::StatusCode::OK);
    assert!(frontend
        .text()
        .await
        .unwrap()
        .contains("isolated RInBot UI"));
    let proactive_status: serde_json::Value = client
        .get(format!("{app_url}/api/proactive/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(proactive_status["runtimeReady"], true);
    assert_eq!(proactive_status["exists"], true);
    let imported_model_settings: serde_json::Value = client
        .get(format!("{app_url}/api/settings/model"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(imported_model_settings["settings"]["max_tool_rounds"], 7);
    timeout(Duration::from_secs(5), connected_rx)
        .await
        .expect("NapCat websocket did not connect")
        .expect("websocket signal dropped");
    let _ = event_tx.send(());

    let result = timeout(Duration::from_secs(8), async {
        let request = model_rx.recv().await.expect("model request missing");
        let mut owner_notices = Vec::new();
        loop {
            let sent = sent_rx.recv().await.expect("OneBot send missing");
            if sent["user_id"] == "100" {
                break (request, sent, owner_notices);
            }
            if sent["user_id"] == "998877" {
                owner_notices.push(sent);
            }
        }
    })
    .await;
    let result = match result {
        Ok(result) => result,
        Err(_) => {
            let tasks = client
                .get(format!("{app_url}/api/tasks?limit=20&offset=0"))
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap_or_default();
            let messages = client
                .get(format!("{app_url}/api/messages?limit=20&offset=0"))
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap_or_default();
            let status = client
                .get(format!("{app_url}/api/status"))
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap_or_default();
            panic!("active message pipeline did not finish; status={status}; tasks={tasks}; messages={messages}");
        }
    };
    assert_eq!(result.0["model"], "mock-model-from-import");
    let user_contents = result.0["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "user")
        .map(|message| message["content"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(user_contents, vec!["你好", "你好"]);
    assert_eq!(result.0["messages"][0]["content"], "导入配置中的测试角色");
    assert_eq!(result.1["user_id"], "100");
    assert_eq!(result.1["message"][0]["data"]["text"], "本机集成测试回复");
    let decision = timeout(Duration::from_secs(3), group_decision_rx.recv())
        .await
        .expect("group invitation decision timed out")
        .expect("mock NapCat action channel closed");
    assert_eq!(decision["sub_type"], "invite");
    assert_eq!(decision["approve"], true);
    assert_eq!(decision["flag"], "isolated-group-invite-capability");
    timeout(Duration::from_secs(2), async {
        loop {
            let status = client
                .get(format!("{app_url}/api/status"))
                .send()
                .await
                .expect("status endpoint request failed")
                .json::<Value>()
                .await
                .expect("status endpoint returned invalid JSON");
            if status["onebot_connected"] == true {
                break;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("RInBot status did not observe the connected OneBot adapter");
    let update = client
        .post(format!("{app_url}/api/group-invites/settings/update"))
        .json(&json!({"settings":{
            "enabled":true,
            "friendsOnly":false,
            "autoWhitelist":true,
            "notifyOwner":true
        }}))
        .send()
        .await
        .unwrap();
    assert_eq!(update.status(), reqwest::StatusCode::OK);
    assert_eq!(update.json::<Value>().await.unwrap()["runtimeReady"], true);
    let _ = group_settings_tx.send(());
    let dynamic_decision = timeout(Duration::from_secs(3), group_decision_rx.recv())
        .await
        .expect("dynamically eligible group invitation decision timed out")
        .expect("mock NapCat action channel closed");
    assert_eq!(dynamic_decision["sub_type"], "invite");
    assert_eq!(dynamic_decision["approve"], true);
    assert_eq!(dynamic_decision["flag"], "dynamic-friends-only-capability");
    let archived = client
        .get(format!("{app_url}/api/messages?limit=100&offset=0"))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert!(archived["messages"]
        .as_array()
        .unwrap()
        .iter()
        .all(|message| {
            message["text"] != "屏蔽消息不得落库" && message["sender_id"] != "555"
        }));
    sleep(Duration::from_millis(250)).await;
    assert!(
        model_rx.try_recv().is_err(),
        "a debounced batch must invoke the model once"
    );
    let mut owner_notices = result.2;
    while let Ok(sent) = sent_rx.try_recv() {
        if sent["user_id"] == "998877" {
            owner_notices.push(sent);
        } else {
            panic!("a debounced batch must send only one reply");
        }
    }
    assert_eq!(
        client
            .get(format!("{app_url}/api/outbox/unknown"))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["items"],
        json!([])
    );
    let identity_page = timeout(Duration::from_secs(3), async {
        loop {
            let response = client
                .get(format!("{app_url}/api/identity/incoming-friend-requests"))
                .send()
                .await
                .unwrap();
            let value = response.json::<Value>().await.unwrap();
            if value["total"] == 2 {
                break value;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("identity module did not persist the incoming friend request");
    assert_eq!(identity_page["source"], "rust_identity_module");
    let incoming_request = identity_page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["userId"] == "200")
        .expect("incoming friend request missing from API");
    assert_eq!(incoming_request["status"], "accepted");
    assert!(
        incoming_request["actionCompletedAt"]
            .as_i64()
            .unwrap_or_default()
            > 0
    );
    assert_eq!(incoming_request["whitelistApplied"], true);
    assert_eq!(incoming_request["comment"], "你好，请加好友");
    assert!(!identity_page
        .to_string()
        .contains("onebot-private-request-capability"));
    let behavior: Value = client
        .get(format!("{app_url}/api/settings/behavior"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(behavior["access"]["allow_private"]
        .as_array()
        .unwrap()
        .iter()
        .any(|id| id == "200"));
    let mut owner_notices = owner_notices;
    timeout(Duration::from_secs(3), async {
        while owner_notices.len() < 2 {
            let sent = sent_rx
                .recv()
                .await
                .expect("OneBot owner notification missing");
            if sent["user_id"] == "998877" {
                owner_notices.push(sent);
            }
        }
    })
    .await
    .expect("RInBot did not notify the configured owner for both records");
    let notice_texts = owner_notices
        .iter()
        .map(|notice| notice["message"][0]["data"]["text"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(notice_texts.iter().any(|text| text.contains("200")));
    assert!(notice_texts.iter().any(|text| text.contains("300")));
    assert!(owner_notices
        .iter()
        .all(|notice| !notice.to_string().contains("capability")));
    let notified_page = timeout(Duration::from_secs(3), async {
        loop {
            let response = client
                .get(format!("{app_url}/api/identity/incoming-friend-requests"))
                .send()
                .await
                .unwrap();
            let value = response.json::<Value>().await.unwrap();
            if value["items"]
                .as_array()
                .is_some_and(|items| items.iter().all(|item| item["notificationState"] == "sent"))
            {
                break value;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("notification state was not durably marked sent");
    assert!(notified_page["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["notificationMessageId"] == "9001"));

    let group_invites = timeout(Duration::from_secs(3), async {
        loop {
            let page = client
                .get(format!("{app_url}/api/group-invites?limit=20&offset=0"))
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap();
            let has_request = page["items"].as_array().is_some_and(|items| {
                items.iter().any(|item| item["groupId"] == "301")
                    && items.iter().any(|item| item["groupId"] == "304")
            });
            let has_direct_entry = page["directEntries"].as_array().is_some_and(|items| {
                items.iter().any(|item| item["groupId"] == "302")
                    && items.iter().any(|item| item["groupId"] == "303")
            });
            if has_request && has_direct_entry {
                break page;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("group invitation request and direct-entry ledger rows were not exposed");
    let invite = group_invites["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["groupId"] == "301")
        .unwrap();
    assert_eq!(invite["state"], "approved");
    assert_eq!(invite["whitelistState"], "not_required");
    assert_eq!(invite["notifyState"], "sent");
    assert_eq!(invite["notifyMessageId"], "9001");
    let dynamic_invite = group_invites["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["groupId"] == "304")
        .unwrap();
    assert_eq!(dynamic_invite["state"], "approved");
    assert_eq!(dynamic_invite["reviewReason"], "");
    let direct_entry = group_invites["directEntries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["groupId"] == "302")
        .unwrap();
    assert_eq!(direct_entry["whitelistState"], "applied");
    assert_eq!(direct_entry["whitelistError"], "");
    assert_eq!(direct_entry["notifyState"], "sent");
    assert_eq!(direct_entry["notifyMessageId"], "9001");
    let behavior: Value = client
        .get(format!("{app_url}/api/settings/behavior"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let allowed_groups = behavior["access"]["allow_groups"].as_array().unwrap();
    assert!(allowed_groups.iter().any(|id| id == "302"));
    assert!(allowed_groups.iter().any(|id| id == "304"));
    assert!(!allowed_groups.iter().any(|id| id == "301"));
    let denied_direct_entry = group_invites["directEntries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["groupId"] == "303")
        .unwrap();
    assert_eq!(denied_direct_entry["whitelistState"], "not_required");
    assert_eq!(denied_direct_entry["notifyState"], "not_required");
    assert!(!group_invites
        .to_string()
        .contains("isolated-group-invite-capability"));
    timeout(Duration::from_secs(3), async {
        while !owner_notices.iter().any(|notice| {
            notice["message"][0]["data"]["text"]
                .as_str()
                .is_some_and(|text| text.contains("群号：301"))
        }) || !owner_notices.iter().any(|notice| {
            notice["message"][0]["data"]["text"]
                .as_str()
                .is_some_and(|text| text.contains("群号：302"))
        }) {
            let sent = sent_rx.recv().await.expect("owner notification missing");
            if sent["user_id"] == "998877" {
                owner_notices.push(sent);
            }
        }
    })
    .await
    .expect("RInBot did not notify the owner about both group-entry paths");
    assert!(owner_notices.iter().any(|notice| {
        notice["message"][0]["data"]["text"]
            .as_str()
            .is_some_and(|text| text.contains("群号：302") && text.contains("已加入聊天白名单"))
    }));

    let people_page = client
        .get(format!("{app_url}/api/identity/people?limit=10&offset=0"))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(people_page["source"], "rust_identity_module");
    assert_eq!(people_page["total"], 3);
    let people = people_page["items"].as_array().unwrap();
    let live_person = people.iter().find(|person| person["uin"] == "100").unwrap();
    assert_eq!(live_person["primary_name"], "test");
    assert_eq!(live_person["message_count"], 2);
    let added_friend = people
        .iter()
        .find(|person| person["uin"] == "200")
        .expect("friend-add notice should add a person record");
    assert_eq!(added_friend["is_friend"], 1);
    let snapshot_friend = people
        .iter()
        .find(|person| person["uin"] == "400")
        .expect("startup friend snapshot should preserve the existing person record");
    assert_eq!(snapshot_friend["is_friend"], 1);
    assert_eq!(live_person["chat_count"], 1);
    let recovered_person = people.iter().find(|person| person["uin"] == "400").unwrap();
    assert_eq!(recovered_person["primary_name"], "同步好友");
    assert_eq!(recovered_person["message_count"], 1);

    let _ = child.0.kill();
    let _ = child.0.wait();
    onebot_server.abort();
    model_server.abort();
    ws_server.abort();
}

#[tokio::test]
async fn observe_binary_reconnects_to_mock_napcat_after_websocket_disconnect() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("reconnect-data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let (sent_tx, _sent_rx) = mpsc::unbounded_channel();
    let (group_decision_tx, _group_decision_rx) = mpsc::unbounded_channel();
    let http_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr = http_listener.local_addr().unwrap();
    let http_server = tokio::spawn(async move {
        let app = Router::new()
            .route("/{action}", post(onebot_action))
            .with_state(OneBotMock {
                sent: sent_tx,
                group_decisions: group_decision_tx,
            });
        axum::serve(http_listener, app).await.unwrap();
    });

    let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_addr = ws_listener.local_addr().unwrap();
    let (attempt_tx, mut attempt_rx) = mpsc::unbounded_channel();
    let ws_server = tokio::spawn(async move {
        for attempt in 1..=2 {
            let (stream, _) = ws_listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let _ = attempt_tx.send(attempt);
            if attempt == 1 {
                socket.send(Message::Close(None)).await.unwrap();
            } else {
                // Hold the recovered connection open until the test drops this task.
                let _ = socket.next().await;
            }
        }
    });

    let app_addr = free_address().await;
    let app_url = format!("http://{app_addr}");
    let ws_url = format!("ws://{ws_addr}/onebot/v11/ws");
    let child = Command::new(env!("CARGO_BIN_EXE_RInBot"))
        .args([
            "--host",
            "127.0.0.1",
            "--port",
            &app_addr.port().to_string(),
            "--data-dir",
            data_dir.to_str().unwrap(),
            "--ui-dir",
            "ui/dist",
            "--connect-onebot",
            "--onebot-ws-url",
            &ws_url,
            "--onebot-http-url",
            &format!("http://{http_addr}"),
            "--onebot-token",
            "isolated-reconnect-test",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn isolated observe-mode RInBot process");
    let mut child = ChildGuard(child);
    let client = reqwest::Client::new();
    let _ = wait_ready(&client, &format!("{app_url}/api/status")).await;

    assert_eq!(
        timeout(Duration::from_secs(5), attempt_rx.recv())
            .await
            .expect("first mock NapCat connection timed out"),
        Some(1)
    );
    assert_eq!(
        timeout(Duration::from_secs(10), attempt_rx.recv())
            .await
            .expect("RInBot did not retry the disconnected websocket"),
        Some(2)
    );
    let health = timeout(Duration::from_secs(3), async {
        loop {
            if let Ok(response) = client.get(format!("{app_url}/api/health")).send().await {
                if let Ok(body) = response.json::<Value>().await {
                    if body["onebot_connected"] == true {
                        break body;
                    }
                }
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("health state did not recover after websocket reconnect");
    assert_eq!(health["onebot_connected"], true);

    let _ = child.0.kill();
    let _ = child.0.wait();
    http_server.abort();
    ws_server.abort();
}
