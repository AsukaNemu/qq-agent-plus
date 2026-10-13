use std::{
    fs::OpenOptions,
    io::Write,
    net::SocketAddr,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{Path, State},
    routing::post,
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use futures_util::SinkExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::{
    net::TcpListener,
    sync::{oneshot, Notify},
    time::{sleep, timeout},
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

const REPLAY_COUNT: usize = 168;
const DEFAULT_REPLAY_DURATION_SECS: u64 = 1_800;
const DEFAULT_SAMPLE_INTERVAL_SECS: u64 = 10;

fn env_usize_or(name: &str, fallback: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(fallback)
}

fn env_u64_or(name: &str, fallback: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(fallback)
}

fn sample_count_for_duration(duration_secs: u64, interval_secs: u64) -> usize {
    assert!(interval_secs > 0, "sample interval must be positive");
    let whole_intervals = duration_secs / interval_secs;
    let intervals = whole_intervals
        + if duration_secs % interval_secs == 0 {
            0
        } else {
            1
        };
    usize::try_from(
        intervals
            .checked_add(1)
            .expect("sample count must not overflow"),
    )
    .expect("sample count must fit in memory")
}

fn validate_report_destination(
    report_path: &std::path::Path,
    temp_dir: &std::path::Path,
) -> Result<(), String> {
    if !report_path.is_absolute() {
        return Err("performance evidence path must be absolute".into());
    }
    let parent = report_path
        .parent()
        .ok_or_else(|| "performance evidence path must have a parent".to_owned())?;
    let name = report_path
        .file_name()
        .ok_or_else(|| "performance evidence path must name a file".to_owned())?;
    let canonical_parent = std::fs::canonicalize(parent)
        .map_err(|_| "performance evidence parent must exist and be canonicalizable".to_owned())?;
    let canonical_temp = std::fs::canonicalize(temp_dir)
        .map_err(|_| "temporary replay directory must be canonicalizable".to_owned())?;
    if canonical_parent.join(name).starts_with(canonical_temp) {
        return Err(
            "performance evidence path resolves inside the temporary replay directory".into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{sample_count_for_duration, validate_report_destination};

    #[test]
    fn sample_window_includes_both_endpoints() {
        assert_eq!(sample_count_for_duration(86_400, 60), 1_441);
        assert_eq!(sample_count_for_duration(1_800, 10), 181);
        assert_eq!(sample_count_for_duration(12, 10), 3);
    }

    #[cfg(unix)]
    #[test]
    fn report_destination_rejects_symlinked_parent_inside_temp_dir() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let alias = outside.path().join("replay-alias");
        std::os::unix::fs::symlink(temp.path(), &alias).unwrap();
        let report = alias.join("samples.json");
        assert!(validate_report_destination(&report, temp.path())
            .unwrap_err()
            .contains("resolves inside"));
    }
}

#[derive(Clone)]
struct MockState {
    model_requests: Arc<AtomicUsize>,
    primary_model_requests: Arc<AtomicUsize>,
    vision_model_requests: Arc<AtomicUsize>,
    valid_image_requests: Arc<AtomicUsize>,
    image_fetches: Arc<AtomicUsize>,
    outbound_messages: Arc<AtomicUsize>,
    image_path: Arc<String>,
    expected_image_sha256: Arc<Option<String>>,
}

async fn onebot_action(
    Path(action): Path<String>,
    State(state): State<MockState>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    match action.as_str() {
        "get_login_info" => Json(json!({
            "status":"ok", "retcode":0,
            "data":{"user_id":"900", "nickname":"RInBot"}
        })),
        "get_friend_list" => Json(json!({"status":"ok", "retcode":0, "data":[]})),
        "get_image" => {
            state.image_fetches.fetch_add(1, Ordering::Relaxed);
            Json(json!({
                "status":"ok", "retcode":0,
                "data":{"file":state.image_path.as_str()}
            }))
        }
        "send_private_msg" => {
            state.outbound_messages.fetch_add(1, Ordering::Relaxed);
            Json(json!({"status":"ok", "retcode":0, "data":{"message_id":9001}}))
        }
        _ => Json(json!({
            "status":"failed", "retcode":100, "wording":"unsupported benchmark action"
        })),
    }
}

async fn model_completion(
    State(state): State<MockState>,
    Json(request): Json<Value>,
) -> Json<Value> {
    state.model_requests.fetch_add(1, Ordering::Relaxed);
    if request["model"] == "isolated-vision-model" {
        state.vision_model_requests.fetch_add(1, Ordering::Relaxed);
        if let Some(expected_sha256) = state.expected_image_sha256.as_ref() {
            let valid_payload = request["messages"][0]["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|part| part["type"] == "image_url")
                .filter_map(|part| part["image_url"]["url"].as_str())
                .filter_map(|url| url.strip_prefix("data:image/png;base64,"))
                .filter_map(|encoded| BASE64.decode(encoded).ok())
                .any(|bytes| format!("{:x}", Sha256::digest(bytes)) == *expected_sha256);
            if valid_payload {
                state.valid_image_requests.fetch_add(1, Ordering::Relaxed);
            }
        }
    } else {
        state.primary_model_requests.fetch_add(1, Ordering::Relaxed);
    }
    Json(json!({
        "choices":[{"message":{"content":"隔离性能回放回复"}}],
        "usage":{"prompt_tokens":24,"completion_tokens":8}
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

fn rss_kib(pid: u32) -> u64 {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .expect("ps must be available for the macOS RSS benchmark");
    assert!(output.status.success(), "ps failed to sample RInBot RSS");
    String::from_utf8(output.stdout)
        .expect("ps output must be UTF-8")
        .trim()
        .parse()
        .expect("RInBot process must remain present during RSS sampling")
}

fn sqlite_count(path: &std::path::Path, query: &str) -> u64 {
    let connection = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .expect("read-only performance ledger must be accessible");
    connection
        .query_row(query, [], |row| row.get(0))
        .expect("performance ledger count query must succeed")
}

async fn wait_for_health(client: &reqwest::Client, url: &str) -> Value {
    timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(response) = client.get(url).send().await {
                if response.status().is_success() {
                    return response.json().await.unwrap_or(Value::Null);
                }
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("isolated RInBot did not become healthy")
}

/// Run explicitly on the target Mac with:
/// `cargo test --release --locked --test performance_replay -- --ignored --nocapture`
/// This is a sanitized mock-dependency replay, not the final same-NapCat acceptance.
#[tokio::test]
#[ignore = "long-running isolated typical-load RSS replay; run explicitly on the target Mac"]
async fn seven_day_p95_typical_load_stays_within_the_rss_budget() {
    let replay_count = env_usize_or("RINBOT_PERF_TEST_MESSAGES", REPLAY_COUNT);
    let media_payload_bytes = env_usize_or("RINBOT_PERF_TEST_MEDIA_BYTES", 0);
    let use_valid_media_fixture = matches!(
        std::env::var("RINBOT_PERF_TEST_VALID_MEDIA").as_deref(),
        Ok("1")
    );
    assert!(
        !use_valid_media_fixture || media_payload_bytes == 0,
        "valid media fixture mode cannot be combined with padded media bytes"
    );
    let replay_duration_secs = env_u64_or(
        "RINBOT_PERF_TEST_DURATION_SECS",
        DEFAULT_REPLAY_DURATION_SECS,
    );
    let sample_interval_secs =
        env_u64_or("RINBOT_PERF_TEST_SAMPLE_SECS", DEFAULT_SAMPLE_INTERVAL_SECS);
    assert!(replay_count > 0, "replay message count must be positive");
    assert!(
        media_payload_bytes == 0 || media_payload_bytes <= 6 * 1024 * 1024,
        "synthetic media payload must be at most 6 MiB"
    );
    assert!(
        sample_interval_secs > 0,
        "RSS sample interval must be positive"
    );
    let sample_count = sample_count_for_duration(replay_duration_secs, sample_interval_secs);
    assert!(
        sample_count > 0,
        "replay duration must include at least one sample"
    );
    let replay_interval =
        Duration::from_secs_f64(replay_duration_secs as f64 / replay_count as f64);
    let sample_interval = Duration::from_secs(sample_interval_secs);

    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join("data");
    let ui_dir = temp.path().join("ui");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&ui_dir).unwrap();
    std::fs::write(
        ui_dir.join("index.html"),
        "<!doctype html><title>RInBot perf</title>",
    )
    .unwrap();

    let image_path = temp.path().join("synthetic-padded-image.png");
    let media_fixture_bytes: &[u8] = include_bytes!("../../docs/assets/social-preview.png");
    let expected_image_sha256 = if use_valid_media_fixture {
        std::fs::write(&image_path, media_fixture_bytes).unwrap();
        let validation = Command::new("/usr/bin/sips")
            .args(["-g", "format", "-g", "pixelWidth", "-g", "pixelHeight"])
            .arg(&image_path)
            .output()
            .expect("macOS sips must be available to validate the real PNG fixture");
        assert!(
            validation.status.success(),
            "the valid media fixture must be decodable by macOS ImageIO"
        );
        let validation_output = String::from_utf8_lossy(&validation.stdout);
        assert!(
            validation_output.contains("format: png")
                && validation_output.contains("pixelWidth: 2560")
                && validation_output.contains("pixelHeight: 1280"),
            "the media fixture must decode as the expected PNG dimensions"
        );
        Some(format!("{:x}", Sha256::digest(media_fixture_bytes)))
    } else if media_payload_bytes > 0 {
        let mut image_bytes = vec![
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 8, 215, 99, 248, 207,
            192, 240, 31, 0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66,
            96, 130,
        ];
        if image_bytes.len() < media_payload_bytes {
            image_bytes.resize(media_payload_bytes, 0);
        }
        std::fs::write(&image_path, image_bytes).unwrap();
        None
    } else {
        None
    };
    let media_enabled = media_payload_bytes > 0 || use_valid_media_fixture;
    let effective_media_payload_bytes = if use_valid_media_fixture {
        media_fixture_bytes.len()
    } else {
        media_payload_bytes
    };

    let state = MockState {
        model_requests: Arc::new(AtomicUsize::new(0)),
        primary_model_requests: Arc::new(AtomicUsize::new(0)),
        vision_model_requests: Arc::new(AtomicUsize::new(0)),
        valid_image_requests: Arc::new(AtomicUsize::new(0)),
        image_fetches: Arc::new(AtomicUsize::new(0)),
        outbound_messages: Arc::new(AtomicUsize::new(0)),
        image_path: Arc::new(image_path.to_string_lossy().into_owned()),
        expected_image_sha256: Arc::new(expected_image_sha256.clone()),
    };
    let onebot_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let onebot_addr = onebot_listener.local_addr().unwrap();
    let onebot_app = Router::new()
        .route("/{action}", post(onebot_action))
        .with_state(state.clone());
    let onebot_server = tokio::spawn(async move {
        axum::serve(onebot_listener, onebot_app).await.unwrap();
    });

    let model_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_addr = model_listener.local_addr().unwrap();
    let model_app = Router::new()
        .route("/v1/chat/completions", post(model_completion))
        .with_state(state.clone())
        .layer(axum::extract::DefaultBodyLimit::max(12 * 1024 * 1024));
    let model_server = tokio::spawn(async move {
        axum::serve(model_listener, model_app).await.unwrap();
    });

    let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_addr = ws_listener.local_addr().unwrap();
    let (connected_tx, connected_rx) = oneshot::channel();
    let start_replay = Arc::new(Notify::new());
    let close_ws = Arc::new(Notify::new());
    let start_signal = start_replay.clone();
    let close_signal = close_ws.clone();
    let ws_server = tokio::spawn(async move {
        let (stream, _) = ws_listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _ = connected_tx.send(());
        start_signal.notified().await;

        let now_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        for index in 0..replay_count {
            let mut segments = vec![json!({
                "type":"text", "data":{"text":format!("典型负载回放消息 {}", index + 1)}
            })];
            if index * 100 / replay_count < 26 {
                // Media markers mirror the measured 26% share without retaining
                // private content or fetching any external image URL.
                segments.push(json!({
                    "type":"image", "data":{"file":format!("fixture-image-{index}.jpg")}
                }));
            }
            websocket
                .send(Message::Text(
                    json!({
                        "post_type":"message", "message_type":"private", "self_id":900,
                        "message_id":format!("perf-{index}"), "user_id":100, "time":now_seconds,
                        "sender":{"user_id":100,"nickname":"合成负载"},
                        "message":segments
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            if index + 1 < replay_count {
                sleep(replay_interval).await;
            }
        }
        close_signal.notified().await;
        let _ = websocket.close(None).await;
    });

    let app_addr = free_address().await;
    let app_url = format!("http://{app_addr}");
    let app_port = app_addr.port().to_string();
    let onebot_http_url = format!("http://{onebot_addr}");
    let onebot_ws_url = format!("ws://{ws_addr}/onebot/v11/ws");
    let model_base_url = format!("http://{model_addr}/v1");
    let vision_model = media_enabled.then(|| {
        json!({
            "enabled":true,
            "base_url":model_base_url.clone(),
            "model":"isolated-vision-model",
            "timeout_ms":90000
        })
    });
    let imported_config = json!({
        "schema_version":1,
        "unmigrated_enabled_features":[],
        "enabled_feature_data_missing":[],
        "onebot_ws_url":onebot_ws_url,
        "onebot_http_url":onebot_http_url,
        "onebot_token":"",
        "model_base_url":model_base_url,
        "model_id":"isolated-benchmark-model",
        "model_api_key":"fixture-only",
        "model_vision":vision_model,
        "model_vision_api_key":if media_enabled { "fixture-vision-only" } else { "" },
        "system_prompt":"你正在处理隔离的性能测试消息。简短回复。",
        "allow_private":"100",
        "allow_groups":"",
        "deny_private":"",
        "deny_groups":"",
        "random_response_percent":100,
        "trigger_keywords":"",
        "at_context_count":12,
        "keyword_context_count":8,
        "random_context_count":5,
        "all_context_count":20,
        "wake_delay_min_ms":100,
        "wake_delay_max_ms":100,
        "max_batch_wait_ms":1000,
        "batch_limit":10,
        "batch_max_chars":12000,
        "time_control_json":"{\"enabled\":false,\"schedule\":{\"mode\":\"always\",\"windows\":[]},\"overrides\":{}}",
        "model_temperature":0.4,
        "model_max_tokens":128
    });
    let imported_path = data_dir.join("rinbot-runtime.json");
    std::fs::write(
        &imported_path,
        serde_json::to_vec(&imported_config).unwrap(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&imported_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let api_token = "isolated-performance-replay-token";
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
            api_token,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(if std::env::var_os("RINBOT_PERF_TEST_VERBOSE").is_some() {
            Stdio::inherit()
        } else {
            Stdio::null()
        })
        .spawn()
        .expect("spawn isolated RInBot performance process");
    let child = ChildGuard(child);

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        reqwest::header::HeaderValue::from_static("Bearer isolated-performance-replay-token"),
    );
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap();
    let health = wait_for_health(&client, &format!("{app_url}/api/health")).await;
    assert_eq!(health["ok"], true);
    timeout(Duration::from_secs(10), connected_rx)
        .await
        .expect("mock OneBot websocket did not connect")
        .expect("mock OneBot connection signal dropped");
    timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(response) = client.get(format!("{app_url}/api/status")).send().await {
                if let Ok(status) = response.json::<Value>().await {
                    if status["onebot_connected"] == true && status["core_ready"] == true {
                        break;
                    }
                }
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("RInBot status did not become ready after the mock OneBot connected");

    let pid = child.0.id();
    let binary_sha256 = format!(
        "{:x}",
        Sha256::digest(std::fs::read(env!("CARGO_BIN_EXE_RInBot")).unwrap())
    );
    let started_at = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    start_replay.notify_one();

    let replay_media_count = (0..replay_count)
        .filter(|index| index * 100 / replay_count < 26)
        .count();
    let expected_processed_media_count = if media_enabled { replay_media_count } else { 0 };
    let mut rss_samples_kib = Vec::with_capacity(sample_count);
    let mut queue_depth_samples = Vec::with_capacity(sample_count);
    let mut sample_records = Vec::with_capacity(sample_count);
    let mut first_sampled_at_unix_ms = None;
    let mut last_sampled_at_unix_ms = None;
    for sample_index in 0..sample_count {
        let sampled_at_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after the Unix epoch")
            .as_millis();
        first_sampled_at_unix_ms.get_or_insert(sampled_at_unix_ms);
        last_sampled_at_unix_ms = Some(sampled_at_unix_ms);
        let rss_sample_kib = rss_kib(pid);
        let status = client
            .get(format!("{app_url}/api/status"))
            .send()
            .await
            .expect("isolated RInBot status API must remain reachable")
            .error_for_status()
            .expect("isolated RInBot status API must remain healthy")
            .json::<Value>()
            .await
            .expect("isolated RInBot status response must be valid JSON");
        assert_eq!(status["onebot_connected"], true, "OneBot disconnected");
        assert_eq!(status["core_ready"], true, "RInBot core became unready");
        let queue_depth = status["queue_depth"]
            .as_u64()
            .expect("status queue depth must be a nonnegative integer");
        rss_samples_kib.push(rss_sample_kib);
        queue_depth_samples.push(queue_depth);
        sample_records.push(json!({
            "sample_index":sample_index,
            "sampled_at_unix_ms":sampled_at_unix_ms,
            "rss_kib":rss_sample_kib,
            "queue_depth":queue_depth,
            "api_healthy":true,
            "onebot_connected":true,
            "core_ready":true
        }));
        sleep(sample_interval).await;
    }
    timeout(Duration::from_secs(120), async {
        while state.primary_model_requests.load(Ordering::Relaxed) < replay_count
            || state.vision_model_requests.load(Ordering::Relaxed) < expected_processed_media_count
            || state.image_fetches.load(Ordering::Relaxed) < expected_processed_media_count
            || state.outbound_messages.load(Ordering::Relaxed) < replay_count
        {
            sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .expect("the replay did not complete every model request and OneBot send");
    close_ws.notify_one();

    let messages_db = data_dir.join("rinbot.sqlite");
    let proactive_db = data_dir.join("proactive.sqlite");
    let completed_inbound_batches = sqlite_count(
        &messages_db,
        "SELECT count(*) FROM inbox_batches WHERE state = 'completed'",
    );
    let non_completed_inbound_batches = sqlite_count(
        &messages_db,
        "SELECT count(*) FROM inbox_batches WHERE state <> 'completed'",
    );
    let sent_follow_up_wakes = sqlite_count(
        &proactive_db,
        "SELECT count(*) FROM proactive_wakes WHERE kind = 'followUp' AND state = 'sent'",
    );
    let sent_outbox = sqlite_count(
        &messages_db,
        "SELECT count(*) FROM outbox WHERE state = 'sent'",
    );
    let non_sent_outbox = sqlite_count(
        &messages_db,
        "SELECT count(*) FROM outbox WHERE state <> 'sent'",
    );
    let failed_outbox = sqlite_count(
        &messages_db,
        "SELECT count(*) FROM outbox WHERE state = 'failed'",
    );
    let unknown_outbox = sqlite_count(
        &messages_db,
        "SELECT count(*) FROM outbox WHERE state = 'unknown'",
    );
    let pending_outbox = sqlite_count(
        &messages_db,
        "SELECT count(*) FROM outbox WHERE state = 'pending'",
    );
    let sending_outbox = sqlite_count(
        &messages_db,
        "SELECT count(*) FROM outbox WHERE state = 'sending'",
    );
    let model_requests = state.model_requests.load(Ordering::Relaxed) as u64;
    let primary_model_requests = state.primary_model_requests.load(Ordering::Relaxed) as u64;
    let vision_model_requests = state.vision_model_requests.load(Ordering::Relaxed) as u64;
    let valid_image_requests = state.valid_image_requests.load(Ordering::Relaxed) as u64;
    let image_fetches = state.image_fetches.load(Ordering::Relaxed) as u64;
    let outbound_messages = state.outbound_messages.load(Ordering::Relaxed) as u64;
    assert_eq!(
        completed_inbound_batches, replay_count as u64,
        "every synthetic inbound message must complete exactly one batch"
    );
    assert_eq!(
        non_completed_inbound_batches, 0,
        "no inbound batch may remain leased, pending, or in a terminal failure state"
    );
    assert_eq!(
        primary_model_requests,
        completed_inbound_batches + sent_follow_up_wakes,
        "primary model requests must reconcile with inbound batches and sent follow-up wakes"
    );
    assert_eq!(
        image_fetches, expected_processed_media_count as u64,
        "every synthetic media marker must fetch exactly one local fixture"
    );
    assert_eq!(
        vision_model_requests, expected_processed_media_count as u64,
        "every fetched synthetic image must be processed by the mock vision model"
    );
    if use_valid_media_fixture {
        assert_eq!(
            valid_image_requests, expected_processed_media_count as u64,
            "every valid fixture image must reach the vision model as intact PNG bytes"
        );
    }
    assert_eq!(
        model_requests,
        primary_model_requests + vision_model_requests,
        "total model requests must reconcile with primary and vision requests"
    );
    assert_eq!(
        outbound_messages, sent_outbox,
        "mock OneBot sends must reconcile with the durable sent outbox"
    );
    assert_eq!(
        sent_outbox,
        completed_inbound_batches + sent_follow_up_wakes,
        "sent outbox must reconcile with completed inbound replies and follow-ups"
    );
    assert_eq!(
        non_sent_outbox, 0,
        "every durable outbound record must be confirmed sent"
    );
    assert_eq!(
        failed_outbox, 0,
        "the replay must not leave failed outbox rows"
    );
    assert_eq!(
        unknown_outbox, 0,
        "the replay must not leave unknown outbox rows"
    );
    assert_eq!(
        pending_outbox, 0,
        "the replay must not leave pending outbox rows"
    );
    assert_eq!(
        sending_outbox, 0,
        "the replay must not leave sending outbox rows"
    );

    let mut sorted_rss_samples_kib = rss_samples_kib.clone();
    sorted_rss_samples_kib.sort_unstable();
    let p50 = sorted_rss_samples_kib[(sorted_rss_samples_kib.len() - 1) * 50 / 100] as f64 / 1024.0;
    let p95 = sorted_rss_samples_kib[(sorted_rss_samples_kib.len() - 1) * 95 / 100] as f64 / 1024.0;
    let peak = *sorted_rss_samples_kib.last().unwrap() as f64 / 1024.0;
    let queue_depth_peak = *queue_depth_samples.iter().max().unwrap_or(&0);
    let growth_window = (rss_samples_kib.len() / 10).max(1);
    let mean_mb =
        |samples: &[u64]| samples.iter().sum::<u64>() as f64 / samples.len() as f64 / 1024.0;
    let post_warmup_growth_mb =
        mean_mb(&rss_samples_kib[rss_samples_kib.len().saturating_sub(growth_window)..])
            - mean_mb(&rss_samples_kib[..growth_window]);
    let observed_sample_window_seconds = last_sampled_at_unix_ms
        .zip(first_sampled_at_unix_ms)
        .map(|(last, first)| last.saturating_sub(first) / 1_000)
        .expect("at least one RSS/status sample is required");
    if replay_duration_secs >= 86_400 {
        assert!(
            observed_sample_window_seconds >= replay_duration_secs as u128,
            "observed sample window {observed_sample_window_seconds}s is shorter than the requested {replay_duration_secs}s"
        );
        assert!(
            post_warmup_growth_mb <= 10.0,
            "post-warmup RSS grew by {post_warmup_growth_mb:.2} MiB"
        );
    }
    let summary = json!({
        "started_at":started_at,
        "messages":replay_count,
        "media_markers":replay_media_count,
        "completed_inbound_batches":completed_inbound_batches,
        "non_completed_inbound_batches":non_completed_inbound_batches,
        "sent_follow_up_wakes":sent_follow_up_wakes,
        "model_requests":model_requests,
        "primary_model_requests":primary_model_requests,
        "vision_model_requests":vision_model_requests,
        "valid_media_images_processed":valid_image_requests,
        "media_fixture_sha256":expected_image_sha256,
        "image_fetches":image_fetches,
        "media_payload_bytes":effective_media_payload_bytes,
        "media_fixture_kind":if use_valid_media_fixture { "valid-png" } else if media_payload_bytes > 0 { "synthetic-padded-png-header" } else { "none" },
        "outbound_messages":outbound_messages,
        "sent_outbox":sent_outbox,
        "non_sent_outbox":non_sent_outbox,
        "failed_outbox":failed_outbox,
        "unknown_outbox":unknown_outbox,
        "pending_outbox":pending_outbox,
        "sending_outbox":sending_outbox,
        "duration_seconds":observed_sample_window_seconds,
        "sample_interval_seconds":sample_interval.as_secs(),
        "sample_count":rss_samples_kib.len(),
        "api_status_sample_count":queue_depth_samples.len(),
        "rss_p50_mb":p50,
        "rss_p95_mb":p95,
        "rss_peak_mb":peak,
        "queue_depth_peak":queue_depth_peak,
        "post_warmup_growth_mb":post_warmup_growth_mb,
        "rust_binary_sha256":binary_sha256,
        "dependencies":"local mock OneBot and mock model; no QQ or production data"
    });
    if let Some(report_path) = std::env::var_os("RINBOT_PERF_TEST_REPORT_PATH") {
        let report_path = std::path::PathBuf::from(report_path);
        validate_report_destination(&report_path, temp.path())
            .expect("RINBOT_PERF_TEST_REPORT_PATH must be absolute and resolve outside temp");
        let parent = report_path
            .parent()
            .expect("the performance evidence path must have a parent directory");
        assert!(
            parent.is_dir(),
            "the performance evidence directory must exist"
        );
        let bytes = serde_json::to_vec_pretty(&json!({
            "format_version":1,
            "summary":summary,
            "samples":sample_records
        }))
        .expect("performance evidence must serialize");
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut evidence = options
            .open(&report_path)
            .expect("performance evidence path must be new; existing files are never overwritten");
        evidence
            .write_all(&bytes)
            .expect("performance evidence must be written completely");
        evidence
            .sync_all()
            .expect("performance evidence must be synchronized to disk");
        eprintln!("RINBOT_PERF_EVIDENCE_PATH={}", report_path.display());
        eprintln!("RINBOT_PERF_EVIDENCE_SHA256={:x}", Sha256::digest(&bytes));
    }
    eprintln!("RINBOT_TYPICAL_LOAD_RESULT={summary}");
    assert!(
        p95 <= 80.0,
        "typical-load RSS P95 {p95:.2} MiB exceeds 80 MiB"
    );

    drop(child);
    onebot_server.abort();
    model_server.abort();
    ws_server.abort();
}
