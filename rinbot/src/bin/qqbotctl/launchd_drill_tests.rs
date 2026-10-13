use super::*;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::os::unix::fs::{symlink, OpenOptionsExt, PermissionsExt};
use tempfile::tempdir;

// Every service operation maps to one unique test label. The production
// controller's fixed URL maps to one ephemeral loopback port; Node operations
// are rejected, including if a future controller accidentally requests one.
struct DrillDriver {
    inner: LaunchctlDriver,
    label: String,
    health_url: String,
    port: u16,
}

impl DrillDriver {
    fn rust_only(&self, label: &str) -> Result<(), String> {
        if label != SERVICE_LABEL {
            return Err("unexpected drill service".into());
        }
        Ok(())
    }
}

impl ServiceDriver for DrillDriver {
    fn ensure_running(&mut self, label: &str) -> Result<(), String> {
        self.rust_only(label)?;
        self.inner.ensure_running(&self.label)
    }
    fn ensure_stopped(&mut self, label: &str) -> Result<(), String> {
        self.rust_only(label)?;
        self.inner.ensure_stopped(&self.label)
    }
    fn stop(&mut self, label: &str, timeout: Duration) -> Result<(), String> {
        self.rust_only(label)?;
        self.inner.stop(&self.label, timeout)
    }
    fn start(&mut self, label: &str, plist: &Path) -> Result<(), String> {
        self.rust_only(label)?;
        self.inner.start(&self.label, plist)
    }
    fn wait_ready(&mut self, url: &str, timeout: Duration) -> Result<(), String> {
        if url != "http://127.0.0.1:3211/api/status" {
            return Err("unexpected drill URL".into());
        }
        self.inner.wait_ready(&self.health_url, timeout)
    }
    fn wait_node_ready(&mut self, _: &str, _: Duration) -> Result<(), String> {
        Err("Node is outside this isolated Rust drill".into())
    }
    fn verify_node_stopped(&mut self, _: Duration) -> Result<(), String> {
        Err("Node is outside this isolated Rust drill".into())
    }
    fn verify_rust_stopped(&mut self, timeout: Duration) -> Result<(), String> {
        verify_loopback_port_closed(self.port, "isolated RInBot", timeout)
    }
}

impl Drop for DrillDriver {
    fn drop(&mut self) {
        let _ = self.inner.stop(&self.label, STOP_BUDGET);
    }
}

#[test]
#[ignore = "ten real isolated launchd restarts and bad-candidate rollback; requires RINBOT_LAUNCHD_TEST_BINARY"]
fn isolated_launchd_rinbot_restarts_and_bad_candidate_restore_authenticated_readiness() {
    let source = PathBuf::from(
        std::env::var_os("RINBOT_LAUNCHD_TEST_BINARY")
            .expect("set RINBOT_LAUNCHD_TEST_BINARY to the retained candidate"),
    );
    assert!(source.is_absolute() && source.is_file() && is_executable(&source));
    let binary_sha = format!("{:x}", Sha256::digest(fs::read(&source).unwrap()));
    let temp = tempdir().unwrap();
    let data = temp.path().join("data");
    let ui = temp.path().join("ui");
    let releases = temp.path().join("releases");
    for path in [&data, &ui, &releases] {
        fs::create_dir(path).unwrap();
    }
    fs::write(ui.join("index.html"), "<html>isolated launchd drill</html>").unwrap();
    fs::write(data.join("rinbot-runtime.json"), br#"{"schema_version":1,"unmigrated_enabled_features":[],"enabled_feature_data_missing":[]}"#).unwrap();
    fs::set_permissions(
        data.join("rinbot-runtime.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let release_a = releases.join("RInBot-a");
    let release_b = releases.join("RInBot-b");
    fs::copy(&source, &release_a).unwrap();
    fs::copy(&source, &release_b).unwrap();
    let bad_release = releases.join("RInBot-bad");
    fs::write(&bad_release, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&bad_release, fs::Permissions::from_mode(0o755)).unwrap();
    let active = temp.path().join("active");
    symlink(&release_a, &active).unwrap();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (http_address, ws_address) = runtime.block_on(async {
        let http = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_address = http.local_addr().unwrap();
        async fn action(axum::extract::Path(action): axum::extract::Path<String>) -> axum::Json<serde_json::Value> {
            axum::Json(match action.as_str() {
                "get_login_info" => serde_json::json!({"status":"ok","retcode":0,"data":{"user_id":900,"nickname":"isolated launchd fixture"}}),
                "get_friend_list" => serde_json::json!({"status":"ok","retcode":0,"data":[]}),
                _ => serde_json::json!({"status":"failed","retcode":100,"wording":"unsupported isolated fixture action"}),
            })
        }
        tokio::spawn(async move {
            axum::serve(http, axum::Router::new().route("/{action}", axum::routing::post(action))).await.unwrap();
        });
        let ws = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_address = ws.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = ws.accept().await {
                tokio::spawn(async move {
                    if let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await {
                        while socket.next().await.is_some() {}
                    }
                });
            }
        });
        (http_address, ws_address)
    });
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let label = format!(
        "com.local.rinbot.restart-drill.{}.{}",
        std::process::id(),
        now_ms()
    );
    let plist_path = temp.path().join("isolated-rinbot.plist");
    let args = vec![
        active.to_string_lossy().into_owned(),
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string(),
        "--data-dir".into(),
        data.to_string_lossy().into_owned(),
        "--ui-dir".into(),
        ui.to_string_lossy().into_owned(),
        "--active".into(),
        "--connect-onebot".into(),
        "--onebot-http-url".into(),
        format!("http://{http_address}"),
        "--onebot-ws-url".into(),
        format!("ws://{ws_address}/onebot/v11/ws"),
        "--model-base-url".into(),
        "http://127.0.0.1:1/v1".into(),
        "--model-id".into(),
        "isolated-fixture".into(),
        "--model-api-key".into(),
        "fixture-only-key".into(),
        "--allow-private".into(),
        "100".into(),
    ];
    let mut plist = plist::Dictionary::new();
    plist.insert("Label".into(), plist::Value::String(label.clone()));
    plist.insert(
        "ProgramArguments".into(),
        plist::Value::Array(args.into_iter().map(plist::Value::String).collect()),
    );
    plist.insert("RunAtLoad".into(), plist::Value::Boolean(true));
    let mut keep_alive = plist::Dictionary::new();
    keep_alive.insert("SuccessfulExit".into(), plist::Value::Boolean(false));
    plist.insert("KeepAlive".into(), plist::Value::Dictionary(keep_alive));
    let mut environment = plist::Dictionary::new();
    environment.insert(
        "QQBOTD_API_TOKEN".into(),
        plist::Value::String("isolated-launchd-readiness-token".into()),
    );
    plist.insert(
        "EnvironmentVariables".into(),
        plist::Value::Dictionary(environment),
    );
    plist.insert(
        "StandardOutPath".into(),
        plist::Value::String(
            temp.path()
                .join("stdout.log")
                .to_string_lossy()
                .into_owned(),
        ),
    );
    plist.insert(
        "StandardErrorPath".into(),
        plist::Value::String(
            temp.path()
                .join("stderr.log")
                .to_string_lossy()
                .into_owned(),
        ),
    );
    plist::Value::Dictionary(plist)
        .to_file_xml(&plist_path)
        .unwrap();
    fs::set_permissions(&plist_path, fs::Permissions::from_mode(0o600)).unwrap();
    let mut driver = DrillDriver {
        inner: LaunchctlDriver::for_rust_plist(&plist_path).unwrap(),
        label: label.clone(),
        health_url: format!("http://127.0.0.1:{port}/api/status"),
        port,
    };
    driver.ensure_stopped(SERVICE_LABEL).unwrap();
    driver.start(SERVICE_LABEL, &plist_path).unwrap();
    driver
        .wait_ready("http://127.0.0.1:3211/api/status", READY_BUDGET)
        .unwrap();
    let mut rounds = Vec::new();
    for round in 0..10 {
        let candidate = if round % 2 == 0 {
            &release_b
        } else {
            &release_a
        };
        let journal = restart_with_rollback(
            &mut driver,
            &active,
            candidate,
            &releases,
            &plist_path,
            "http://127.0.0.1:3211/api/status",
            &temp.path().join(format!("round-{round}.json")),
        )
        .unwrap_or_else(|error| panic!("launchd round {} failed: {}", round + 1, error.message));
        assert!(journal.elapsed_ms <= 10_000);
        assert_eq!(journal.state, "candidate-ready");
        rounds.push(journal);
    }
    let before_failure = fs::canonicalize(&active).unwrap();
    let failure = restart_with_rollback(
        &mut driver,
        &active,
        &bad_release,
        &releases,
        &plist_path,
        "http://127.0.0.1:3211/api/status",
        &temp.path().join("bad-candidate.json"),
    )
    .expect_err("bad executable must trigger automatic rollback");
    assert_eq!(
        failure.journal.recovery_state, "old-service-restored",
        "{}",
        failure.message
    );
    assert_eq!(fs::canonicalize(&active).unwrap(), before_failure);
    driver
        .wait_ready("http://127.0.0.1:3211/api/status", READY_BUDGET)
        .unwrap();
    driver.stop(SERVICE_LABEL, STOP_BUDGET).unwrap();
    driver.ensure_stopped(SERVICE_LABEL).unwrap();
    driver.verify_rust_stopped(STOP_BUDGET).unwrap();
    let max_ms = rounds.iter().map(|round| round.elapsed_ms).max().unwrap();
    let report = serde_json::json!({"format_version":1,"rust_binary_sha256":binary_sha,
        "controller_source_sha256":format!("{:x}", Sha256::digest(include_bytes!("switch.rs"))),
        "dependencies":"real isolated launchd and RInBot; local mock OneBot; no QQ or production data",
        "successful_rounds":rounds.len(),"max_recovery_ms":max_ms,"rounds":rounds,
        "bad_candidate":failure.journal,"isolated_service_unloaded":true,"isolated_port_closed":true});
    if let Some(path) = std::env::var_os("RINBOT_LAUNCHD_TEST_REPORT_PATH") {
        use std::io::Write;
        let path = PathBuf::from(path);
        assert!(path.is_absolute() && !path.starts_with(temp.path()));
        let bytes = serde_json::to_vec_pretty(&report).unwrap();
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.write_all(&bytes).unwrap();
        file.sync_all().unwrap();
    }
    eprintln!(
        "ISOLATED_LAUNCHD_ROUNDS=10 MAX_RECOVERY_MS={max_ms} BAD_CANDIDATE_RECOVERY={}",
        failure.journal.recovery_state
    );
}
