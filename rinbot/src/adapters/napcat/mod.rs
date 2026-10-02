use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use reqwest::{Client, StatusCode};
use serde_json::{json, Value};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::{
    domain::{
        event::DomainEvent,
        message::{IncomingMessage, MessageSegment},
    },
    ports::gateway::{GatewayError, LoginInfo, MessageGateway, SendReceipt},
};

const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct NapcatConfig {
    pub ws_url: String,
    pub http_url: String,
    pub access_token: String,
    pub observe_only: bool,
}

#[derive(Debug, Error)]
pub enum NapcatError {
    #[error("websocket connection failed: {0}")]
    WebSocket(String),
    #[error("websocket stream ended")]
    StreamEnded,
    #[error("event channel closed")]
    EventChannelClosed,
    #[error("HTTP request failed: {0}")]
    Request(String),
    #[error("OneBot health check failed: {0}")]
    Health(String),
}

pub struct NapcatAdapter {
    config: NapcatConfig,
    client: Client,
    connected: Arc<AtomicBool>,
}

impl NapcatAdapter {
    pub fn new(config: NapcatConfig) -> Self {
        Self {
            config,
            client: Client::new(),
            connected: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    async fn call_action(&self, action: &str, params: Value) -> Result<Value, GatewayError> {
        let url = format!("{}/{}", self.config.http_url.trim_end_matches('/'), action);
        let mut request = self.client.post(url).json(&params);
        if !self.config.access_token.is_empty() {
            request = request.bearer_auth(&self.config.access_token);
        }
        let response = tokio::time::timeout(Duration::from_secs(15), request.send())
            .await
            .map_err(|_| GatewayError::UnknownOutcome)?
            .map_err(|_| GatewayError::UnknownOutcome)?;
        let status = response.status();
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES)
        {
            return Err(if status.is_client_error() {
                GatewayError::Request(format!(
                    "OneBot {action} returned an oversized client-error response"
                ))
            } else {
                GatewayError::UnknownOutcome
            });
        }
        let body = response
            .bytes()
            .await
            .map_err(|_| GatewayError::UnknownOutcome)?;
        if body.len() as u64 > MAX_RESPONSE_BYTES {
            return Err(GatewayError::UnknownOutcome);
        }
        let body: Value =
            serde_json::from_slice(&body).map_err(|_| GatewayError::UnknownOutcome)?;
        if !status.is_success() {
            return Err(
                if status == StatusCode::UNAUTHORIZED || status.is_client_error() {
                    GatewayError::Request(format!("OneBot {action} HTTP {status}"))
                } else {
                    GatewayError::UnknownOutcome
                },
            );
        }
        let failed = body
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|value| value != "ok" && value != "async")
            || body.get("status").is_none()
                && body
                    .get("retcode")
                    .and_then(Value::as_i64)
                    .is_some_and(|code| code != 0);
        if failed {
            return Err(GatewayError::Request(
                body.get("wording")
                    .and_then(Value::as_str)
                    .unwrap_or("OneBot rejected request")
                    .to_owned(),
            ));
        }
        Ok(body.get("data").cloned().unwrap_or(Value::Null))
    }

    pub async fn get_login_info(&self) -> Result<LoginInfo, GatewayError> {
        let data = self.call_action("get_login_info", json!({})).await?;
        let user_id = data
            .get("user_id")
            .map(ToString::to_string)
            .unwrap_or_default()
            .trim_matches('"')
            .to_owned();
        let nickname = data
            .get("nickname")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if user_id.is_empty() {
            return Err(GatewayError::Request(
                "OneBot get_login_info returned no user_id".into(),
            ));
        }
        Ok(LoginInfo { user_id, nickname })
    }

    pub async fn run(&self, events: mpsc::Sender<DomainEvent>) -> Result<(), NapcatError> {
        let request = {
            let mut request = http::Request::builder().uri(&self.config.ws_url);
            if !self.config.access_token.is_empty() {
                request = request.header(
                    "Authorization",
                    format!("Bearer {}", self.config.access_token),
                );
            }
            request
                .body(())
                .map_err(|error| NapcatError::WebSocket(error.to_string()))?
        };
        let (socket, _) = connect_async(request)
            .await
            .map_err(|error| NapcatError::WebSocket(error.to_string()))?;
        self.connected.store(true, Ordering::Release);
        if let Err(error) = self.get_login_info().await {
            self.connected.store(false, Ordering::Release);
            return Err(NapcatError::Health(error.to_string()));
        }
        let (mut sink, mut stream) = socket.split();
        let mut heartbeat = tokio::time::interval(Duration::from_secs(30));
        loop {
            tokio::select! {
                _ = heartbeat.tick() => {
                    sink.send(Message::Ping(Vec::new().into())).await.map_err(|error| NapcatError::WebSocket(error.to_string()))?;
                }
                item = stream.next() => {
                    let Some(item) = item else { break; };
            match item {
                Ok(message) if message.is_text() => {
                    let raw: Value = serde_json::from_str(message.to_text().unwrap_or("{}"))
                        .map_err(|error| NapcatError::WebSocket(error.to_string()))?;
                    if let Some(event) = normalize_event(&raw) {
                        events.send(event).await.map_err(|_| NapcatError::EventChannelClosed)?;
                    }
                }
                Ok(message) if message.is_close() => break,
                Ok(_) => {}
                Err(error) => return Err(NapcatError::WebSocket(error.to_string())),
            }
                }
            }
        }
        self.connected.store(false, Ordering::Release);
        Err(NapcatError::StreamEnded)
    }
}

#[async_trait]
impl MessageGateway for NapcatAdapter {
    async fn get_login_info(&self) -> Result<LoginInfo, GatewayError> {
        NapcatAdapter::get_login_info(self).await
    }

    async fn send_text(
        &self,
        conversation_id: &str,
        text: &str,
    ) -> Result<SendReceipt, GatewayError> {
        if self.config.observe_only {
            return Err(GatewayError::WritesDisabled);
        }
        if !self.connected() {
            return Err(GatewayError::NotConnected);
        }
        let (kind, id) = conversation_id
            .split_once(':')
            .ok_or_else(|| GatewayError::Request("invalid conversation id".into()))?;
        let mut params = json!({ "message": [{ "type": "text", "data": { "text": text } }] });
        match kind {
            "group" => params["group_id"] = Value::String(id.into()),
            "private" => params["user_id"] = Value::String(id.into()),
            _ => return Err(GatewayError::Request("invalid conversation kind".into())),
        }
        let body = self
            .call_action(&format!("send_{kind}_msg"), params)
            .await?;
        let message_id = body
            .get("message_id")
            .map(ToString::to_string)
            .unwrap_or_default();
        Ok(SendReceipt { message_id })
    }
}

pub fn normalize_event(raw: &Value) -> Option<DomainEvent> {
    let post_type = raw
        .get("post_type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if post_type != "message" {
        return Some(DomainEvent::Unknown {
            post_type: post_type.to_owned(),
            event_type: raw
                .get("meta_event_type")
                .or_else(|| raw.get("notice_type"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        });
    }
    let message_type = raw.get("message_type").and_then(Value::as_str)?;
    let (is_group, peer_id) = match message_type {
        "group" => (true, raw.get("group_id")?.to_string()),
        "private" => (false, raw.get("user_id")?.to_string()),
        _ => return None,
    };
    let peer_id = peer_id.trim_matches('"').to_owned();
    let sender_id = raw
        .get("sender")
        .and_then(|sender| sender.get("user_id"))
        .or_else(|| raw.get("user_id"))?
        .to_string()
        .trim_matches('"')
        .to_owned();
    let message_id = raw
        .get("message_id")?
        .to_string()
        .trim_matches('"')
        .to_owned();
    let segments = match raw.get("message") {
        Some(Value::Array(items)) => items.iter().map(normalize_segment).collect(),
        Some(Value::String(text)) => vec![MessageSegment::Text { text: text.clone() }],
        _ => vec![MessageSegment::Unknown {
            kind: "missing_message".into(),
        }],
    };
    let timestamp = raw
        .get("time")
        .and_then(Value::as_i64)
        .unwrap_or_default()
        .saturating_mul(1000);
    Some(DomainEvent::Message(IncomingMessage {
        message_id,
        conversation_id: format!("{}:{}", if is_group { "group" } else { "private" }, peer_id),
        sender_id,
        is_group,
        segments,
        timestamp,
    }))
}

fn normalize_segment(raw: &Value) -> MessageSegment {
    let kind = raw.get("type").and_then(Value::as_str).unwrap_or("unknown");
    let data = raw.get("data").cloned().unwrap_or(Value::Null);
    let string = |key: &str| {
        data.get(key)
            .map(ToString::to_string)
            .unwrap_or_default()
            .trim_matches('"')
            .to_owned()
    };
    match kind {
        "text" => MessageSegment::Text {
            text: string("text"),
        },
        "at" => MessageSegment::Mention {
            user_id: string("qq"),
        },
        "image" => MessageSegment::Image {
            file: string("file"),
            url: string("url"),
        },
        "record" | "voice" => MessageSegment::Audio {
            file: string("file"),
            url: string("url"),
        },
        "video" => MessageSegment::Video {
            file: string("file"),
            url: string("url"),
        },
        "reply" => MessageSegment::Reply {
            message_id: string("id"),
        },
        _ => MessageSegment::Unknown {
            kind: kind.to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_private_text_message_without_exposing_raw_onebot_shape() {
        let raw = json!({
            "post_type": "message",
            "message_type": "private",
            "message_id": 12,
            "user_id": 34,
            "time": 1700000000,
            "message": [{"type":"text", "data":{"text":"hello"}}]
        });
        let DomainEvent::Message(message) = normalize_event(&raw).expect("message") else {
            panic!("not a message")
        };
        assert_eq!(message.conversation_id, "private:34");
        assert_eq!(message.text_projection(), "hello");
    }
}
