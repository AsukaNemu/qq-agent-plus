use std::{
    net::SocketAddr,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use axum::{extract::Path, routing::post, Json, Router};
use futures_util::StreamExt;
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::{
    net::TcpListener,
    sync::mpsc,
    time::{sleep, timeout},
};
use tokio_tungstenite::accept_async;

async fn onebot_action(Path(action): Path<String>, Json(_body): Json<Value>) -> Json<Value> {
    if action == "get_login_info" {
        Json(json!({
            "status":"ok", "retcode":0,
            "data":{"user_id":"900","nickname":"RInBot restart fixture"}
        }))
    } else {
        Json(json!({"status":"failed","retcode":100,"wording":"unsupported fixture action"}))
    }
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

async fn wait_active_ready(client: &reqwest::Client, base_url: &str, child: &mut Child) -> Value {
    let mut last_status = Value::Null;
    let mut last_error = String::new();
    match timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(Some(exit)) = child.try_wait() {
                panic!("isolated RInBot exited before readiness with {exit}");
            }
            match client.get(format!("{base_url}/api/status")).send().await {
                Ok(response) if response.status().is_success() => {
                    let status: Value = response.json().await.unwrap_or(Value::Null);
                    last_status = status.clone();
                    if status["mode"] == "active"
                        && status["onebot_connected"] == true
                        && status["core_ready"] == true
                    {
                        return Some(status);
                    }
                }
                Ok(response) => last_error = format!("HTTP {}", response.status()),
                Err(error) => last_error = error.to_string(),
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    {
        Ok(Some(status)) => status,
        Ok(None) => unreachable!("readiness loop only resolves with a status"),
        Err(_) => panic!("active RInBot was not ready; last status={last_status}, last request error={last_error}"),
    }
}

#[tokio::test]
async fn ten_isolated_active_process_restarts_restore_api_core_and_mock_napcat_under_ten_seconds() {
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join("rinbot-data");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(
        data_dir.join("rinbot-runtime.json"),
        br#"{"schema_version":1,"unmigrated_enabled_features":[],"enabled_feature_data_missing":[]}"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            data_dir.join("rinbot-runtime.json"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }

    let onebot_http_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let onebot_http_addr = onebot_http_listener.local_addr().unwrap();
    let http_server = tokio::spawn(async move {
        axum::serve(
            onebot_http_listener,
            Router::new().route("/{action}", post(onebot_action)),
        )
        .await
        .unwrap();
    });

    let onebot_ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let onebot_ws_addr = onebot_ws_listener.local_addr().unwrap();
    let (connections_tx, mut connections_rx) = mpsc::unbounded_channel();
    let ws_server = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = onebot_ws_listener.accept().await else {
                break;
            };
            let Ok(mut websocket) = accept_async(stream).await else {
                continue;
            };
            let _ = connections_tx.send(());
            tokio::spawn(async move { while websocket.next().await.is_some() {} });
        }
    });

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        reqwest::header::HeaderValue::from_static("Bearer isolated-restart-test-token"),
    );
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap();
    let mut startup_durations = Vec::new();
    for cycle in 1..=10 {
        let app_addr = free_address().await;
        let base_url = format!("http://{app_addr}");
        let onebot_http_url = format!("http://{onebot_http_addr}");
        let onebot_ws_url = format!("ws://{onebot_ws_addr}/onebot/v11/ws");
        let started = Instant::now();
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
                "--active",
                "--connect-onebot",
                "--onebot-ws-url",
                &onebot_ws_url,
                "--onebot-http-url",
                &onebot_http_url,
                "--api-token",
                "isolated-restart-test-token",
                "--model-base-url",
                "http://127.0.0.1:1/v1",
                "--model-id",
                "restart-fixture-model",
                "--model-api-key",
                "fixture-only-key",
                "--allow-private",
                "100",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn isolated RInBot process");
        let mut child = ChildGuard(child);
        let status = wait_active_ready(&client, &base_url, &mut child.0).await;
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(10),
            "cycle {cycle} readiness took {elapsed:?}"
        );
        assert_eq!(status["mode"], "active");
        assert_eq!(status["onebot_connected"], true);
        assert_eq!(status["core_ready"], true);
        timeout(Duration::from_secs(2), connections_rx.recv())
            .await
            .expect("mock NapCat did not observe the connection")
            .expect("mock NapCat connection stream closed");
        startup_durations.push(elapsed);

        if cycle == 1 {
            #[cfg(unix)]
            {
                let signal_result =
                    unsafe { libc::kill(child.0.id() as libc::pid_t, libc::SIGTERM) };
                assert_eq!(signal_result, 0, "send SIGTERM to isolated Rust process");
                let exit = timeout(Duration::from_secs(2), async {
                    loop {
                        if let Some(exit) = child.0.try_wait().expect("poll child exit") {
                            break exit;
                        }
                        sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("SIGTERM shutdown exceeded the bounded test deadline");
                assert_eq!(exit.code(), Some(0), "SIGTERM should gracefully exit");
            }
            #[cfg(not(unix))]
            {
                child.0.kill().expect("stop isolated process");
                child.0.wait().expect("reap isolated process");
            }
        } else {
            child
                .0
                .kill()
                .expect("stop isolated process before next cycle");
            child.0.wait().expect("reap isolated process");
        }
        // Avoid a port-reuse race while the killed process releases its listener.
        sleep(Duration::from_millis(30)).await;
    }

    let max_startup = startup_durations.into_iter().max().unwrap();
    assert!(max_startup < Duration::from_secs(10));
    ws_server.abort();
    http_server.abort();
}
