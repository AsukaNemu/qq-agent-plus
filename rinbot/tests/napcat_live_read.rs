//! Explicitly opt-in, read-only live transport probe. No executor, model,
//! database, friend-list sync, or QQ/QZone write is constructed here.
use std::{fs, path::PathBuf, sync::Arc, time::Duration};

use rinbot::{
    adapters::napcat::{NapcatAdapter, NapcatConfig},
    domain::event::DomainEvent,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

fn local_transport_url(value: &str, scheme: &str) -> bool {
    reqwest::Url::parse(value).is_ok_and(|url| {
        url.scheme() == scheme
            && url.host_str() == Some("127.0.0.1")
            && url.port().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    })
}

#[test]
fn live_probe_rejects_nonlocal_urls_and_embedded_credentials() {
    assert!(local_transport_url("http://127.0.0.1:3001", "http"));
    assert!(local_transport_url(
        "ws://127.0.0.1:3001/onebot/v11/ws",
        "ws"
    ));
    for value in [
        "http://example.com:3001",
        "http://127.0.0.1:3001?token=x",
        "http://x@127.0.0.1:3001",
        "https://127.0.0.1:3001",
    ] {
        assert!(!local_transport_url(value, "http"));
    }
}

#[tokio::test]
#[ignore = "live read-only HTTP/WebSocket probe; requires RINBOT_NAPCAT_READ_CONFIG"]
async fn live_napcat_login_and_two_observe_only_connections_are_healthy() {
    let config_path = PathBuf::from(
        std::env::var_os("RINBOT_NAPCAT_READ_CONFIG")
            .expect("set RINBOT_NAPCAT_READ_CONFIG to the local Node config.json"),
    );
    let metadata = fs::symlink_metadata(&config_path).expect("read config metadata");
    assert!(
        metadata.file_type().is_file() && metadata.len() <= 4 * 1024 * 1024,
        "probe configuration must be a bounded ordinary file"
    );
    let bytes = fs::read(&config_path).expect("read probe config");
    let config_hash = format!("{:x}", Sha256::digest(&bytes));
    let config: Value = serde_json::from_slice(&bytes).expect("parse probe config");
    let onebot = config
        .get("onebot")
        .and_then(Value::as_object)
        .expect("OneBot configuration missing");
    let string = |key: &str| {
        onebot
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let http_url = string("httpUrl");
    let ws_url = string("wsUrl");
    let token = string("accessToken");
    let http_token = string("httpAccessToken");
    assert!(
        local_transport_url(&http_url, "http") && local_transport_url(&ws_url, "ws"),
        "live probe accepts explicit loopback HTTP/WS only"
    );
    assert!(http_token.is_empty() || http_token == token,
        "current adapter requires the same HTTP/WS credential; separate credentials need a separate compatibility review");
    let adapter = Arc::new(NapcatAdapter::new(NapcatConfig {
        ws_url,
        http_url,
        access_token: token,
        observe_only: true,
    }));
    let login = tokio::time::timeout(Duration::from_secs(5), adapter.get_login_info())
        .await
        .expect("login-info read timed out")
        .unwrap_or_else(|_| panic!("login-info read failed"));
    assert!(!login.user_id.is_empty());
    let mut normalized_messages = 0_u64;
    let mut notices = 0_u64;
    let mut health_samples = 0_u64;
    for _ in 0..2 {
        let (tx, mut rx) = mpsc::channel(64);
        let observer = adapter.clone();
        let task = tokio::spawn(async move { observer.run(tx).await });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !adapter.connected() {
                assert!(
                    !task.is_finished(),
                    "read-only observer ended before connecting"
                );
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("read-only WebSocket connection timed out");
        for _ in 0..10 {
            while let Ok(event) = rx.try_recv() {
                if matches!(event, DomainEvent::Message(_)) {
                    normalized_messages += 1;
                } else {
                    notices += 1;
                }
            }
            assert!(
                !task.is_finished() && adapter.connected(),
                "read-only observer disconnected"
            );
            let current = tokio::time::timeout(Duration::from_secs(5), adapter.get_login_info())
                .await
                .expect("read-only health sample timed out")
                .unwrap_or_else(|_| panic!("read-only health sample failed"));
            assert!(
                current.user_id == login.user_id,
                "OneBot account changed during probe"
            );
            health_samples += 1;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        // Cancellation drops ConnectionGuard, closes the socket, and leaves
        // the production Node connection untouched. No observer data is saved.
        task.abort();
        let _ = task.await;
        assert!(
            !adapter.connected(),
            "observer cancellation must clear connected state"
        );
    }
    assert!(
        config_hash == format!("{:x}", Sha256::digest(fs::read(&config_path).unwrap())),
        "source config changed during the read-only probe"
    );
    let report = json!({
        "format_version": 1,
        "recorded_at": time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap(),
        "scope": "live read-only NapCat adapter transport; no RInBot core, executor, model or production database",
        "source_config_sha256": config_hash,
        "adapter_source_sha256": format!("{:x}", Sha256::digest(include_bytes!("../src/adapters/napcat/mod.rs"))),
        "successful_observer_connections": 2,
        "healthy_login_samples": health_samples,
        "normalized_messages_observed": normalized_messages,
        "normalized_nonmessage_events_observed": notices,
        "external_write_calls": 0,
        "source_config_unchanged": true,
        "observer_disconnected_after_probe": true,
        "full_qq_acceptance_passed": false
    });
    if let Some(path) = std::env::var_os("RINBOT_NAPCAT_READ_REPORT") {
        use std::io::Write;
        let path = PathBuf::from(path);
        assert!(path.is_absolute(), "report destination must be absolute");
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path).expect("create new private probe report");
        file.write_all(&serde_json::to_vec_pretty(&report).unwrap())
            .unwrap();
        file.sync_all().unwrap();
    }
    println!("LIVE_NAPCAT_READ_ONLY=true CONNECTIONS=2 HEALTH_SAMPLES={health_samples} EXTERNAL_WRITES=0");
}
