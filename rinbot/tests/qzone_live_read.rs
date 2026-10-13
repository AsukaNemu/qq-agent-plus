use std::{fs, path::PathBuf, sync::Arc};

use rinbot::{
    adapters::napcat::{NapcatAdapter, NapcatConfig},
    application::qzone_web_read::QzoneReadService,
    ports::gateway::{GatewayError, QzoneGateway, QzoneReadAction},
};
use serde_json::Value;

#[tokio::test]
#[ignore = "explicit live read-only probe; requires RINBOT_QZONE_CONFIG"]
async fn napcat_unsupported_qzone_reads_fall_back_to_live_cookie_web_reads() {
    let config_path = std::env::var_os("RINBOT_QZONE_CONFIG")
        .map(PathBuf::from)
        .expect("set RINBOT_QZONE_CONFIG to the local Node config.json");
    let bytes = fs::read(config_path).expect("read configured QZone probe config");
    let config: Value =
        serde_json::from_slice(&bytes).expect("parse configured QZone probe config");
    let onebot = config
        .get("onebot")
        .and_then(Value::as_object)
        .expect("OneBot settings must be present");
    let string = |key: &str| {
        onebot
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let http_url = string("httpUrl");
    let access_token = {
        let dedicated = string("httpAccessToken");
        if dedicated.is_empty() {
            string("accessToken")
        } else {
            dedicated
        }
    };
    assert!(http_url.starts_with("http://127.0.0.1:") || http_url.starts_with("http://localhost:"));

    // The adapter is deliberately not run as a WebSocket client. observe_only
    // makes every QZone/QQ write unavailable even if this probe is extended.
    let adapter = Arc::new(NapcatAdapter::new(NapcatConfig {
        ws_url: "ws://127.0.0.1:1/unused".into(),
        http_url,
        access_token,
        observe_only: true,
    }));
    let login = adapter
        .get_login_info()
        .await
        .unwrap_or_else(|_| panic!("NapCat login-info read failed"));
    let cookie_data = adapter
        .qzone_read(
            QzoneReadAction::GetCookies,
            serde_json::json!({"domain":"user.qzone.qq.com"}),
        )
        .await
        .unwrap_or_else(|_| panic!("NapCat QZone cookie read failed"));
    let cookies = cookie_data
        .get("cookies")
        .or_else(|| cookie_data.get("cookie"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let has_session_key = cookies
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .any(|(key, value)| matches!(key.trim(), "p_skey" | "skey") && !value.trim().is_empty());
    assert!(
        has_session_key,
        "NapCat did not provide a usable QZone session cookie"
    );
    let key_order_mismatch = {
        let keys = cookies
            .split(';')
            .filter_map(|part| part.trim().split_once('=').map(|(key, _)| key.trim()))
            .collect::<Vec<_>>();
        let p_skey = keys.iter().position(|key| *key == "p_skey");
        let skey = keys.iter().position(|key| *key == "skey");
        matches!((p_skey, skey), (Some(p), Some(s)) if s < p)
    };
    let reader = QzoneReadService::new(adapter).expect("construct read-only QZone service");

    let feeds = reader.feeds(&login.user_id, 1).await;
    let own_posts = reader.own_posts(&login.user_id, 1).await;
    let feed_count = feeds
        .as_ref()
        .ok()
        .and_then(|feeds| feeds.get("feeds").and_then(Value::as_array).map(Vec::len));
    let own_post_count = own_posts
        .as_ref()
        .ok()
        .and_then(|posts| posts.get("msglist").and_then(Value::as_array).map(Vec::len));
    assert!(
        feed_count.is_some() && own_post_count.is_some(),
        "Rust live QZone fallback categories: feeds={}, own_posts={}",
        feeds
            .as_ref()
            .err()
            .map(safe_error_category)
            .unwrap_or("ok"),
        own_posts
            .as_ref()
            .err()
            .map(safe_error_category)
            .unwrap_or("ok")
    );

    // Never print account IDs, QZone text, image URLs, comments, or cookies.
    println!(
        "read-only live QZone fallback passed: normalized feeds={}, own_posts={}, legacy_cookie_order_mismatch={key_order_mismatch}",
        feed_count.unwrap_or_default(),
        own_post_count.unwrap_or_default()
    );
}

fn safe_error_category(error: &GatewayError) -> &'static str {
    match error {
        GatewayError::WritesDisabled => "writes_disabled",
        GatewayError::NotConnected => "not_connected",
        GatewayError::UnsupportedAction(_) => "unsupported_action",
        GatewayError::UnknownOutcome => "unknown_outcome",
        GatewayError::Request(message)
            if message.contains("Cookie") || message.contains("会话") =>
        {
            "cookie_or_session"
        }
        GatewayError::Request(message) if message.starts_with("OneBot ") => "onebot_rejection",
        GatewayError::Request(message) if message.starts_with("QZone HTTP 3") => {
            "qzone_redirect_status"
        }
        GatewayError::Request(message) if message == "QZone HTTP 401" => "qzone_auth_status",
        GatewayError::Request(message) if message == "QZone HTTP 403" => "qzone_forbidden_status",
        GatewayError::Request(message) if message.starts_with("QZone HTTP 4") => {
            "qzone_client_status"
        }
        GatewayError::Request(message) if message.starts_with("QZone HTTP 5") => {
            "qzone_server_status"
        }
        GatewayError::Request(message) if message.contains("请求失败") => "http_transport",
        GatewayError::Request(message) if message.contains("QZone 动态") => "js_object_parser",
        GatewayError::Request(message) if message.contains("JSONP") || message.contains("响应") => {
            "response_parse_or_shape"
        }
        GatewayError::Request(message) if message.contains("接口返回错误") => {
            "qzone_api_error"
        }
        GatewayError::Request(message) if message.contains("返回格式无效") => {
            "response_shape"
        }
        GatewayError::Request(_) => "request_rejected",
    }
}
