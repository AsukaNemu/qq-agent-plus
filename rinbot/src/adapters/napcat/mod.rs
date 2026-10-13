use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use async_trait::async_trait;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use reqwest::{Client, StatusCode};
use serde_json::{json, Value};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::{io::AsyncReadExt as TokioAsyncReadExt, process::Command as TokioCommand};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, Message},
};

use crate::{
    domain::{
        event::{DomainEvent, FriendRequestFlag, FriendSummary},
        message::{IncomingMessage, MessageSegment},
    },
    ports::gateway::{
        DailyMomentImageReader, FavoriteAddReceipt, ForwardMessageNode, FriendRequestGateway,
        GatewayError, GroupRequestGateway, LoginInfo, MessageGateway, QzoneGateway,
        QzoneReadAction, QzoneWriteAction, SendReceipt, StickerGateway,
    },
};

const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 7 * 1024 * 1024;
const MAX_DOWNSAMPLE_INPUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_UPLOAD_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_VIDEO_BYTES: usize = 96 * 1024 * 1024;
const MAX_FORWARD_MEDIA_NODES: usize = 30;

fn friend_list_snapshot_event(result: Result<Vec<FriendSummary>, GatewayError>) -> DomainEvent {
    match result {
        Ok(friends) => DomainEvent::FriendListSnapshot { friends },
        Err(_) => DomainEvent::FriendListSnapshotUnavailable,
    }
}

struct ConnectionGuard(Arc<AtomicBool>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

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
    identity_sync_enabled: AtomicBool,
    group_name_cache: std::sync::Mutex<HashMap<String, String>>,
}

impl NapcatAdapter {
    pub fn new(config: NapcatConfig) -> Self {
        Self {
            config,
            client: Client::new(),
            connected: Arc::new(AtomicBool::new(false)),
            identity_sync_enabled: AtomicBool::new(false),
            group_name_cache: std::sync::Mutex::new(HashMap::new()),
        }
    }

    pub fn set_identity_sync_enabled(&self, enabled: bool) {
        self.identity_sync_enabled.store(enabled, Ordering::Release);
    }

    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    async fn call_action(&self, action: &str, params: Value) -> Result<Value, GatewayError> {
        self.call_action_with_timeout(action, params, Duration::from_secs(15))
            .await
    }

    async fn call_action_with_timeout(
        &self,
        action: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, GatewayError> {
        let url = format!("{}/{}", self.config.http_url.trim_end_matches('/'), action);
        let mut request = self.client.post(url).json(&params);
        if !self.config.access_token.is_empty() {
            request = request.bearer_auth(&self.config.access_token);
        }
        let response = tokio::time::timeout(timeout, request.send())
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
        let status_value = body.get("status").and_then(Value::as_str);
        let return_code = body.get("retcode").and_then(Value::as_i64);
        let confirmed_success = (status_value == Some("ok")
            && return_code.is_none_or(|code| code == 0))
            || status_value.is_none() && return_code == Some(0);
        if !confirmed_success {
            if status_value == Some("async") {
                return Err(GatewayError::UnknownOutcome);
            }
            if status_value.is_none() && return_code.is_none() {
                return Err(GatewayError::UnknownOutcome);
            }
            if return_code == Some(200) {
                return Err(GatewayError::UnsupportedAction(action.to_owned()));
            }
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

    async fn message_forward(
        &self,
        conversation_id: &str,
        message_id: &str,
    ) -> Result<Vec<ForwardMessageNode>, GatewayError> {
        if !self.connected() {
            return Err(GatewayError::NotConnected);
        }
        let (kind, peer_id) = conversation_id
            .split_once(':')
            .ok_or_else(|| GatewayError::Request("会话编号无效".into()))?;
        let message_id = message_id.trim();
        if !matches!(kind, "group" | "private")
            || !valid_qq_id(peer_id)
            || message_id.is_empty()
            || message_id.len() > 64
            || !message_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'-')
        {
            return Err(GatewayError::Request("转发消息编号无效".into()));
        }
        let numeric_message_id = message_id
            .parse::<i64>()
            .map_err(|_| GatewayError::Request("转发消息编号无效".into()))?;
        let mut resource_id = None;
        let fetched_message = self
            .call_action("get_msg", json!({"message_id":numeric_message_id}))
            .await;
        if let Ok(message) = fetched_message {
            let returned_id = message.get("message_id").map(value_to_string);
            let returned_kind = message
                .get("message_type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let returned_peer = if kind == "group" {
                message.get("group_id").map(value_to_string)
            } else {
                message
                    .pointer("/sender/user_id")
                    .or_else(|| message.get("user_id"))
                    .map(value_to_string)
            }
            .unwrap_or_default();
            if returned_id.as_deref() != Some(message_id)
                || returned_kind != kind
                || returned_peer != peer_id
            {
                return Err(GatewayError::Request("消息不属于当前会话".into()));
            }
            resource_id = message
                .get("message")
                .and_then(Value::as_array)
                .and_then(|segments| {
                    segments.iter().find_map(|segment| {
                        (segment.get("type").and_then(Value::as_str) == Some("forward"))
                            .then(|| segment.pointer("/data/id").map(value_to_string))
                            .flatten()
                    })
                })
                .filter(|id| !id.trim().is_empty() && id.len() <= 2048);
        }

        let mut result = None;
        if let Some(resource_id) = resource_id {
            if let Ok(data) = self
                .call_action("get_forward_msg", json!({"id":resource_id}))
                .await
            {
                result = forward_nodes(&data);
            }
        }
        if result.as_ref().is_none_or(Vec::is_empty) {
            let data = self
                .call_action("get_forward_msg", json!({"message_id":numeric_message_id}))
                .await?;
            result = forward_nodes(&data);
        }
        result
            .filter(|nodes| !nodes.is_empty())
            .ok_or_else(|| GatewayError::Request("转发内容为空或已过期".into()))
    }

    pub async fn get_group_name(&self, group_id: &str) -> Result<Option<String>, GatewayError> {
        let group_id = group_id.trim();
        if group_id.is_empty()
            || group_id.len() > 15
            || !group_id.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(GatewayError::Request("invalid group identifier".into()));
        }
        if let Some(name) = self
            .group_name_cache
            .lock()
            .ok()
            .and_then(|cache| cache.get(group_id).cloned())
        {
            return Ok(Some(name));
        }
        let numeric_group_id = group_id
            .parse::<u64>()
            .map_err(|_| GatewayError::Request("invalid group identifier".into()))?;
        let data = self
            .call_action_with_timeout(
                "get_group_info",
                json!({"group_id":numeric_group_id}),
                Duration::from_secs(5),
            )
            .await?;
        let name = data
            .get("group_name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(|name| name.chars().take(128).collect::<String>());
        if let Some(name) = name.as_ref() {
            if let Ok(mut cache) = self.group_name_cache.lock() {
                cache.insert(group_id.to_owned(), name.clone());
            }
        }
        Ok(name)
    }

    pub async fn get_friend_list(&self) -> Result<Vec<FriendSummary>, GatewayError> {
        let data = self.call_action("get_friend_list", json!({})).await?;
        let Some(items) = data.as_array() else {
            return Err(GatewayError::Request(
                "OneBot get_friend_list returned an invalid payload".into(),
            ));
        };
        let mut friends = Vec::with_capacity(items.len().min(50_000));
        for item in items.iter().take(50_000) {
            let user_id = item
                .get("user_id")
                .or_else(|| item.get("uin"))
                .map(value_to_string)
                .unwrap_or_default();
            if user_id.is_empty()
                || user_id.len() > 15
                || !user_id.bytes().all(|byte| byte.is_ascii_digit())
            {
                continue;
            }
            let nickname = item
                .get("remark")
                .or_else(|| item.get("nickname"))
                .or_else(|| item.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            friends.push(FriendSummary { user_id, nickname });
        }
        if !items.is_empty() && friends.is_empty() {
            return Err(GatewayError::Request(
                "OneBot get_friend_list contained no valid QQ identifiers".into(),
            ));
        }
        Ok(friends)
    }

    async fn image_data_url(&self, file: &str) -> Result<Option<String>, GatewayError> {
        let file = file.trim();
        if file.is_empty() || file.len() > 2048 {
            return Ok(None);
        }
        let data = self.call_action("get_image", json!({"file":file})).await?;
        let Some(path) = data.get("file").and_then(Value::as_str) else {
            return Ok(None);
        };
        image_path_to_data_url(path).await
    }

    async fn message_images(
        &self,
        conversation_id: &str,
        message_id: &str,
    ) -> Result<Vec<String>, GatewayError> {
        let files = self
            .message_image_files(conversation_id, message_id)
            .await?;
        let mut images = Vec::with_capacity(files.len());
        for file in files {
            if let Some(data_url) = self.image_data_url(&file).await? {
                images.push(data_url);
            }
        }
        for url in self.message_video_urls(conversation_id, message_id).await? {
            if images.len() >= 4 {
                break;
            }
            if let Ok(bytes) =
                crate::application::web_tools::safe_fetch_binary(&url, MAX_VIDEO_BYTES).await
            {
                if let Ok(Some(frame_strip)) = video_to_frame_strip(&bytes, &resolve_ffmpeg()).await
                {
                    images.push(frame_strip);
                }
            }
        }
        Ok(images)
    }

    async fn message_video_urls(
        &self,
        conversation_id: &str,
        message_id: &str,
    ) -> Result<Vec<String>, GatewayError> {
        let (kind, peer_id) = conversation_id
            .split_once(':')
            .ok_or_else(|| GatewayError::Request("会话编号无效".into()))?;
        let message_id = message_id.trim();
        if !matches!(kind, "group" | "private")
            || !valid_qq_id(peer_id)
            || message_id.is_empty()
            || message_id.len() > 64
            || !message_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'-')
        {
            return Err(GatewayError::Request("消息编号无效".into()));
        }
        let numeric_id = message_id
            .parse::<i64>()
            .map_err(|_| GatewayError::Request("消息编号无效".into()))?;
        let response = self
            .call_action("get_msg", json!({"message_id":numeric_id}))
            .await?;
        verify_message_ownership(&response, kind, peer_id, message_id)?;
        let segments = response
            .get("message")
            .and_then(Value::as_array)
            .ok_or_else(|| GatewayError::Request("协议端未返回消息内容".into()))?;
        let mut urls = video_urls_from_segments(segments);
        if urls.is_empty() {
            if let Some(resource_id) = forward_resource_id(segments) {
                if let Ok(data) = self
                    .call_action("get_forward_msg", json!({"id":resource_id}))
                    .await
                {
                    urls = forward_audio_candidates(&data)
                        .into_iter()
                        .filter(|candidate| candidate.0 == "video")
                        .map(|candidate| candidate.2)
                        .filter(|url| !url.is_empty())
                        .take(4)
                        .collect();
                }
            }
            if urls.is_empty() {
                if let Ok(data) = self
                    .call_action("get_forward_msg", json!({"message_id":numeric_id}))
                    .await
                {
                    urls = forward_audio_candidates(&data)
                        .into_iter()
                        .filter(|candidate| candidate.0 == "video")
                        .map(|candidate| candidate.2)
                        .filter(|url| !url.is_empty())
                        .take(4)
                        .collect();
                }
            }
        }
        Ok(urls)
    }

    async fn message_image_files(
        &self,
        conversation_id: &str,
        message_id: &str,
    ) -> Result<Vec<String>, GatewayError> {
        if !self.connected() {
            return Err(GatewayError::NotConnected);
        }
        let (conversation_kind, conversation_uin) = conversation_id
            .split_once(':')
            .ok_or_else(|| GatewayError::Request("会话编号无效".into()))?;
        if !matches!(conversation_kind, "private" | "group")
            || conversation_uin.is_empty()
            || conversation_uin.len() > 15
            || !conversation_uin.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(GatewayError::Request("会话编号无效".into()));
        }
        let message_id = message_id.trim();
        if message_id.is_empty()
            || message_id.len() > 64
            || !message_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'-')
        {
            return Err(GatewayError::Request("消息编号无效".into()));
        }
        let raw_id = message_id
            .parse::<i64>()
            .map_err(|_| GatewayError::Request("消息编号无效".into()))?;
        let response = self
            .call_action("get_msg", json!({"message_id":raw_id}))
            .await?;
        let actual_message_id = response.get("message_id").map(value_to_string);
        let actual_kind = response
            .get("message_type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let actual_uin = if conversation_kind == "group" {
            response.get("group_id").map(value_to_string)
        } else {
            response
                .pointer("/sender/user_id")
                .or_else(|| response.get("user_id"))
                .map(value_to_string)
        }
        .unwrap_or_default();
        if actual_message_id.as_deref() != Some(message_id)
            || actual_kind != conversation_kind
            || actual_uin != conversation_uin
        {
            return Err(GatewayError::Request("消息不属于当前会话".into()));
        }
        let segments = response
            .get("message")
            .and_then(Value::as_array)
            .ok_or_else(|| GatewayError::Request("协议端未返回消息内容".into()))?;
        let mut files = Vec::new();
        for segment in segments {
            if segment.get("type").and_then(Value::as_str) != Some("image") {
                continue;
            }
            let file = segment
                .pointer("/data/file")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim();
            if !file.is_empty() && file.len() <= 2048 && !files.iter().any(|seen| seen == file) {
                files.push(file.to_owned());
                if files.len() == 4 {
                    break;
                }
            }
        }
        Ok(files)
    }

    async fn image_upload_source(&self, file: &str) -> Result<Option<String>, GatewayError> {
        let file = file.trim();
        if file.is_empty() || file.len() > 2048 {
            return Ok(None);
        }
        let data = self.call_action("get_image", json!({"file":file})).await?;
        let Some(path) = data.get("file").and_then(Value::as_str) else {
            return Ok(None);
        };
        image_path_to_upload_source(path).await
    }

    async fn message_audio(
        &self,
        conversation_id: &str,
        message_id: &str,
    ) -> Result<Option<(String, Vec<u8>)>, GatewayError> {
        if !self.connected() {
            return Err(GatewayError::NotConnected);
        }
        let (kind, raw_conversation_id) = conversation_id
            .split_once(':')
            .ok_or_else(|| GatewayError::Request("会话编号无效".into()))?;
        let id = raw_conversation_id
            .parse::<u64>()
            .map_err(|_| GatewayError::Request("会话编号无效".into()))?;
        if !matches!(kind, "private" | "group")
            || !valid_qq_id(raw_conversation_id)
            || message_id.is_empty()
            || message_id.len() > 64
            || !message_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'-')
        {
            return Err(GatewayError::Request("消息编号无效".into()));
        }
        let raw_id = message_id
            .parse::<i64>()
            .map_err(|_| GatewayError::Request("消息编号无效".into()))?;
        let response = self
            .call_action("get_msg", json!({"message_id":raw_id}))
            .await?;
        verify_message_ownership(&response, kind, raw_conversation_id, message_id)?;
        let segments = response
            .get("message")
            .and_then(Value::as_array)
            .ok_or_else(|| GatewayError::Request("协议端未返回消息内容".into()))?;
        let mut candidate = segments.iter().find_map(|segment| {
            let segment_kind = segment.get("type").and_then(Value::as_str)?;
            let data = segment.get("data")?;
            let name = data
                .get("file")
                .or_else(|| data.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let url = data
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            match segment_kind {
                "record" | "voice" | "video" => Some((segment_kind, name, url, String::new())),
                "file" => {
                    let file_id = data
                        .get("file_id")
                        .or_else(|| data.get("id"))
                        .map(value_to_string)
                        .unwrap_or_default();
                    let looks_audio = [
                        ".m4a", ".mp3", ".wav", ".amr", ".aac", ".ogg", ".flac", ".wma", ".mp4",
                        ".mov", ".avi", ".mkv", ".webm", ".silk",
                    ]
                    .iter()
                    .any(|extension| name.to_ascii_lowercase().ends_with(extension));
                    (looks_audio || !file_id.is_empty()).then_some((
                        segment_kind,
                        name,
                        url,
                        file_id,
                    ))
                }
                _ => None,
            }
        });
        if candidate.is_none() {
            if let Some(resource_id) = forward_resource_id(segments) {
                let expanded = self
                    .call_action("get_forward_msg", json!({"id":resource_id}))
                    .await
                    .ok();
                if let Some(data) = expanded {
                    let candidates = forward_audio_candidates(&data);
                    candidate = candidates
                        .iter()
                        .find(|candidate| !candidate.2.is_empty())
                        .cloned()
                        .or_else(|| candidates.into_iter().next());
                }
            }
            if candidate.is_none() {
                if let Some(data) = self
                    .call_action("get_forward_msg", json!({"message_id":raw_id}))
                    .await
                    .ok()
                {
                    let candidates = forward_audio_candidates(&data);
                    candidate = candidates
                        .iter()
                        .find(|candidate| !candidate.2.is_empty())
                        .cloned()
                        .or_else(|| candidates.into_iter().next());
                }
            }
        }
        let Some((segment_kind, name, mut url, file_id)) = candidate else {
            return Ok(None);
        };
        if !url.starts_with("http://") && !url.starts_with("https://") {
            url.clear();
        }
        if url.is_empty() && matches!(segment_kind, "record" | "voice") && !name.is_empty() {
            let data = self.call_action("get_record", json!({"file":name})).await?;
            url = data
                .get("file")
                .or_else(|| data.get("url"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
        }
        if url.is_empty() && segment_kind == "file" && !file_id.is_empty() {
            let action = if kind == "private" {
                "get_private_file_url"
            } else {
                "get_group_file_url"
            };
            let params = if kind == "private" {
                json!({"user_id":id,"file_id":file_id})
            } else {
                json!({"group_id":id,"file_id":file_id})
            };
            let data = self.call_action(action, params).await?;
            url = data
                .get("url")
                .or_else(|| data.get("data").and_then(|value| value.get("url")))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
        }
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(GatewayError::Request(
                "协议端没有提供可下载的语音、音频或视频地址".into(),
            ));
        }
        let bytes = crate::application::web_tools::safe_fetch_binary(
            &url,
            crate::application::asr::MAX_AUDIO_BYTES,
        )
        .await
        .map_err(GatewayError::Request)?;
        Ok(Some((name, bytes)))
    }

    pub async fn decide_friend_request(
        &self,
        request_flag: &str,
        approve: bool,
        remark: Option<&str>,
    ) -> Result<(), GatewayError> {
        if self.config.observe_only {
            return Err(GatewayError::WritesDisabled);
        }
        if !self.connected() {
            return Err(GatewayError::NotConnected);
        }
        let request_flag = request_flag.trim();
        if request_flag.is_empty() || request_flag.len() > 500 {
            return Err(GatewayError::Request("invalid friend-request flag".into()));
        }
        let remark = remark
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let remark = remark.chars().take(60).collect::<String>();
        let mut params = json!({ "flag": request_flag, "approve": approve });
        if approve && !remark.is_empty() {
            params["remark"] = Value::String(remark);
        }
        self.call_action("set_friend_add_request", params).await?;
        Ok(())
    }

    pub async fn decide_group_request(
        &self,
        request_flag: &str,
        sub_type: &str,
        approve: bool,
    ) -> Result<(), GatewayError> {
        if self.config.observe_only {
            return Err(GatewayError::WritesDisabled);
        }
        if !self.connected() {
            return Err(GatewayError::NotConnected);
        }
        let request_flag = request_flag.trim();
        if request_flag.is_empty() || request_flag.len() > 500 {
            return Err(GatewayError::Request("invalid group-request flag".into()));
        }
        if !matches!(sub_type, "invite" | "add") {
            return Err(GatewayError::Request(
                "invalid group-request subtype".into(),
            ));
        }
        self.call_action(
            "set_group_add_request",
            json!({"flag":request_flag,"sub_type":sub_type,"approve":approve}),
        )
        .await?;
        Ok(())
    }

    pub async fn run(&self, events: mpsc::Sender<DomainEvent>) -> Result<(), NapcatError> {
        self.run_with_heartbeat(events, Duration::from_secs(30))
            .await
    }

    async fn run_with_heartbeat(
        &self,
        events: mpsc::Sender<DomainEvent>,
        heartbeat_period: Duration,
    ) -> Result<(), NapcatError> {
        let mut request = self
            .config
            .ws_url
            .as_str()
            .into_client_request()
            .map_err(|error| NapcatError::WebSocket(error.to_string()))?;
        if !self.config.access_token.is_empty() {
            let authorization = format!("Bearer {}", self.config.access_token)
                .parse()
                .map_err(|error| {
                    NapcatError::WebSocket(format!("invalid OneBot token: {error}"))
                })?;
            request
                .headers_mut()
                .insert(http::header::AUTHORIZATION, authorization);
        }
        let (socket, _) = connect_async(request)
            .await
            .map_err(|error| NapcatError::WebSocket(error.to_string()))?;
        self.connected.store(true, Ordering::Release);
        let _connection_guard = ConnectionGuard(self.connected.clone());
        let login_info = self
            .get_login_info()
            .await
            .map_err(|error| NapcatError::Health(error.to_string()))?;
        if self.identity_sync_enabled.load(Ordering::Acquire) {
            let event = friend_list_snapshot_event(self.get_friend_list().await);
            events
                .send(event)
                .await
                .map_err(|_| NapcatError::EventChannelClosed)?;
        }
        let (mut sink, mut stream) = socket.split();
        let mut heartbeat = tokio::time::interval_at(
            tokio::time::Instant::now() + heartbeat_period,
            heartbeat_period,
        );
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
                    if let Some(mut event) = normalize_event(&raw) {
                        if let DomainEvent::GroupIncreased { user_id, is_bot, .. } = &mut event {
                            *is_bot = user_id == &login_info.user_id;
                        }
                        if let DomainEvent::GroupDecreased { user_id, is_bot, .. } = &mut event {
                            *is_bot = user_id == &login_info.user_id;
                        }
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
        Err(NapcatError::StreamEnded)
    }
}

#[async_trait]
impl MessageGateway for NapcatAdapter {
    async fn get_login_info(&self) -> Result<LoginInfo, GatewayError> {
        NapcatAdapter::get_login_info(self).await
    }

    async fn recalled_message_media(
        &self,
        conversation_id: &str,
        message_id: &str,
    ) -> Result<Option<Vec<MessageSegment>>, GatewayError> {
        let (kind, peer_id) = conversation_id
            .split_once(':')
            .ok_or_else(|| GatewayError::Request("会话编号无效".into()))?;
        if !matches!(kind, "group" | "private")
            || !valid_qq_id(peer_id)
            || message_id.is_empty()
            || message_id.len() > 64
            || !message_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'-')
        {
            return Err(GatewayError::Request("撤回消息编号无效".into()));
        }
        let numeric_id = message_id
            .parse::<i64>()
            .map_err(|_| GatewayError::Request("撤回消息编号无效".into()))?;
        let response = self
            .call_action("get_msg", json!({"message_id":numeric_id}))
            .await?;
        verify_message_ownership(&response, kind, peer_id, message_id)?;
        let segments = response
            .get("message")
            .and_then(Value::as_array)
            .ok_or_else(|| GatewayError::Request("协议端未返回消息内容".into()))?
            .iter()
            .map(normalize_segment)
            .filter(|segment| {
                matches!(
                    segment,
                    MessageSegment::Image { .. }
                        | MessageSegment::Audio { .. }
                        | MessageSegment::Video { .. }
                        | MessageSegment::Face { .. }
                )
            })
            .collect::<Vec<_>>();
        Ok(Some(segments))
    }

    async fn recalled_image_alternatives(&self, file: &str) -> Result<Vec<String>, GatewayError> {
        if file.trim().is_empty() || file.len() > 2048 || file.chars().any(char::is_control) {
            return Err(GatewayError::Request("撤回图片引用无效".into()));
        }
        let response = self.call_action("get_image", json!({"file":file})).await?;
        let mut alternatives = Vec::new();
        for object in [Some(&response), response.get("data")]
            .into_iter()
            .flatten()
        {
            for key in ["file", "path", "url"] {
                let Some(value) = object.get(key).and_then(Value::as_str) else {
                    continue;
                };
                let value = value.trim();
                if !value.is_empty()
                    && value.len() <= 2048
                    && !value.chars().any(char::is_control)
                    && !alternatives.iter().any(|current| current == value)
                {
                    alternatives.push(value.to_owned());
                }
            }
        }
        alternatives.truncate(12);
        Ok(alternatives)
    }

    async fn get_group_name(&self, group_id: &str) -> Result<Option<String>, GatewayError> {
        NapcatAdapter::get_group_name(self, group_id).await
    }

    async fn image_data_url(&self, file: &str) -> Result<Option<String>, GatewayError> {
        NapcatAdapter::image_data_url(self, file).await
    }

    async fn message_images(
        &self,
        conversation_id: &str,
        message_id: &str,
    ) -> Result<Vec<String>, GatewayError> {
        NapcatAdapter::message_images(self, conversation_id, message_id).await
    }

    async fn message_audio(
        &self,
        conversation_id: &str,
        message_id: &str,
    ) -> Result<Option<(String, Vec<u8>)>, GatewayError> {
        NapcatAdapter::message_audio(self, conversation_id, message_id).await
    }

    async fn message_forward(
        &self,
        conversation_id: &str,
        message_id: &str,
    ) -> Result<Vec<ForwardMessageNode>, GatewayError> {
        NapcatAdapter::message_forward(self, conversation_id, message_id).await
    }

    async fn send_text(
        &self,
        conversation_id: &str,
        text: &str,
    ) -> Result<SendReceipt, GatewayError> {
        self.send_text_with_options(conversation_id, text, None, None)
            .await
    }

    async fn send_text_with_options(
        &self,
        conversation_id: &str,
        text: &str,
        reply_to_message_id: Option<&str>,
        at_user_id: Option<&str>,
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
        if reply_to_message_id.is_some_and(|id| !crate::domain::message::valid_quote_message_id(id))
            || at_user_id.is_some_and(|id| !valid_qq_id(id))
            || (reply_to_message_id.is_some() && at_user_id.is_some())
            || (at_user_id.is_some() && kind != "group")
        {
            return Err(GatewayError::Request(
                "invalid reply or mention target".into(),
            ));
        }
        let mut segments = Vec::new();
        if let Some(message_id) = reply_to_message_id {
            segments.push(json!({"type":"reply","data":{"id":message_id}}));
        }
        if let Some(user_id) = at_user_id {
            segments.push(json!({"type":"at","data":{"qq":user_id}}));
        }
        segments.push(json!({ "type": "text", "data": { "text": text } }));
        let mut params = json!({ "message": segments });
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
            .and_then(|message_id| {
                message_id
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| message_id.as_i64().map(|id| id.to_string()))
                    .or_else(|| message_id.as_u64().map(|id| id.to_string()))
            })
            .filter(|message_id| !message_id.trim().is_empty())
            .ok_or(GatewayError::UnknownOutcome)?;
        Ok(SendReceipt { message_id })
    }

    async fn send_poke(
        &self,
        conversation_id: &str,
        target_user_id: &str,
    ) -> Result<SendReceipt, GatewayError> {
        if self.config.observe_only {
            return Err(GatewayError::WritesDisabled);
        }
        if !self.connected() {
            return Err(GatewayError::NotConnected);
        }
        let (kind, peer_id) = conversation_id
            .split_once(':')
            .ok_or_else(|| GatewayError::Request("会话编号无效".into()))?;
        if !valid_qq_id(peer_id) || !valid_qq_id(target_user_id) {
            return Err(GatewayError::Request("拍一拍目标 QQ 号无效".into()));
        }
        let peer_id = peer_id
            .parse::<u64>()
            .map_err(|_| GatewayError::Request("会话编号无效".into()))?;
        let target = target_user_id
            .parse::<u64>()
            .map_err(|_| GatewayError::Request("拍一拍目标 QQ 号无效".into()))?;
        let (action, params) = match kind {
            "private" if peer_id == target => ("friend_poke", json!({"user_id":peer_id})),
            "private" => return Err(GatewayError::Request("私聊只能拍当前聊天对象".into())),
            "group" => ("group_poke", json!({"group_id":peer_id,"user_id":target})),
            _ => return Err(GatewayError::Request("会话类型无效".into())),
        };
        let data = self.call_action(action, params).await?;
        let message_id = data
            .get("message_id")
            .and_then(|message_id| {
                message_id
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| message_id.as_i64().map(|id| id.to_string()))
                    .or_else(|| message_id.as_u64().map(|id| id.to_string()))
            })
            .unwrap_or_default();
        Ok(SendReceipt { message_id })
    }

    async fn send_face(
        &self,
        conversation_id: &str,
        face_id: &str,
        face_name: &str,
        text: Option<&str>,
        reply_to_message_id: Option<&str>,
        at_user_id: Option<&str>,
    ) -> Result<SendReceipt, GatewayError> {
        if self.config.observe_only {
            return Err(GatewayError::WritesDisabled);
        }
        if !self.connected() {
            return Err(GatewayError::NotConnected);
        }
        if face_id.is_empty()
            || face_id.len() > 10
            || !face_id.bytes().all(|byte| byte.is_ascii_digit())
            || face_name.len() > 64
            || face_name.chars().any(char::is_control)
            || text.is_some_and(|value| value.len() > 100_000)
        {
            return Err(GatewayError::Request("QQ 系统表情参数无效".into()));
        }
        let (kind, id) = conversation_id
            .split_once(':')
            .ok_or_else(|| GatewayError::Request("invalid conversation id".into()))?;
        if !valid_qq_id(id)
            || !matches!(kind, "group" | "private")
            || reply_to_message_id.is_some_and(|message_id| {
                !crate::domain::message::valid_quote_message_id(message_id)
            })
            || at_user_id.is_some_and(|user_id| !valid_qq_id(user_id))
            || (at_user_id.is_some() && kind != "group")
        {
            return Err(GatewayError::Request("invalid face message target".into()));
        }
        let mut segments = Vec::new();
        if let Some(message_id) = reply_to_message_id {
            segments.push(json!({"type":"reply","data":{"id":message_id}}));
        }
        if let Some(user_id) = at_user_id {
            segments.push(json!({"type":"at","data":{"qq":user_id}}));
        }
        if let Some(text) = text.filter(|text| !text.trim().is_empty()) {
            segments.push(json!({"type":"text","data":{"text":text}}));
        }
        segments.push(json!({"type":"face","data":{"id":face_id}}));
        let mut params = json!({"message":segments});
        match kind {
            "group" => params["group_id"] = Value::String(id.into()),
            "private" => params["user_id"] = Value::String(id.into()),
            _ => unreachable!(),
        }
        let body = self
            .call_action(&format!("send_{kind}_msg"), params)
            .await?;
        let message_id = body
            .get("message_id")
            .map(value_to_string)
            .filter(|message_id| !message_id.trim().is_empty())
            .ok_or(GatewayError::UnknownOutcome)?;
        Ok(SendReceipt { message_id })
    }

    async fn send_image(
        &self,
        conversation_id: &str,
        base64_image: &str,
    ) -> Result<SendReceipt, GatewayError> {
        if self.config.observe_only {
            return Err(GatewayError::WritesDisabled);
        }
        if !self.connected() {
            return Err(GatewayError::NotConnected);
        }
        let encoded = base64_image
            .strip_prefix("base64://")
            .ok_or_else(|| GatewayError::Request("图片必须是本地 base64:// 数据".into()))?;
        if encoded.len() > MAX_UPLOAD_IMAGE_BYTES.saturating_mul(4).div_ceil(3) {
            return Err(GatewayError::Request("图片超过 8 MiB 限制".into()));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| GatewayError::Request("图片 base64 数据无效".into()))?;
        if bytes.is_empty()
            || bytes.len() > MAX_UPLOAD_IMAGE_BYTES
            || (image_mime(&bytes).is_none() && image_gif(&bytes).is_none())
        {
            return Err(GatewayError::Request(
                "图片为空、格式无效或超过 8 MiB".into(),
            ));
        }
        let (kind, id) = conversation_id
            .split_once(':')
            .ok_or_else(|| GatewayError::Request("invalid conversation id".into()))?;
        let mut params = json!({
            "message": [{ "type": "image", "data": { "file": format!("base64://{encoded}") } }]
        });
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
            .and_then(|message_id| {
                message_id
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| message_id.as_i64().map(|id| id.to_string()))
                    .or_else(|| message_id.as_u64().map(|id| id.to_string()))
            })
            .filter(|message_id| !message_id.trim().is_empty())
            .ok_or(GatewayError::UnknownOutcome)?;
        Ok(SendReceipt { message_id })
    }

    async fn send_media_segment(
        &self,
        conversation_id: &str,
        segment: &MessageSegment,
    ) -> Result<SendReceipt, GatewayError> {
        if self.config.observe_only {
            return Err(GatewayError::WritesDisabled);
        }
        if !self.connected() {
            return Err(GatewayError::NotConnected);
        }
        let (kind, data) = match segment {
            MessageSegment::Face { id } => {
                if id.is_empty() || id.len() > 10 || !id.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(GatewayError::Request(
                        "invalid OneBot face identifier".into(),
                    ));
                }
                ("face", json!({"id":id}))
            }
            MessageSegment::Image { file, url } => {
                let file = if !file.trim().is_empty() { file } else { url };
                validate_media_reference(file)?;
                ("image", json!({"file":file}))
            }
            MessageSegment::Audio { file, url } => {
                let file = if !file.trim().is_empty() { file } else { url };
                validate_media_reference(file)?;
                ("record", json!({"file":file}))
            }
            MessageSegment::Video { file, url } => {
                let file = if !file.trim().is_empty() { file } else { url };
                validate_media_reference(file)?;
                ("video", json!({"file":file}))
            }
            _ => return Err(GatewayError::UnsupportedAction("send_media_segment".into())),
        };
        let (chat_kind, id) = conversation_id
            .split_once(':')
            .ok_or_else(|| GatewayError::Request("invalid conversation id".into()))?;
        if !valid_qq_id(id) || !matches!(chat_kind, "group" | "private") {
            return Err(GatewayError::Request("invalid conversation id".into()));
        }
        let mut params = json!({"message":[{"type":kind,"data":data}]});
        match chat_kind {
            "group" => params["group_id"] = Value::String(id.into()),
            "private" => params["user_id"] = Value::String(id.into()),
            _ => unreachable!(),
        }
        let body = self
            .call_action(&format!("send_{chat_kind}_msg"), params)
            .await?;
        let message_id = body
            .get("message_id")
            .map(value_to_string)
            .filter(|message_id| !message_id.trim().is_empty())
            .ok_or(GatewayError::UnknownOutcome)?;
        Ok(SendReceipt { message_id })
    }
}

#[async_trait]
impl DailyMomentImageReader for NapcatAdapter {
    async fn daily_moment_image(
        &self,
        conversation_id: &str,
        message_id: &str,
        media_index: usize,
    ) -> Result<Option<String>, GatewayError> {
        if media_index >= 4 {
            return Err(GatewayError::Request("每日动态图片编号无效".into()));
        }
        Ok(
            NapcatAdapter::message_images(self, conversation_id, message_id)
                .await?
                .get(media_index)
                .cloned(),
        )
    }

    async fn daily_moment_upload_source(
        &self,
        conversation_id: &str,
        message_id: &str,
        media_index: usize,
    ) -> Result<Option<String>, GatewayError> {
        if media_index >= 4 {
            return Err(GatewayError::Request("每日动态图片编号无效".into()));
        }
        let files = self
            .message_image_files(conversation_id, message_id)
            .await?;
        let Some(file) = files.get(media_index) else {
            return Ok(None);
        };
        self.image_upload_source(file).await
    }
}

#[async_trait]
impl StickerGateway for NapcatAdapter {
    async fn fetch_qq_favorites(&self, limit: usize) -> Result<Vec<Value>, GatewayError> {
        if !self.connected() {
            return Err(GatewayError::NotConnected);
        }
        if !(1..=500).contains(&limit) {
            return Err(GatewayError::Request("QQ 收藏同步数量超出范围".into()));
        }
        let data = self
            .call_action_with_timeout(
                "fetch_custom_face_detail",
                json!({"count":limit}),
                Duration::from_secs(30),
            )
            .await?;
        let items = data
            .as_array()
            .or_else(|| data.get("data").and_then(Value::as_array))
            .ok_or_else(|| GatewayError::Request("NapCat 未返回 QQ 收藏列表".into()))?;
        if items.len() > limit {
            return Err(GatewayError::Request(
                "NapCat 返回的 QQ 收藏超过请求上限".into(),
            ));
        }
        Ok(items.clone())
    }

    async fn add_message_image_to_favorites(
        &self,
        conversation_id: &str,
        message_id: &str,
        media_index: usize,
    ) -> Result<FavoriteAddReceipt, GatewayError> {
        if self.config.observe_only {
            return Err(GatewayError::WritesDisabled);
        }
        if !self.connected() {
            return Err(GatewayError::NotConnected);
        }
        if media_index >= 4 {
            return Err(GatewayError::Request("收藏图片编号无效".into()));
        }
        let files = self
            .message_image_files(conversation_id, message_id)
            .await?;
        let Some(file) = files.get(media_index) else {
            return Err(GatewayError::Request("指定消息没有这张图片".into()));
        };
        // Let NapCat resolve the opaque OneBot file identifier inside its own
        // container; the application process must not write into QQ's sandbox.
        let downloaded = self
            .call_action_with_timeout("get_image", json!({"file":file}), Duration::from_secs(30))
            .await?;
        let container_path = downloaded
            .get("file")
            .or_else(|| downloaded.get("path"))
            .or_else(|| downloaded.get("file_path"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty() && path.len() <= 4096 && !path.contains('\0'))
            .ok_or_else(|| GatewayError::Request("NapCat 未返回可用的容器内图片路径".into()))?;
        let result = self
            .call_action_with_timeout(
                "add_custom_face",
                json!({"file":container_path}),
                Duration::from_secs(60),
            )
            .await?;
        let emoji_id = result
            .get("emoji_id")
            .or_else(|| result.get("resId"))
            .or_else(|| result.pointer("/data/emoji_id"))
            .map(value_to_string)
            .filter(|id| !id.is_empty() && id.len() <= 256)
            .unwrap_or_default();
        let explicitly_confirmed = result.get("result").and_then(Value::as_i64) == Some(0);
        if explicitly_confirmed || !emoji_id.is_empty() {
            return Ok(FavoriteAddReceipt { emoji_id });
        }
        if let Some(code) = result.get("result").and_then(Value::as_i64) {
            return Err(GatewayError::Request(format!(
                "NapCat rejected favorite write with result {code}"
            )));
        }
        Err(GatewayError::UnknownOutcome)
    }
}

async fn image_path_to_upload_source(path: &str) -> Result<Option<String>, GatewayError> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut image = options
        .open(path)
        .map_err(|_| GatewayError::Request("NapCat 图片文件不可读取".into()))?;
    let metadata = image
        .metadata()
        .map_err(|_| GatewayError::Request("NapCat 图片文件不可读取".into()))?;
    if !metadata.is_file() || metadata.len() > MAX_UPLOAD_IMAGE_BYTES as u64 {
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut image)
        .take((MAX_UPLOAD_IMAGE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| GatewayError::Request("NapCat 图片文件不可读取".into()))?;
    if bytes.len() > MAX_UPLOAD_IMAGE_BYTES
        || (image_mime(&bytes).is_none() && image_gif(&bytes).is_none())
    {
        return Ok(None);
    }
    Ok(Some(format!(
        "base64://{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )))
}

async fn image_path_to_data_url(path: &str) -> Result<Option<String>, GatewayError> {
    image_path_to_data_url_with_ffmpeg(path, &resolve_ffmpeg()).await
}

/// Convert bounded sticker bytes for vision inspection using the same GIF
/// contact-sheet/downsampling path as message images. Publication retains the
/// original bytes separately.
pub async fn image_bytes_to_vision_data_url(bytes: &[u8]) -> Result<Option<String>, GatewayError> {
    if bytes.is_empty() || bytes.len() > MAX_DOWNSAMPLE_INPUT_BYTES {
        return Ok(None);
    }
    let (mime, payload) = if image_gif(bytes).is_some() || bytes.len() > MAX_IMAGE_BYTES {
        let Some(jpeg) = downsample_image(bytes, &resolve_ffmpeg())
            .await
            .map_err(|_| GatewayError::Request("每日动态图片转换失败".into()))?
        else {
            return Ok(None);
        };
        ("image/jpeg", jpeg)
    } else if let Some(mime) = image_mime(bytes) {
        (mime, bytes.to_vec())
    } else {
        return Ok(None);
    };
    let encoded = base64::engine::general_purpose::STANDARD.encode(payload);
    Ok(Some(format!("data:{mime};base64,{encoded}")))
}

async fn image_path_to_data_url_with_ffmpeg(
    path: &str,
    ffmpeg_path: &OsStr,
) -> Result<Option<String>, GatewayError> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut image = options
        .open(path)
        .map_err(|_| GatewayError::Request("NapCat 图片文件不可读取".into()))?;
    let metadata = image
        .metadata()
        .map_err(|_| GatewayError::Request("NapCat 图片文件不可读取".into()))?;
    if !metadata.is_file() || metadata.len() > MAX_DOWNSAMPLE_INPUT_BYTES as u64 {
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut image)
        .take((MAX_DOWNSAMPLE_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| GatewayError::Request("NapCat 图片文件不可读取".into()))?;
    if bytes.len() > MAX_DOWNSAMPLE_INPUT_BYTES {
        return Ok(None);
    }
    let (mime, payload) = if bytes.len() > MAX_IMAGE_BYTES {
        let Some(jpeg) = downsample_image(&bytes, ffmpeg_path)
            .await
            .map_err(|_| GatewayError::Request("NapCat 图片降采样失败".into()))?
        else {
            return Ok(None);
        };
        ("image/jpeg", jpeg)
    } else if let Some(mime) = image_mime(&bytes) {
        (mime, bytes)
    } else {
        let Some(jpeg) = downsample_image(&bytes, ffmpeg_path)
            .await
            .map_err(|_| GatewayError::Request("NapCat 图片格式转换失败".into()))?
        else {
            return Ok(None);
        };
        ("image/jpeg", jpeg)
    };
    let encoded = base64::engine::general_purpose::STANDARD.encode(payload);
    Ok(Some(format!("data:{mime};base64,{encoded}")))
}

fn resolve_ffmpeg() -> OsString {
    if let Some(path) = std::env::var_os("FFMPEG").filter(|path| !path.is_empty()) {
        return path;
    }
    if let Ok(executable) = std::env::current_exe() {
        for ancestor in executable.ancestors() {
            let bundled = ancestor.join("runtime").join("bin").join(if cfg!(windows) {
                "ffmpeg.exe"
            } else {
                "ffmpeg"
            });
            if bundled.is_file() {
                return bundled.into_os_string();
            }
        }
    }
    OsString::from("ffmpeg")
}

struct TemporaryImageInput(PathBuf);

impl TemporaryImageInput {
    fn write(bytes: &[u8]) -> Result<Self, std::io::Error> {
        let path = std::env::temp_dir().join(format!("rinbot-image-{}.tmp", uuid::Uuid::new_v4()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        file.write_all(bytes)?;
        drop(file);
        Ok(Self(path))
    }
}

impl Drop for TemporaryImageInput {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn gif_strip_filter(frame_count: Option<usize>) -> String {
    let step = frame_count
        .filter(|count| *count > 4)
        .map(|count| count.div_ceil(4))
        .unwrap_or(1);
    let select = if step > 1 {
        format!("select=not(mod(n\\,{step})),")
    } else {
        String::new()
    };
    format!("{select}scale=512:-2,tpad=stop_mode=clone:stop=3,tile=2x2")
}

fn parse_ffmpeg_frame_count(progress: &[u8]) -> Option<usize> {
    String::from_utf8_lossy(progress)
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix("frame=")?.trim().parse().ok())
        .filter(|count| *count > 0)
}

async fn count_gif_frames(
    input: &std::path::Path,
    ffmpeg_path: &OsStr,
) -> Result<Option<usize>, String> {
    let mut child = TokioCommand::new(ffmpeg_path)
        .args(["-hide_banner", "-nostats", "-progress", "pipe:2", "-i"])
        .arg(input)
        .args(["-map", "0:v:0", "-f", "null", "-"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "ffmpeg could not start")?;
    let mut stderr = child.stderr.take().ok_or("ffmpeg stderr unavailable")?;
    let stderr_task = tokio::spawn(async move {
        let mut chunk = [0_u8; 1024];
        let mut retained = Vec::with_capacity(8192);
        loop {
            let read = stderr.read(&mut chunk).await.unwrap_or(0);
            if read == 0 {
                break;
            }
            retained.extend_from_slice(&chunk[..read]);
            if retained.len() > 8192 {
                let excess = retained.len() - 8192;
                retained.drain(..excess);
            }
        }
        retained
    });
    let status = match tokio::time::timeout(Duration::from_secs(30), child.wait()).await {
        Ok(status) => status.map_err(|_| "ffmpeg wait failed")?,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let _ = stderr_task.await;
            return Err("ffmpeg timed out".into());
        }
    };
    let progress = stderr_task
        .await
        .map_err(|_| "ffmpeg progress reader failed")?;
    if !status.success() {
        return Ok(None);
    }
    Ok(parse_ffmpeg_frame_count(&progress))
}

async fn downsample_image(bytes: &[u8], ffmpeg_path: &OsStr) -> Result<Option<Vec<u8>>, String> {
    if bytes.is_empty() || bytes.len() > MAX_DOWNSAMPLE_INPUT_BYTES {
        return Ok(None);
    }
    let input = TemporaryImageInput::write(bytes).map_err(|_| "temporary media file failed")?;
    let filter = if image_gif(bytes).is_some() {
        let frame_count = count_gif_frames(&input.0, ffmpeg_path).await?;
        gif_strip_filter(frame_count)
    } else {
        "scale=2048:2048:force_original_aspect_ratio=decrease:force_divisible_by=2".into()
    };
    let mut child = TokioCommand::new(ffmpeg_path)
        .args(["-hide_banner", "-loglevel", "error", "-threads", "1", "-i"])
        .arg(&input.0)
        .arg("-vf")
        .arg(filter)
        .args([
            "-frames:v",
            "1",
            "-q:v",
            "5",
            "-f",
            "image2pipe",
            "-vcodec",
            "mjpeg",
            "pipe:1",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "ffmpeg could not start")?;
    let mut stdout = child.stdout.take().ok_or("ffmpeg stdout unavailable")?;
    let mut stderr = child.stderr.take().ok_or("ffmpeg stderr unavailable")?;
    let stderr_task = tokio::spawn(async move {
        let mut chunk = [0_u8; 1024];
        let mut retained = Vec::with_capacity(4096);
        loop {
            let read = stderr.read(&mut chunk).await.unwrap_or(0);
            if read == 0 {
                break;
            }
            let remaining = 4096_usize.saturating_sub(retained.len());
            retained.extend_from_slice(&chunk[..read.min(remaining)]);
        }
        retained
    });
    let process = async move {
        let mut output = Vec::new();
        let mut chunk = [0_u8; 16 * 1024];
        loop {
            let read = stdout
                .read(&mut chunk)
                .await
                .map_err(|_| "ffmpeg output could not be read")?;
            if read == 0 {
                break;
            }
            if output.len().saturating_add(read) > MAX_IMAGE_BYTES {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Ok(None);
            }
            output.extend_from_slice(&chunk[..read]);
        }
        let status = child.wait().await.map_err(|_| "ffmpeg wait failed")?;
        if !status.success() || !output.starts_with(b"\xff\xd8\xff") {
            let _bounded_error = stderr_task.await.unwrap_or_default();
            return Ok(None);
        }
        let _bounded_error = stderr_task.await.unwrap_or_default();
        Ok(Some(output))
    };
    match tokio::time::timeout(Duration::from_secs(30), process).await {
        Ok(result) => result,
        Err(_) => Err("ffmpeg timed out".into()),
    }
}

fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

fn image_gif(bytes: &[u8]) -> Option<&'static str> {
    (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")).then_some("image/gif")
}

fn value_to_string(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_i64().map(|number| number.to_string()))
        .or_else(|| value.as_u64().map(|number| number.to_string()))
        .unwrap_or_default()
}

#[async_trait]
impl FriendRequestGateway for NapcatAdapter {
    async fn decide_friend_request(
        &self,
        request_flag: &str,
        approve: bool,
        remark: Option<&str>,
    ) -> Result<(), GatewayError> {
        NapcatAdapter::decide_friend_request(self, request_flag, approve, remark).await
    }
}

#[async_trait]
impl GroupRequestGateway for NapcatAdapter {
    async fn decide_group_request(
        &self,
        request_flag: &str,
        sub_type: &str,
        approve: bool,
    ) -> Result<(), GatewayError> {
        NapcatAdapter::decide_group_request(self, request_flag, sub_type, approve).await
    }
}

#[async_trait]
impl QzoneGateway for NapcatAdapter {
    async fn account_identity(&self) -> Result<(String, String), GatewayError> {
        let info = self.get_login_info().await?;
        Ok((info.user_id, info.nickname))
    }

    async fn qzone_read(
        &self,
        action: QzoneReadAction,
        params: Value,
    ) -> Result<Value, GatewayError> {
        // QZone endpoints may be slower than ordinary OneBot actions. Returned
        // cookie values are passed only to the caller and are never logged here.
        self.call_action_with_timeout(action.onebot_action(), params, Duration::from_secs(30))
            .await
    }

    async fn qzone_write(
        &self,
        action: QzoneWriteAction,
        params: Value,
    ) -> Result<(), GatewayError> {
        if self.config.observe_only {
            return Err(GatewayError::WritesDisabled);
        }
        if !self.connected() {
            return Err(GatewayError::NotConnected);
        }
        if action == QzoneWriteAction::PublishMoment {
            return Err(GatewayError::Request(
                "说说发布必须使用可核验回执接口".into(),
            ));
        }
        self.call_action_with_timeout(action.onebot_action(), params, Duration::from_secs(30))
            .await?;
        Ok(())
    }

    async fn publish_daily_moment(&self, params: Value) -> Result<String, GatewayError> {
        if self.config.observe_only {
            return Err(GatewayError::WritesDisabled);
        }
        if !self.connected() {
            return Err(GatewayError::NotConnected);
        }
        let response = self
            .call_action_with_timeout(
                QzoneWriteAction::PublishMoment.onebot_action(),
                params,
                Duration::from_secs(90),
            )
            .await?;
        let tid = response
            .get("tid")
            .or_else(|| response.get("id"))
            .map(value_to_string)
            .filter(|tid| {
                !tid.is_empty()
                    && tid.len() <= 256
                    && tid
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
            })
            .ok_or(GatewayError::UnknownOutcome)?;
        Ok(tid)
    }
}

pub fn normalize_event(raw: &Value) -> Option<DomainEvent> {
    let post_type = raw
        .get("post_type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if post_type == "notice" && raw.get("notice_type").and_then(Value::as_str) == Some("friend_add")
    {
        let user_id = raw.get("user_id").map(value_to_string).unwrap_or_default();
        if user_id.is_empty()
            || user_id.len() > 15
            || !user_id.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Some(DomainEvent::Unknown {
                post_type: "notice".into(),
                event_type: "friend_add.invalid".into(),
            });
        }
        return Some(DomainEvent::FriendAdded { user_id });
    }
    if post_type == "request" && raw.get("request_type").and_then(Value::as_str) == Some("group") {
        let group_id = raw.get("group_id").map(value_to_string).unwrap_or_default();
        let user_id = raw.get("user_id").map(value_to_string).unwrap_or_default();
        let sub_type = raw
            .get("sub_type")
            .and_then(Value::as_str)
            .unwrap_or("invite")
            .to_owned();
        let flag = raw
            .get("flag")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        if !valid_qq_id(&group_id)
            || !valid_qq_id(&user_id)
            || !matches!(sub_type.as_str(), "invite" | "add")
            || flag.is_empty()
            || flag.len() > 500
        {
            return Some(DomainEvent::Unknown {
                post_type: "request".into(),
                event_type: "group.invalid".into(),
            });
        }
        return Some(DomainEvent::GroupRequest {
            group_id,
            user_id,
            sub_type,
            flag: FriendRequestFlag::new(flag),
            comment: raw
                .get("comment")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        });
    }
    if post_type == "notice"
        && raw.get("notice_type").and_then(Value::as_str) == Some("group_increase")
    {
        let group_id = raw.get("group_id").map(value_to_string).unwrap_or_default();
        let user_id = raw.get("user_id").map(value_to_string).unwrap_or_default();
        if !valid_qq_id(&group_id) || !valid_qq_id(&user_id) {
            return Some(DomainEvent::Unknown {
                post_type: "notice".into(),
                event_type: "group_increase.invalid".into(),
            });
        }
        return Some(DomainEvent::GroupIncreased {
            group_id,
            user_id,
            operator_id: raw
                .get("operator_id")
                .map(value_to_string)
                .unwrap_or_default(),
            time: raw.get("time").and_then(Value::as_i64).unwrap_or_default(),
            is_bot: false,
        });
    }
    if post_type == "notice"
        && raw.get("notice_type").and_then(Value::as_str) == Some("group_decrease")
    {
        let group_id = raw.get("group_id").map(value_to_string).unwrap_or_default();
        let user_id = raw.get("user_id").map(value_to_string).unwrap_or_default();
        let sub_type = raw
            .get("sub_type")
            .and_then(Value::as_str)
            .unwrap_or("leave")
            .to_owned();
        if !valid_qq_id(&group_id)
            || !valid_qq_id(&user_id)
            || !matches!(sub_type.as_str(), "leave" | "kick")
        {
            return Some(DomainEvent::Unknown {
                post_type: "notice".into(),
                event_type: "group_decrease.invalid".into(),
            });
        }
        return Some(DomainEvent::GroupDecreased {
            group_id,
            user_id,
            sub_type,
            is_bot: false,
        });
    }
    if post_type == "notice" {
        let notice_type = raw
            .get("notice_type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let (kind, peer_field) = match notice_type {
            "group_recall" => ("group", "group_id"),
            "friend_recall" => ("private", "user_id"),
            _ => ("", ""),
        };
        if !kind.is_empty() {
            let peer_id = raw.get(peer_field).map(value_to_string).unwrap_or_default();
            let message_id = raw
                .get("message_id")
                .map(value_to_string)
                .unwrap_or_default();
            if !valid_qq_id(&peer_id)
                || message_id.is_empty()
                || message_id.len() > 128
                || !message_id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || byte == b'-')
            {
                return Some(DomainEvent::Unknown {
                    post_type: "notice".into(),
                    event_type: format!("{notice_type}.invalid"),
                });
            }
            let operator_id = raw
                .get("operator_id")
                .map(value_to_string)
                .filter(|value| valid_qq_id(value))
                .unwrap_or_default();
            return Some(DomainEvent::MessageRecalled {
                conversation_id: format!("{kind}:{peer_id}"),
                message_id,
                operator_id,
            });
        }
    }
    if post_type == "request" && raw.get("request_type").and_then(Value::as_str) == Some("friend") {
        let user_id = raw
            .get("user_id")
            .map(ToString::to_string)
            .unwrap_or_default()
            .trim_matches('"')
            .to_owned();
        let flag = raw
            .get("flag")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        if user_id.is_empty()
            || user_id.len() > 15
            || !user_id.bytes().all(|byte| byte.is_ascii_digit())
            || flag.is_empty()
            || flag.len() > 500
        {
            return Some(DomainEvent::Unknown {
                post_type: "request".into(),
                event_type: "friend.invalid".into(),
            });
        }
        return Some(DomainEvent::FriendRequest {
            user_id,
            flag: FriendRequestFlag::new(flag),
            comment: raw
                .get("comment")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        });
    }
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
    let sender_name = raw
        .get("sender")
        .and_then(|sender| sender.get("card").or_else(|| sender.get("nickname")))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let is_self = raw
        .get("self_id")
        .map(ToString::to_string)
        .is_some_and(|self_id| self_id.trim_matches('"') == sender_id);
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
        sender_name,
        is_group,
        is_self,
        segments,
        timestamp,
    }))
}

fn valid_qq_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 15 && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn validate_media_reference(value: &str) -> Result<(), GatewayError> {
    if value.trim().is_empty() || value.len() > 2048 || value.chars().any(char::is_control) {
        Err(GatewayError::Request(
            "invalid OneBot media reference".into(),
        ))
    } else {
        Ok(())
    }
}

fn verify_message_ownership(
    response: &Value,
    conversation_kind: &str,
    conversation_id: &str,
    message_id: &str,
) -> Result<(), GatewayError> {
    let actual_message_id = response.get("message_id").map(value_to_string);
    let actual_kind = response
        .get("message_type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let actual_peer = if conversation_kind == "group" {
        response.get("group_id").map(value_to_string)
    } else {
        response
            .pointer("/sender/user_id")
            .or_else(|| response.get("user_id"))
            .map(value_to_string)
    }
    .unwrap_or_default();
    if actual_message_id.as_deref() != Some(message_id)
        || actual_kind != conversation_kind
        || actual_peer != conversation_id
    {
        return Err(GatewayError::Request("消息不属于当前会话".into()));
    }
    Ok(())
}

fn forward_resource_id(segments: &[Value]) -> Option<String> {
    segments.iter().find_map(|segment| {
        (segment.get("type").and_then(Value::as_str) == Some("forward"))
            .then(|| segment.pointer("/data/id").map(value_to_string))
            .flatten()
            .filter(|id| !id.trim().is_empty() && id.len() <= 2048)
    })
}

type AudioCandidate = (&'static str, String, String, String);

fn forward_audio_candidates(data: &Value) -> Vec<AudioCandidate> {
    let Some(nodes) = data
        .get("messages")
        .or_else(|| data.pointer("/data/messages"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    for node in nodes.iter().take(MAX_FORWARD_MEDIA_NODES) {
        let Some(segments) = node
            .get("message")
            .or_else(|| node.get("content"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for segment in segments {
            let Some(segment_kind) = segment.get("type").and_then(Value::as_str) else {
                continue;
            };
            let Some(data) = segment.get("data") else {
                continue;
            };
            let name = data
                .get("file")
                .or_else(|| data.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .chars()
                .take(2048)
                .collect::<String>();
            let url = data
                .get("url")
                .and_then(Value::as_str)
                .filter(|url| {
                    url.len() <= 8192 && (url.starts_with("http://") || url.starts_with("https://"))
                })
                .unwrap_or_default()
                .to_owned();
            let file_id = data
                .get("file_id")
                .or_else(|| data.get("id"))
                .map(value_to_string)
                .filter(|id| !id.is_empty() && id.len() <= 2048)
                .unwrap_or_default();
            let candidate_kind = match segment_kind {
                "record" => Some("record"),
                "voice" => Some("voice"),
                "video" => Some("video"),
                "file" => {
                    let lower = name.to_ascii_lowercase();
                    let media_file = [
                        ".m4a", ".mp3", ".wav", ".amr", ".aac", ".ogg", ".flac", ".wma", ".mp4",
                        ".mov", ".avi", ".mkv", ".webm", ".silk",
                    ]
                    .iter()
                    .any(|extension| lower.ends_with(extension));
                    (media_file || !file_id.is_empty()).then_some("file")
                }
                _ => None,
            };
            if let Some(kind) = candidate_kind {
                candidates.push((kind, name, url, file_id));
                if candidates.len() >= 8 {
                    return candidates;
                }
            }
        }
    }
    candidates
}

fn video_urls_from_segments(segments: &[Value]) -> Vec<String> {
    segments
        .iter()
        .filter(|segment| {
            let kind = segment
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            kind == "video"
                || (kind == "file"
                    && segment
                        .pointer("/data/name")
                        .or_else(|| segment.pointer("/data/file"))
                        .and_then(Value::as_str)
                        .is_some_and(|name| {
                            [".mp4", ".mov", ".avi", ".mkv", ".webm"]
                                .iter()
                                .any(|extension| name.to_ascii_lowercase().ends_with(extension))
                        }))
        })
        .filter_map(|segment| {
            segment
                .pointer("/data/url")
                .and_then(Value::as_str)
                .filter(|url| {
                    url.len() <= 8192 && (url.starts_with("http://") || url.starts_with("https://"))
                })
                .map(str::to_owned)
        })
        .take(4)
        .collect()
}

async fn video_to_frame_strip(
    bytes: &[u8],
    ffmpeg_path: &OsStr,
) -> Result<Option<String>, GatewayError> {
    if bytes.is_empty() || bytes.len() > MAX_VIDEO_BYTES {
        return Ok(None);
    }
    let input = TemporaryImageInput::write(bytes)
        .map_err(|_| GatewayError::Request("临时视频文件不可写".into()))?;
    let seconds = probe_video_seconds(ffmpeg_path, &input.0).await;
    let filter = video_frame_filter(seconds);
    let mut child = TokioCommand::new(ffmpeg_path)
        .args(["-hide_banner", "-loglevel", "error", "-threads", "1", "-i"])
        .arg(&input.0)
        .arg("-vf")
        .arg(filter)
        .args([
            "-frames:v",
            "1",
            "-q:v",
            "5",
            "-f",
            "image2pipe",
            "-vcodec",
            "mjpeg",
            "pipe:1",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| GatewayError::Request("无法启动视频抽帧程序".into()))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| GatewayError::Request("视频抽帧输出不可用".into()))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| GatewayError::Request("视频抽帧诊断流不可用".into()))?;
    let stderr_task = tokio::spawn(async move {
        let mut chunk = [0_u8; 1024];
        while stderr.read(&mut chunk).await.unwrap_or(0) > 0 {}
    });
    let process = async {
        let mut output = Vec::new();
        let mut chunk = [0_u8; 16 * 1024];
        loop {
            let read = stdout
                .read(&mut chunk)
                .await
                .map_err(|_| GatewayError::Request("视频帧读取失败".into()))?;
            if read == 0 {
                break;
            }
            if output.len().saturating_add(read) > MAX_IMAGE_BYTES {
                let _ = child.kill().await;
                let _ = child.wait().await;
                let _ = stderr_task.await;
                return Ok(None);
            }
            output.extend_from_slice(&chunk[..read]);
        }
        let status = child
            .wait()
            .await
            .map_err(|_| GatewayError::Request("视频抽帧进程异常".into()))?;
        let _ = stderr_task.await;
        if !status.success() || !output.starts_with(b"\xff\xd8\xff") {
            return Ok(None);
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(output);
        Ok(Some(format!("data:image/jpeg;base64,{encoded}")))
    };
    match tokio::time::timeout(Duration::from_secs(30), process).await {
        Ok(result) => result,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(GatewayError::Request("视频抽帧超时".into()))
        }
    }
}

fn video_frame_filter(seconds: f64) -> String {
    let seconds = if seconds.is_finite() && seconds > 0.0 {
        seconds.clamp(1.0, 600.0)
    } else {
        10.0
    };
    let rate = 4.0 / seconds;
    format!("fps={rate:.6},scale=320:-2,tile=2x2")
}

async fn probe_video_seconds(ffmpeg_path: &OsStr, input_path: &Path) -> f64 {
    let ffmpeg_path = PathBuf::from(ffmpeg_path);
    let probe_path = ffmpeg_path
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(if cfg!(windows) {
            "ffprobe.exe"
        } else {
            "ffprobe"
        });
    let result = tokio::time::timeout(
        Duration::from_secs(8),
        TokioCommand::new(probe_path)
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=duration",
                "-of",
                "csv=p=0",
            ])
            .arg(input_path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let Ok(Ok(output)) = result else {
        return 0.0;
    };
    if !output.status.success() || output.stdout.len() > 128 {
        return 0.0;
    }
    std::str::from_utf8(&output.stdout)
        .ok()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .unwrap_or(0.0)
}

fn forward_nodes(data: &Value) -> Option<Vec<ForwardMessageNode>> {
    let nodes = data
        .get("messages")
        .or_else(|| data.pointer("/data/messages"))?
        .as_array()?;
    let mut output = Vec::with_capacity(nodes.len().min(30));
    for node in nodes.iter().take(30) {
        let sender = node.get("sender").unwrap_or(&Value::Null);
        let sender_name = sender
            .get("card")
            .or_else(|| sender.get("nickname"))
            .or_else(|| sender.get("user_id"))
            .map(value_to_string)
            .unwrap_or_else(|| "未知".into())
            .chars()
            .take(80)
            .collect::<String>();
        let message = node.get("message").or_else(|| node.get("content"));
        let media = match message {
            Some(Value::Array(segments)) => segments
                .iter()
                .filter_map(|segment| {
                    let kind = segment.get("type").and_then(Value::as_str)?;
                    let data = segment.get("data")?;
                    let file = data
                        .get("file")
                        .or_else(|| data.get("name"))
                        .and_then(Value::as_str)?
                        .trim();
                    if file.is_empty()
                        || file.len() > 2048
                        || file.starts_with("http://")
                        || file.starts_with("https://")
                        || file.contains('/')
                        || file.contains('\\')
                        || file.chars().any(char::is_control)
                    {
                        return None;
                    }
                    match kind {
                        "image" => Some(crate::domain::message::MessageSegment::Image {
                            file: file.to_owned(),
                            url: String::new(),
                        }),
                        "record" | "voice" => Some(crate::domain::message::MessageSegment::Audio {
                            file: file.to_owned(),
                            url: String::new(),
                        }),
                        "video" => Some(crate::domain::message::MessageSegment::Video {
                            file: file.to_owned(),
                            url: String::new(),
                        }),
                        _ => None,
                    }
                })
                .take(8)
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        let mut text = match message {
            Some(Value::String(text)) => text
                .split("[CQ:")
                .enumerate()
                .map(|(index, part)| {
                    if index == 0 {
                        part.to_owned()
                    } else {
                        part.split_once(']')
                            .map(|(_, rest)| rest.to_owned())
                            .unwrap_or_default()
                    }
                })
                .collect::<String>(),
            Some(Value::Array(segments)) => segments
                .iter()
                .map(
                    |segment| match segment.get("type").and_then(Value::as_str) {
                        Some("text") => segment
                            .pointer("/data/text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        Some("at") => segment
                            .pointer("/data/qq")
                            .map(|value| format!("@{}", value_to_string(value)))
                            .unwrap_or_default(),
                        Some("image") => "[图片]".into(),
                        Some("record" | "voice") => "[语音]".into(),
                        Some("video") => "[视频]".into(),
                        Some("file") => "[文件]".into(),
                        Some("face") => "[表情]".into(),
                        Some("forward") => "[嵌套转发]".into(),
                        Some(_) | None => String::new(),
                    },
                )
                .collect::<String>(),
            _ => String::new(),
        };
        text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let text = text.chars().take(200).collect::<String>();
        if !text.trim().is_empty() || !media.is_empty() {
            output.push(ForwardMessageNode {
                sender_name,
                text,
                media,
            });
        }
    }
    Some(output)
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
        "face" => MessageSegment::Face { id: string("id") },
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
        "file" => MessageSegment::File {
            name: string("name"),
            url: string("url"),
            file_id: data
                .get("file_id")
                .or_else(|| data.get("id"))
                .map(value_to_string)
                .unwrap_or_default(),
        },
        "reply" => MessageSegment::Reply {
            message_id: string("id"),
        },
        "forward" => MessageSegment::Forward {
            resource_id: string("id"),
        },
        _ => MessageSegment::Unknown {
            kind: kind.to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    use tokio_tungstenite::accept_async;

    #[test]
    fn failed_identity_friend_snapshot_is_a_typed_domain_event() {
        let event = friend_list_snapshot_event(Err(GatewayError::Request(
            "sensitive upstream response".into(),
        )));

        assert!(matches!(event, DomainEvent::FriendListSnapshotUnavailable));
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            json!({"kind":"friend_list_snapshot_unavailable"})
        );
    }

    #[test]
    fn qq_face_segments_preserve_the_numeric_face_id() {
        assert_eq!(
            normalize_segment(&json!({"type":"face","data":{"id":277}})),
            MessageSegment::Face { id: "277".into() }
        );
    }

    #[tokio::test]
    async fn group_name_lookup_validates_ids_and_caches_read_only_onebot_result() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 2048];
            let read = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.contains("/get_group_info"));
            assert!(request.contains("\"group_id\":200"));
            let body = r#"{"status":"ok","retcode":0,"data":{"group_name":"测试群"}}"#;
            let wire = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(), body
            );
            stream.write_all(wire.as_bytes()).await.unwrap();
        });
        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1/unused".into(),
            http_url: format!("http://{address}"),
            access_token: String::new(),
            observe_only: false,
        });

        assert!(adapter.get_group_name("200;drop").await.is_err());
        assert_eq!(
            adapter.get_group_name("200").await.unwrap().as_deref(),
            Some("测试群")
        );
        assert_eq!(
            adapter.get_group_name("200").await.unwrap().as_deref(),
            Some("测试群")
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn forward_read_validates_message_scope_and_expands_bounded_text_nodes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for (expected_action, expected_body, response) in [
                (
                    "/get_msg",
                    "\"message_id\":41004",
                    json!({
                        "status":"ok","retcode":0,"data":{
                            "message_id":41004,"message_type":"group","group_id":200,
                            "message":[{"type":"forward","data":{"id":"fresh-resource"}}]
                        }
                    }),
                ),
                (
                    "/get_forward_msg",
                    "\"id\":\"fresh-resource\"",
                    json!({
                        "status":"ok","retcode":0,"data":{"messages":[
                            {"sender":{"card":"群友","nickname":"账号","user_id":1},"message":[
                                {"type":"text","data":{"text":"原文"}},
                                {"type":"image","data":{"file":"forward-image-id","url":"https://private.invalid/token=secret"}},
                                {"type":"forward","data":{"id":"nested"}}
                            ]},
                            {"sender":{"nickname":"另一个人"},"message":"[CQ:face,id=1]你好"}
                        ]}
                    }),
                ),
            ] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 8192];
                let read = stream.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                assert!(request.contains(expected_action));
                assert!(request.contains(expected_body));
                let response = response.to_string();
                let wire = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.len(), response
                );
                stream.write_all(wire.as_bytes()).await.unwrap();
            }
        });
        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1/unused".into(),
            http_url: format!("http://{address}"),
            access_token: String::new(),
            observe_only: true,
        });
        adapter.connected.store(true, Ordering::Release);
        let nodes = adapter.message_forward("group:200", "41004").await.unwrap();
        server.await.unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].sender_name, "群友");
        assert_eq!(nodes[0].text, "原文[图片][嵌套转发]");
        assert_eq!(
            nodes[0].media,
            vec![crate::domain::message::MessageSegment::Image {
                file: "forward-image-id".into(),
                url: String::new(),
            }]
        );
        assert_eq!(nodes[1].text, "你好");
        assert!(!format!("{nodes:?}").contains("private.invalid"));
    }

    #[tokio::test]
    async fn forward_read_rejects_onebot_message_from_another_conversation_before_fallback() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request).await.unwrap();
            let response = r#"{"status":"ok","retcode":0,"data":{"message_id":41004,"message_type":"group","group_id":201,"message":[{"type":"forward","data":{"id":"foreign-resource"}}]}}"#;
            let wire = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(), response
            );
            stream.write_all(wire.as_bytes()).await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err()
            );
        });
        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1/unused".into(),
            http_url: format!("http://{address}"),
            access_token: String::new(),
            observe_only: true,
        });
        adapter.connected.store(true, Ordering::Release);
        assert!(adapter.message_forward("group:200", "41004").await.is_err());
        server.await.unwrap();
    }

    #[test]
    fn forward_node_normalization_bounds_nodes_and_cleans_cq_text() {
        let nodes = (0..40)
            .map(|index| {
                json!({
                    "sender":{"nickname":format!("昵称{index}")},
                    "message":if index % 2 == 0 {
                        json!([{"type":"text","data":{"text":"你好"}}])
                    } else {
                        json!("[CQ:face,id=4]你好")
                    }
                })
            })
            .collect::<Vec<_>>();
        let result = forward_nodes(&json!({"messages":nodes})).unwrap();
        assert_eq!(result.len(), 30);
        assert!(result.iter().all(|node| node.text == "你好"));
    }

    #[test]
    fn forward_audio_candidates_are_bounded_and_keep_urls_ephemeral() {
        let data = json!({"messages":[
            {"message":[{"type":"text","data":{"text":"说明"}}]},
            {"message":[
                {"type":"record","data":{"file":"voice.silk","url":"https://cdn.example/voice"}},
                {"type":"video","data":{"file":"clip.mp4","url":"http://127.0.0.1/private"}}
            ]}
        ]});
        let candidates = forward_audio_candidates(&data);
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].0, "record");
        assert_eq!(candidates[0].1, "voice.silk");
        assert_eq!(candidates[0].2, "https://cdn.example/voice");
        assert_eq!(candidates[1].0, "video");
        assert_eq!(candidates[1].2, "http://127.0.0.1/private");
        // URLs stay in this transient resolver result; normalized archive nodes
        // intentionally retain safe file refs, never URLs.
        let archived = forward_nodes(&data).unwrap();
        assert!(archived
            .iter()
            .flat_map(|node| &node.media)
            .all(|media| match media {
                MessageSegment::Audio { url, .. } | MessageSegment::Video { url, .. } =>
                    url.is_empty(),
                _ => true,
            }));
    }

    #[test]
    fn audio_message_scope_verification_rejects_cross_conversation_reads() {
        let response = json!({
            "message_id":41004,
            "message_type":"group",
            "group_id":200,
            "message":[]
        });
        assert!(verify_message_ownership(&response, "group", "200", "41004").is_ok());
        assert!(verify_message_ownership(&response, "group", "201", "41004").is_err());
        assert!(verify_message_ownership(&response, "private", "200", "41004").is_err());
    }

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

    #[test]
    fn normalizes_forward_as_a_safe_placeholder_and_keeps_resource_id_private_to_domain() {
        let raw = json!({
            "post_type":"message","message_type":"group","message_id":41004,
            "group_id":200,"user_id":100,"self_id":900,
            "message":[{"type":"forward","data":{"id":"short-lived-resource"}}]
        });
        let DomainEvent::Message(message) = normalize_event(&raw).unwrap() else {
            panic!("not a message")
        };
        assert_eq!(message.text_projection(), "[合并转发聊天记录]");
        assert!(matches!(
            &message.segments[0],
            MessageSegment::Forward { resource_id } if resource_id == "short-lived-resource"
        ));
        assert!(!message.text_projection().contains("short-lived-resource"));
    }

    #[test]
    fn friend_recall_is_private_and_malformed_recall_ids_fail_closed() {
        let event = normalize_event(&json!({
            "post_type":"notice",
            "notice_type":"friend_recall",
            "user_id":12345,
            "message_id":678,
            "operator_id":12345
        }))
        .expect("friend recall event");
        assert!(
            matches!(event, DomainEvent::MessageRecalled { conversation_id, message_id, .. }
            if conversation_id == "private:12345" && message_id == "678")
        );

        let malformed = normalize_event(&json!({
            "post_type":"notice",
            "notice_type":"group_recall",
            "group_id":12345,
            "message_id":"bad-id\n"
        }))
        .expect("sanitized invalid recall event");
        assert!(matches!(malformed, DomainEvent::Unknown { event_type, .. }
            if event_type == "group_recall.invalid"));
    }

    #[tokio::test]
    async fn image_retrieval_uses_napcat_path_and_detects_mime_from_content() {
        let directory = tempfile::tempdir().unwrap();
        let image_path = directory.path().join("fixture.png");
        std::fs::write(&image_path, b"\x89PNG\r\n\x1a\nfixture").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let returned_path = image_path.to_string_lossy().into_owned();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request).await.unwrap();
            let response = serde_json::json!({
                "status":"ok",
                "retcode":0,
                "data":{"file":returned_path}
            })
            .to_string();
            let wire = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(), response
            );
            stream.write_all(wire.as_bytes()).await.unwrap();
        });
        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1".into(),
            http_url: format!("http://{address}"),
            access_token: String::new(),
            observe_only: true,
        });
        let result = adapter.image_data_url("fixture-id").await.unwrap().unwrap();
        server.await.unwrap();
        assert!(result.starts_with("data:image/png;base64,"));
        let encoded = result.split_once(',').unwrap().1;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        assert!(decoded.ends_with(b"fixture"));

        let oversized_path = directory.path().join("oversized.png");
        std::fs::write(&oversized_path, vec![0; MAX_IMAGE_BYTES.saturating_add(1)]).unwrap();
        let oversized_result = image_path_to_data_url(oversized_path.to_str().unwrap()).await;
        assert!(oversized_result.is_err() || oversized_result.unwrap().is_none());

        let unsupported_path = directory.path().join("unsupported.png");
        std::fs::write(&unsupported_path, b"not an image").unwrap();
        let unsupported_result = image_path_to_data_url(unsupported_path.to_str().unwrap()).await;
        assert!(unsupported_result.is_err() || unsupported_result.unwrap().is_none());
    }

    #[tokio::test]
    async fn recall_media_send_maps_video_to_onebot_segment_and_requires_receipt() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8192];
            let length = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..length]);
            assert!(request.starts_with("POST /send_group_msg "));
            assert!(request.contains(r#""group_id":"200""#));
            assert!(request.contains(r#""type":"video""#));
            assert!(request.contains(r#""file":"file:///tmp/clip.mp4""#));
            let response = r#"{"status":"ok","retcode":0,"data":{"message_id":901}}"#;
            let wire = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(), response
            );
            stream.write_all(wire.as_bytes()).await.unwrap();
        });
        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1".into(),
            http_url: format!("http://{address}"),
            access_token: String::new(),
            observe_only: false,
        });
        adapter.connected.store(true, Ordering::Release);
        let receipt = adapter
            .send_media_segment(
                "group:200",
                &MessageSegment::Video {
                    file: "file:///tmp/clip.mp4".into(),
                    url: String::new(),
                },
            )
            .await
            .unwrap();
        server.await.unwrap();
        assert_eq!(receipt.message_id, "901");
    }

    #[tokio::test]
    async fn recall_media_send_forwards_qq_face_segment_and_requires_receipt() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8192];
            let length = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..length]);
            assert!(request.starts_with("POST /send_private_msg "));
            assert!(request.contains(r#""user_id":"99999""#));
            assert!(request.contains(r#""type":"face""#));
            assert!(request.contains(r#""id":"277""#));
            let response = r#"{"status":"ok","retcode":0,"data":{"message_id":902}}"#;
            let wire = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(), response
            );
            stream.write_all(wire.as_bytes()).await.unwrap();
        });
        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1".into(),
            http_url: format!("http://{address}"),
            access_token: String::new(),
            observe_only: false,
        });
        adapter.connected.store(true, Ordering::Release);
        let receipt = adapter
            .send_media_segment("private:99999", &MessageSegment::Face { id: "277".into() })
            .await
            .unwrap();
        server.await.unwrap();
        assert_eq!(receipt.message_id, "902");
    }

    #[tokio::test]
    async fn daily_moment_upload_source_preserves_supported_gif_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let gif_path = directory.path().join("animated.gif");
        let original = b"GIF89a\x01\x00\x01\x00original-animation";
        std::fs::write(&gif_path, original).unwrap();

        let source = image_path_to_upload_source(gif_path.to_str().unwrap())
            .await
            .unwrap()
            .unwrap();
        let encoded = source.strip_prefix("base64://").unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        assert_eq!(decoded, original);

        let unsupported = directory.path().join("not-image.bin");
        std::fs::write(&unsupported, b"not an image").unwrap();
        assert!(image_path_to_upload_source(unsupported.to_str().unwrap())
            .await
            .unwrap()
            .is_none());
    }

    #[test]
    fn gif_vision_filter_matches_node_four_frame_sampling_rules() {
        assert_eq!(
            gif_strip_filter(Some(12)),
            r"select=not(mod(n\,3)),scale=512:-2,tpad=stop_mode=clone:stop=3,tile=2x2"
        );
        assert_eq!(
            gif_strip_filter(Some(4)),
            "scale=512:-2,tpad=stop_mode=clone:stop=3,tile=2x2"
        );
        assert_eq!(
            gif_strip_filter(None),
            "scale=512:-2,tpad=stop_mode=clone:stop=3,tile=2x2"
        );
        assert_eq!(
            parse_ffmpeg_frame_count(b"frame=2\nprogress=continue\nframe=12\nprogress=end\n"),
            Some(12)
        );
        assert_eq!(parse_ffmpeg_frame_count(b"progress=end\n"), None);
    }

    #[test]
    fn video_vision_filter_samples_four_frames_across_node_compatible_duration() {
        assert_eq!(
            video_frame_filter(10.0),
            "fps=0.400000,scale=320:-2,tile=2x2"
        );
        assert_eq!(
            video_frame_filter(600.0),
            "fps=0.006667,scale=320:-2,tile=2x2"
        );
        assert_eq!(
            video_frame_filter(0.0),
            "fps=0.400000,scale=320:-2,tile=2x2"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn video_frame_conversion_returns_bounded_jpeg_data_url() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let fake_ffmpeg = directory.path().join("fake-ffmpeg");
        std::fs::write(&fake_ffmpeg, "#!/bin/sh\nprintf '\\377\\330\\377fixture'\n").unwrap();
        std::fs::set_permissions(&fake_ffmpeg, std::fs::Permissions::from_mode(0o700)).unwrap();
        let output = video_to_frame_strip(b"video fixture", fake_ffmpeg.as_os_str())
            .await
            .unwrap()
            .unwrap();
        assert!(output.starts_with("data:image/jpeg;base64,"));
        assert!(
            video_to_frame_strip(&vec![0; MAX_VIDEO_BYTES + 1], fake_ffmpeg.as_os_str())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn historical_message_image_lookup_is_bounded_deduplicated_and_reads_only() {
        let directory = tempfile::tempdir().unwrap();
        let image_path = directory.path().join("history.png");
        std::fs::write(&image_path, b"\x89PNG\r\n\x1a\nfixture").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let returned_path = image_path.to_string_lossy().into_owned();
        let server = tokio::spawn(async move {
            let (mut action_stream, _) = listener.accept().await.unwrap();
            let mut action_request = [0; 4096];
            let action_len = action_stream.read(&mut action_request).await.unwrap();
            let action_wire = String::from_utf8_lossy(&action_request[..action_len]);
            assert!(action_wire.starts_with("POST /get_msg "));
            assert!(action_wire.contains("\"message_id\":42"));
            let action_response = json!({
                "status":"ok",
                "retcode":0,
                "data":{"message_id":42,"message_type":"private","sender":{"user_id":99},"message":[
                    {"type":"text","data":{"text":"hello"}},
                    {"type":"image","data":{"file":"history-image"}},
                    {"type":"image","data":{"file":"history-image"}}
                ]}
            })
            .to_string();
            let action_http = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                action_response.len(), action_response
            );
            action_stream
                .write_all(action_http.as_bytes())
                .await
                .unwrap();

            let (mut image_stream, _) = listener.accept().await.unwrap();
            let mut image_request = [0; 4096];
            let image_len = image_stream.read(&mut image_request).await.unwrap();
            let image_wire = String::from_utf8_lossy(&image_request[..image_len]);
            assert!(image_wire.starts_with("POST /get_image "));
            assert!(image_wire.contains("\"file\":\"history-image\""));
            let image_response = json!({
                "status":"ok",
                "retcode":0,
                "data":{"file":returned_path}
            })
            .to_string();
            let image_http = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                image_response.len(), image_response
            );
            image_stream.write_all(image_http.as_bytes()).await.unwrap();

            let (mut mismatch_stream, _) = listener.accept().await.unwrap();
            let mut mismatch_request = [0; 4096];
            let mismatch_len = mismatch_stream.read(&mut mismatch_request).await.unwrap();
            let mismatch_wire = String::from_utf8_lossy(&mismatch_request[..mismatch_len]);
            assert!(mismatch_wire.starts_with("POST /get_msg "));
            let mismatch_response = json!({
                "status":"ok",
                "retcode":0,
                "data":{"message_id":42,"message_type":"private","sender":{"user_id":99},"message":[
                    {"type":"image","data":{"file":"history-image"}}
                ]}
            })
            .to_string();
            let mismatch_http = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                mismatch_response.len(), mismatch_response
            );
            mismatch_stream
                .write_all(mismatch_http.as_bytes())
                .await
                .unwrap();
        });
        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1".into(),
            http_url: format!("http://{address}"),
            access_token: String::new(),
            observe_only: true,
        });
        adapter.connected.store(true, Ordering::Release);
        let images = adapter.message_images("private:99", "42").await.unwrap();
        assert_eq!(images.len(), 1);
        assert!(images[0].starts_with("data:image/png;base64,"));
        assert!(adapter
            .message_images("private:99", "not-a-message-id")
            .await
            .is_err());
        assert!(adapter.message_images("private:888", "42").await.is_err());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn sticker_favorite_write_uses_verified_message_and_napcat_container_path() {
        let directory = tempfile::tempdir().unwrap();
        let image_path = directory.path().join("favorite.png");
        std::fs::write(&image_path, b"\x89PNG\r\n\x1a\nfixture").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let container_path = image_path.to_string_lossy().into_owned();
        let server = tokio::spawn(async move {
            let expected = ["/get_msg ", "/get_image ", "/add_custom_face "];
            let replies = [
                json!({"status":"ok","retcode":0,"data":{"message_id":42,"message_type":"group","group_id":99,"message":[{"type":"image","data":{"file":"opaque-image-id"}}]}}),
                json!({"status":"ok","retcode":0,"data":{"file":container_path}}),
                json!({"status":"ok","retcode":0,"data":{"result":0,"errMsg":"success"}}),
            ];
            for (action, reply) in expected.into_iter().zip(replies) {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let size = stream.read(&mut request).await.unwrap();
                let wire = String::from_utf8_lossy(&request[..size]);
                assert!(wire.starts_with(&format!("POST {action}")));
                if action == "/get_msg " {
                    assert!(wire.contains("\"message_id\":42"));
                } else if action == "/get_image " {
                    assert!(wire.contains("\"file\":\"opaque-image-id\""));
                } else {
                    assert!(wire.contains("\"file\":"));
                }
                let body = reply.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1".into(),
            http_url: format!("http://{address}"),
            access_token: String::new(),
            observe_only: false,
        });
        adapter.connected.store(true, Ordering::Release);
        let receipt = adapter
            .add_message_image_to_favorites("group:99", "42", 0)
            .await
            .unwrap();
        assert_eq!(receipt.emoji_id, "");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn sticker_favorite_sync_reads_only_the_bounded_full_snapshot() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for (response_body, expected_count) in [
                (
                    json!({"status":"ok","retcode":0,"data":[{"emoji_id":"a"}]}),
                    500,
                ),
                (
                    json!({"status":"ok","retcode":0,"data":{"data":[{"emoji_id":"b"}]}}),
                    2,
                ),
            ] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let size = stream.read(&mut request).await.unwrap();
                let wire = String::from_utf8_lossy(&request[..size]);
                assert!(wire.starts_with("POST /fetch_custom_face_detail "));
                assert!(wire.contains(&format!("\"count\":{expected_count}")));
                let body = response_body.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1".into(),
            http_url: format!("http://{address}"),
            access_token: String::new(),
            observe_only: true,
        });
        adapter.connected.store(true, Ordering::Release);
        assert_eq!(adapter.fetch_qq_favorites(500).await.unwrap().len(), 1);
        assert_eq!(adapter.fetch_qq_favorites(2).await.unwrap().len(), 1);
        assert!(adapter.fetch_qq_favorites(501).await.is_err());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn sticker_favorite_write_is_disabled_in_observation_mode() {
        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1".into(),
            http_url: "http://127.0.0.1:1".into(),
            access_token: String::new(),
            observe_only: true,
        });
        adapter.connected.store(true, Ordering::Release);
        assert!(matches!(
            adapter
                .add_message_image_to_favorites("group:99", "42", 0)
                .await,
            Err(GatewayError::WritesDisabled)
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn oversized_images_are_boundedly_downsampled_to_jpeg() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let fake_ffmpeg = directory.path().join("fake-ffmpeg");
        std::fs::write(&fake_ffmpeg, "#!/bin/sh\nprintf '\\377\\330\\377fixture'\n").unwrap();
        std::fs::set_permissions(&fake_ffmpeg, std::fs::Permissions::from_mode(0o700)).unwrap();

        let input = vec![0_u8; MAX_IMAGE_BYTES + 1];
        let output = downsample_image(&input, fake_ffmpeg.as_os_str())
            .await
            .unwrap()
            .expect("fake converter returns a JPEG");

        assert!(output.starts_with(b"\xff\xd8\xff"));
        assert!(output.len() <= MAX_IMAGE_BYTES);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn image_file_reader_preserves_oversized_input_for_downsampling() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let fake_ffmpeg = directory.path().join("fake-ffmpeg");
        std::fs::write(&fake_ffmpeg, "#!/bin/sh\nprintf '\\377\\330\\377fixture'\n").unwrap();
        std::fs::set_permissions(&fake_ffmpeg, std::fs::Permissions::from_mode(0o700)).unwrap();

        let image_path = directory.path().join("large.png");
        std::fs::write(&image_path, vec![0_u8; MAX_IMAGE_BYTES + 128 * 1024]).unwrap();
        let result = image_path_to_data_url_with_ffmpeg(
            image_path.to_str().unwrap(),
            fake_ffmpeg.as_os_str(),
        )
        .await
        .unwrap()
        .expect("oversized bounded input is converted");

        assert!(result.starts_with("data:image/jpeg;base64,"));
        let encoded = result.split_once(',').unwrap().1;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        assert_eq!(decoded, b"\xff\xd8\xfffixture");
    }

    #[test]
    fn normalizes_friend_request_without_serializing_or_debugging_onebot_flag() {
        let event = normalize_event(&json!({
            "post_type":"request",
            "request_type":"friend",
            "user_id":12345,
            "flag":"private-capability-token",
            "comment":"please add"
        }))
        .expect("friend request");
        let serialized = serde_json::to_string(&event).unwrap();
        assert!(serialized.contains("friend_request"));
        assert!(!serialized.contains("private-capability-token"));
        assert!(!format!("{event:?}").contains("private-capability-token"));
        let DomainEvent::FriendRequest {
            user_id, comment, ..
        } = event
        else {
            panic!("expected normalized friend request")
        };
        assert_eq!(user_id, "12345");
        assert_eq!(comment, "please add");
    }

    #[test]
    fn normalizes_friend_added_notice_and_rejects_invalid_user_ids() {
        let event = normalize_event(&json!({
            "post_type":"notice",
            "notice_type":"friend_add",
            "user_id":12345
        }))
        .expect("friend-added notice");
        assert!(matches!(event, DomainEvent::FriendAdded { user_id } if user_id == "12345"));

        let invalid = normalize_event(&json!({
            "post_type":"notice",
            "notice_type":"friend_add",
            "user_id":"bad-id"
        }))
        .expect("malformed notice remains traceable");
        assert!(matches!(
            invalid,
            DomainEvent::Unknown { post_type, event_type }
                if post_type == "notice" && event_type == "friend_add.invalid"
        ));
    }

    #[test]
    fn normalizes_group_invite_capability_without_exposing_it() {
        let event = normalize_event(&json!({
            "post_type":"request",
            "request_type":"group",
            "sub_type":"invite",
            "group_id":67890,
            "user_id":12345,
            "flag":"onebot-group-invite-capability",
            "comment":"请来群里"
        }))
        .expect("group request");
        let serialized = serde_json::to_string(&event).unwrap();
        let debug = format!("{event:?}");
        assert!(serialized.contains("group_request"));
        assert!(!serialized.contains("onebot-group-invite-capability"));
        assert!(!debug.contains("onebot-group-invite-capability"));
        assert!(matches!(
            event,
            DomainEvent::GroupRequest { group_id, user_id, sub_type, comment, .. }
                if group_id == "67890" && user_id == "12345"
                    && sub_type == "invite" && comment == "请来群里"
        ));

        let direct = normalize_event(&json!({
            "post_type":"notice",
            "notice_type":"group_increase",
            "group_id":67890,
            "user_id":900,
            "operator_id":12345,
            "time":1791178000
        }))
        .expect("direct group increase");
        assert!(matches!(
            direct,
            DomainEvent::GroupIncreased {
                group_id,
                user_id,
                operator_id,
                time: 1791178000,
                is_bot: false
            } if group_id == "67890" && user_id == "900" && operator_id == "12345"
        ));

        let departure = normalize_event(&json!({
            "post_type":"notice",
            "notice_type":"group_decrease",
            "sub_type":"kick",
            "group_id":67890,
            "user_id":900,
            "operator_id":12345
        }))
        .expect("group departure");
        assert!(matches!(
            departure,
            DomainEvent::GroupDecreased {
                group_id,
                user_id,
                sub_type,
                is_bot: false
            } if group_id == "67890" && user_id == "900" && sub_type == "kick"
        ));
    }

    #[test]
    fn malformed_group_departures_are_traceable_unknown_events() {
        let event = normalize_event(&json!({
            "post_type":"notice",
            "notice_type":"group_decrease",
            "sub_type":"ban",
            "group_id":67890,
            "user_id":12345
        }))
        .expect("malformed departure should remain traceable");
        assert!(matches!(event, DomainEvent::Unknown { event_type, .. }
            if event_type == "group_decrease.invalid"));
    }

    #[test]
    fn malformed_group_invites_are_sanitized_unknown_events() {
        let event = normalize_event(&json!({
            "post_type":"request",
            "request_type":"group",
            "sub_type":"unsupported",
            "group_id":"not-a-group",
            "user_id":12345,
            "flag":"private-capability"
        }))
        .expect("malformed request should remain traceable");
        assert!(matches!(
            event,
            DomainEvent::Unknown { post_type, event_type }
                if post_type == "request" && event_type == "group.invalid"
        ));
    }

    #[test]
    fn malformed_friend_request_is_retained_as_sanitized_unknown_event() {
        let event = normalize_event(&json!({
            "post_type":"request",
            "request_type":"friend",
            "user_id":"not-a-qq-number",
            "flag":"private-capability-token"
        }))
        .expect("malformed request should remain traceable");
        let debug = format!("{event:?}");
        assert!(matches!(
            event,
            DomainEvent::Unknown {
                post_type,
                event_type
            } if post_type == "request" && event_type == "friend.invalid"
        ));
        assert!(!debug.contains("private-capability-token"));
    }

    #[tokio::test]
    async fn send_requires_a_message_id_to_be_considered_confirmed() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for response in [
                r#"{"status":"ok","retcode":0,"data":{}}"#,
                r#"{"status":"ok","retcode":0,"data":{"message_id":42}}"#,
            ] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 2048];
                let read = stream.read(&mut request).await.unwrap();
                assert!(String::from_utf8_lossy(&request[..read]).contains("send_private_msg"));
                let wire = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.len(), response
                );
                stream.write_all(wire.as_bytes()).await.unwrap();
            }
        });
        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1/unused".into(),
            http_url: format!("http://{address}"),
            access_token: String::new(),
            observe_only: false,
        });
        adapter.connected.store(true, Ordering::Release);
        assert!(matches!(
            adapter.send_text("private:123", "hello").await,
            Err(GatewayError::UnknownOutcome)
        ));
        assert_eq!(
            adapter
                .send_text("private:123", "hello")
                .await
                .unwrap()
                .message_id,
            "42"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn send_text_with_options_preserves_reply_mention_and_text_segment_order() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0; 2048];
            loop {
                let read = stream.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let text = String::from_utf8_lossy(&request);
                    if text.contains("message_id") {
                        break;
                    }
                    if let Some(length) = text
                        .split("content-length:")
                        .nth(1)
                        .or_else(|| text.split("Content-Length:").nth(1))
                        .and_then(|value| value.trim().split('\r').next())
                        .and_then(|value| value.parse::<usize>().ok())
                    {
                        let header_end = request
                            .windows(4)
                            .position(|window| window == b"\r\n\r\n")
                            .unwrap()
                            + 4;
                        if request.len() >= header_end + length {
                            break;
                        }
                    }
                }
            }
            let header_end = request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .unwrap()
                + 4;
            let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
            assert!(headers.contains("/send_group_msg"));
            let body: Value = serde_json::from_slice(&request[header_end..]).unwrap();
            assert_eq!(
                body["message"][0],
                json!({"type":"reply","data":{"id":"-123"}})
            );
            assert_eq!(
                body["message"][1],
                json!({"type":"text","data":{"text":"hello"}})
            );
            let response = r#"{"status":"ok","retcode":0,"data":{"message_id":77}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(), response
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1/unused".into(),
            http_url: format!("http://{address}"),
            access_token: String::new(),
            observe_only: false,
        });
        adapter.connected.store(true, Ordering::Release);
        let receipt = adapter
            .send_text_with_options("group:200", "hello", Some("-123"), None)
            .await
            .unwrap();
        assert_eq!(receipt.message_id, "77");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn send_face_uses_native_onebot_segment_and_preserves_target_order() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0; 2048];
            loop {
                let read = stream.read(&mut chunk).await.unwrap();
                assert_ne!(read, 0, "request ended before face action body arrived");
                request.extend_from_slice(&chunk[..read]);
                let headers_end = request
                    .windows(4)
                    .position(|bytes| bytes == b"\r\n\r\n")
                    .expect("HTTP headers");
                let headers = String::from_utf8_lossy(&request[..headers_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap();
                if request.len() >= headers_end + 4 + content_length {
                    break;
                }
            }
            let header_end = request
                .windows(4)
                .position(|bytes| bytes == b"\r\n\r\n")
                .unwrap()
                + 4;
            let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
            assert!(headers.contains("/send_group_msg"));
            let body: Value = serde_json::from_slice(&request[header_end..]).unwrap();
            assert_eq!(
                body["message"][0],
                json!({"type":"reply","data":{"id":"-123"}})
            );
            assert_eq!(body["message"][1], json!({"type":"at","data":{"qq":"456"}}));
            assert_eq!(
                body["message"][2],
                json!({"type":"text","data":{"text":"hello"}})
            );
            assert_eq!(
                body["message"][3],
                json!({"type":"face","data":{"id":"14"}})
            );
            let response = r#"{"status":"ok","retcode":0,"data":{"message_id":88}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(), response
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1/unused".into(),
            http_url: format!("http://{address}"),
            access_token: String::new(),
            observe_only: false,
        });
        adapter.connected.store(true, Ordering::Release);
        let receipt = adapter
            .send_face(
                "group:200",
                "14",
                "微笑",
                Some("hello"),
                Some("-123"),
                Some("456"),
            )
            .await
            .unwrap();
        assert_eq!(receipt.message_id, "88");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn poke_uses_onebot_actions_and_accepts_confirmed_action_without_message_id() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for (action, expected_ids) in [
                ("friend_poke", ["123", "123"]),
                ("group_poke", ["200", "456"]),
            ] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut chunk = [0; 2048];
                loop {
                    let read = stream.read(&mut chunk).await.unwrap();
                    assert_ne!(read, 0, "request ended before action body arrived");
                    request.extend_from_slice(&chunk[..read]);
                    let headers_end = request
                        .windows(4)
                        .position(|bytes| bytes == b"\r\n\r\n")
                        .expect("HTTP headers");
                    let headers = String::from_utf8_lossy(&request[..headers_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap();
                    if request.len() >= headers_end + 4 + content_length {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&request);
                assert!(request.contains(action), "{request}");
                for id in expected_ids {
                    assert!(request.contains(id), "{request}");
                }
                let body = r#"{"status":"ok","retcode":0,"data":{}}"#;
                let wire = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                );
                stream.write_all(wire.as_bytes()).await.unwrap();
            }
        });
        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1/unused".into(),
            http_url: format!("http://{address}"),
            access_token: String::new(),
            observe_only: false,
        });
        adapter.connected.store(true, Ordering::Release);
        assert!(adapter
            .send_poke("private:123", "123")
            .await
            .unwrap()
            .message_id
            .is_empty());
        assert!(adapter
            .send_poke("group:200", "456")
            .await
            .unwrap()
            .message_id
            .is_empty());
        assert!(adapter.send_poke("private:123", "456").await.is_err());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn image_send_accepts_only_bounded_local_images_and_requires_receipt() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0; 4096];
            loop {
                let read = stream.read(&mut chunk).await.unwrap();
                assert_ne!(read, 0, "request ended before image payload arrived");
                request.extend_from_slice(&chunk[..read]);
                if let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n")
                {
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .expect("HTTP request has a content length");
                    if request.len() >= header_end + 4 + content_length {
                        break;
                    }
                }
            }
            let wire = String::from_utf8_lossy(&request);
            assert!(wire.contains("send_group_msg"));
            assert!(wire.contains("\"group_id\":\"456\""));
            assert!(wire.contains("\"type\":\"image\""));
            assert!(wire.contains("base64://iVBORw0KGgp"));
            let response = r#"{"status":"ok","retcode":0,"data":{"message_id":73}}"#;
            let http = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(), response
            );
            stream.write_all(http.as_bytes()).await.unwrap();
        });
        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1/unused".into(),
            http_url: format!("http://{address}"),
            access_token: String::new(),
            observe_only: false,
        });
        adapter.connected.store(true, Ordering::Release);
        let image = format!(
            "base64://{}",
            base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\nfixture")
        );
        assert_eq!(
            adapter
                .send_image("group:456", &image)
                .await
                .unwrap()
                .message_id,
            "73"
        );
        server.await.unwrap();

        assert!(matches!(
            adapter
                .send_image("group:456", "https://example.invalid/image.png")
                .await,
            Err(GatewayError::Request(_))
        ));
        assert!(matches!(
            adapter
                .send_image("group:456", "base64://bm90LWltYWdl")
                .await,
            Err(GatewayError::Request(_))
        ));
    }

    #[tokio::test]
    async fn group_request_decision_uses_onebot_action_and_unknown_ack_stays_unknown() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for (response, expected_approve) in [
                (r#"{"status":"ok","retcode":0,"data":{}}"#, true),
                (r#"{"status":"async","retcode":0}"#, false),
            ] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 2048];
                let read = stream.read(&mut request).await.unwrap();
                let wire = String::from_utf8_lossy(&request[..read]);
                assert!(wire.contains("set_group_add_request"));
                assert!(wire.contains("group-capability-token"));
                assert!(wire.contains("\"sub_type\":\"invite\""));
                assert!(wire.contains(&format!("\"approve\":{expected_approve}")));
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    response.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1/unused".into(),
            http_url: format!("http://{address}"),
            access_token: String::new(),
            observe_only: false,
        });
        adapter.connected.store(true, Ordering::Release);
        assert!(adapter
            .decide_group_request("group-capability-token", "invite", true)
            .await
            .is_ok());
        assert!(matches!(
            adapter
                .decide_group_request("group-capability-token", "invite", false)
                .await,
            Err(GatewayError::UnknownOutcome)
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn group_request_decision_classifies_the_full_http_ack_matrix() {
        let cases = [
            (
                "200 OK",
                r#"{"status":"ok","retcode":0,"data":{}}"#,
                "confirmed",
            ),
            ("200 OK", r#"{"retcode":0}"#, "confirmed"),
            (
                "200 OK",
                r#"{"status":"failed","retcode":17,"wording":"permission denied"}"#,
                "request",
            ),
            (
                "200 OK",
                r#"{"status":"failed","retcode":200}"#,
                "unsupported",
            ),
            ("200 OK", r#"{"status":"async","retcode":0}"#, "unknown"),
            ("200 OK", r#"{"status":"ok"}"#, "confirmed"),
            ("200 OK", "not-json", "unknown"),
            (
                "400 Bad Request",
                r#"{"status":"failed","retcode":17}"#,
                "request",
            ),
            (
                "503 Service Unavailable",
                r#"{"status":"failed","retcode":17}"#,
                "unknown",
            ),
        ];
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server_cases = cases.map(|(status, body, _)| (status, body));
        let server = tokio::spawn(async move {
            for (status, body) in server_cases {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 2048];
                let read = stream.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                assert!(request.contains("set_group_add_request"));
                assert!(request.contains("group-capability-token"));
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1/unused".into(),
            http_url: format!("http://{address}"),
            access_token: String::new(),
            observe_only: false,
        });
        adapter.connected.store(true, Ordering::Release);

        for (_, _, expected) in cases {
            let actual = match adapter
                .decide_group_request("group-capability-token", "invite", true)
                .await
            {
                Ok(()) => "confirmed",
                Err(GatewayError::Request(_)) => "request",
                Err(GatewayError::UnsupportedAction(_)) => "unsupported",
                Err(GatewayError::UnknownOutcome) => "unknown",
                Err(error) => panic!("unexpected group-request result: {error}"),
            };
            assert_eq!(actual, expected);
        }
        server.await.unwrap();
    }

    #[tokio::test]
    async fn group_request_decision_rejects_observation_mode_and_invalid_subtype() {
        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1/unused".into(),
            http_url: "http://127.0.0.1:1".into(),
            access_token: String::new(),
            observe_only: true,
        });
        assert!(matches!(
            adapter.decide_group_request("flag", "invite", true).await,
            Err(GatewayError::WritesDisabled)
        ));
        let active_adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1/unused".into(),
            http_url: "http://127.0.0.1:1".into(),
            access_token: String::new(),
            observe_only: false,
        });
        active_adapter.connected.store(true, Ordering::Release);
        assert!(matches!(
            active_adapter
                .decide_group_request("flag", "invalid", true)
                .await,
            Err(GatewayError::Request(_))
        ));
    }

    #[tokio::test]
    async fn onebot_http_results_normalize_definite_failures_and_unknown_outcomes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let cases = vec![
            (
                "200 OK",
                r#"{"status":"ok","retcode":0,"data":{"message_id":42}}"#,
            ),
            (
                "400 Bad Request",
                r#"{"status":"failed","retcode":100,"wording":"bad request"}"#,
            ),
            (
                "401 Unauthorized",
                r#"{"status":"failed","retcode":401,"wording":"unauthorized"}"#,
            ),
            (
                "500 Internal Server Error",
                r#"{"status":"failed","retcode":500}"#,
            ),
            (
                "200 OK",
                r#"{"status":"failed","retcode":100,"wording":"rejected"}"#,
            ),
            ("200 OK", r#"{"status":"async","retcode":0}"#),
            ("200 OK", "not-json"),
        ];
        let expected = [
            "confirmed",
            "request",
            "request",
            "unknown",
            "request",
            "unknown",
            "unknown",
        ];
        let server_cases = cases.clone();
        let server = tokio::spawn(async move {
            for (status, body) in server_cases {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 2048];
                let read = stream.read(&mut request).await.unwrap();
                assert!(String::from_utf8_lossy(&request[..read]).contains("send_private_msg"));
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1/unused".into(),
            http_url: format!("http://{address}"),
            access_token: String::new(),
            observe_only: false,
        });
        adapter.connected.store(true, Ordering::Release);

        for ((_, _), expected) in cases.into_iter().zip(expected) {
            let actual = match adapter.send_text("private:123", "test").await {
                Ok(receipt) if receipt.message_id == "42" => "confirmed",
                Err(GatewayError::Request(_)) => "request",
                Err(GatewayError::UnknownOutcome) => "unknown",
                Err(error) => panic!("unexpected normalized OneBot result: {error}"),
                Ok(receipt) => panic!("unexpected message receipt: {receipt:?}"),
            };
            assert_eq!(actual, expected);
        }
        server.await.unwrap();
    }

    #[tokio::test]
    async fn friend_request_decision_requires_confirmed_onebot_result() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for (response, expected_approve) in [
                (r#"{"status":"ok","retcode":0,"data":{}}"#, true),
                (r#"{"status":"async","retcode":0,"data":{}}"#, false),
            ] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 2048];
                let read = stream.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                assert!(request.contains("set_friend_add_request"));
                assert!(request.contains("capability-flag"));
                assert!(request.contains(&format!("\"approve\":{expected_approve}")));
                let wire = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.len(), response
                );
                stream.write_all(wire.as_bytes()).await.unwrap();
            }
        });
        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1/unused".into(),
            http_url: format!("http://{address}"),
            access_token: String::new(),
            observe_only: false,
        });
        adapter.connected.store(true, Ordering::Release);
        assert!(adapter
            .decide_friend_request("capability-flag", true, Some("  hello\n there  "))
            .await
            .is_ok());
        assert!(matches!(
            adapter
                .decide_friend_request("capability-flag", false, None)
                .await,
            Err(GatewayError::UnknownOutcome)
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn friend_request_decision_refuses_observation_mode() {
        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1/unused".into(),
            http_url: "http://127.0.0.1:1".into(),
            access_token: String::new(),
            observe_only: true,
        });
        assert!(matches!(
            adapter.decide_friend_request("flag", true, None).await,
            Err(GatewayError::WritesDisabled)
        ));
    }

    #[tokio::test]
    async fn websocket_sends_periodic_ping_and_clears_connected_state_on_close() {
        let http_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_address = http_listener.local_addr().unwrap();
        let http_server = tokio::spawn(async move {
            let (mut stream, _) = http_listener.accept().await.unwrap();
            let mut request = [0; 2048];
            let read = stream.read(&mut request).await.unwrap();
            assert!(String::from_utf8_lossy(&request[..read]).contains("get_login_info"));
            let body = r#"{"status":"ok","retcode":0,"data":{"user_id":900,"nickname":"RInBot"}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(), body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_address = ws_listener.local_addr().unwrap();
        let ws_server = tokio::spawn(async move {
            let (stream, _) = ws_listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let ping = tokio::time::timeout(Duration::from_secs(1), socket.next())
                .await
                .expect("heartbeat ping was not sent")
                .expect("websocket ended before heartbeat")
                .expect("websocket heartbeat read failed");
            assert!(ping.is_ping(), "expected Ping frame, received {ping:?}");
            socket.send(Message::Close(None)).await.unwrap();
        });

        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: format!("ws://{ws_address}/onebot/v11/ws"),
            http_url: format!("http://{http_address}"),
            access_token: String::new(),
            observe_only: true,
        });
        let (event_tx, _event_rx) = mpsc::channel(1);
        let run = tokio::time::timeout(
            Duration::from_secs(2),
            adapter.run_with_heartbeat(event_tx, Duration::from_millis(20)),
        )
        .await
        .expect("adapter did not stop after mock websocket close");
        assert!(matches!(run, Err(NapcatError::StreamEnded)));
        assert!(!adapter.connected());

        http_server.await.unwrap();
        ws_server.await.unwrap();
    }

    #[tokio::test]
    async fn qzone_read_uses_allowlisted_action_and_accepts_ephemeral_cookie_data() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let size = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..size]);
            assert!(request.starts_with("POST /get_cookies HTTP/1.1"));
            assert!(request.contains("user.qzone.qq.com"));
            let response = json!({"status":"ok", "retcode":0,
                "data":{"cookies":"runtime-only-fixture"}})
            .to_string();
            let wire = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(), response
            );
            stream.write_all(wire.as_bytes()).await.unwrap();
        });
        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1".into(),
            http_url: format!("http://{address}"),
            access_token: String::new(),
            observe_only: true,
        });
        let value = adapter
            .qzone_read(
                QzoneReadAction::GetCookies,
                json!({"domain":"user.qzone.qq.com"}),
            )
            .await
            .unwrap();
        assert_eq!(value["cookies"], "runtime-only-fixture");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn qzone_write_is_refused_in_observation_mode_before_network_io() {
        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1".into(),
            http_url: "http://127.0.0.1:1".into(),
            access_token: String::new(),
            observe_only: true,
        });
        assert!(matches!(
            adapter
                .qzone_write(QzoneWriteAction::LikePost, json!({"id":"feed-1"}))
                .await,
            Err(GatewayError::WritesDisabled)
        ));
    }

    #[tokio::test]
    async fn qzone_async_write_receipt_is_classified_as_unknown() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let size = stream.read(&mut request).await.unwrap();
            assert!(String::from_utf8_lossy(&request[..size])
                .starts_with("POST /send_qzone_msg HTTP/1.1"));
            let response = json!({"status":"async", "retcode":1, "data":{}}).to_string();
            let wire = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(), response
            );
            stream.write_all(wire.as_bytes()).await.unwrap();
        });
        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1".into(),
            http_url: format!("http://{address}"),
            access_token: String::new(),
            observe_only: false,
        });
        adapter.connected.store(true, Ordering::Release);
        assert!(matches!(
            adapter
                .publish_daily_moment(json!({"content":"fixture","images":[],"ugc_right":4}))
                .await,
            Err(GatewayError::UnknownOutcome)
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn qzone_moment_publish_uses_allowlisted_action_and_confirms_only_sync_success() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let size = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..size]);
            assert!(request.starts_with("POST /send_qzone_msg HTTP/1.1"));
            assert!(request.contains("\"content\":\"fixture moment\""));
            assert!(request.contains("\"images\":[]"));
            let response =
                json!({"status":"ok", "retcode":0, "data":{"tid":"moment-7"}}).to_string();
            let wire = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(), response
            );
            stream.write_all(wire.as_bytes()).await.unwrap();
        });
        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1".into(),
            http_url: format!("http://{address}"),
            access_token: String::new(),
            observe_only: false,
        });
        adapter.connected.store(true, Ordering::Release);
        assert_eq!(
            adapter
                .publish_daily_moment(json!({
                    "content":"fixture moment",
                    "images":[],
                    "ugc_right":4
                }))
                .await
                .unwrap(),
            "moment-7"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn qzone_retcode_200_is_typed_as_explicit_unsupported_not_transport_failure() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let size = stream.read(&mut request).await.unwrap();
            assert!(String::from_utf8_lossy(&request[..size])
                .starts_with("POST /get_qzone_feeds HTTP/1.1"));
            let response = r#"{"status":"failed","retcode":200,"wording":"unsupported action"}"#;
            let wire = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(), response
            );
            stream.write_all(wire.as_bytes()).await.unwrap();
        });
        let adapter = NapcatAdapter::new(NapcatConfig {
            ws_url: "ws://127.0.0.1:1".into(),
            http_url: format!("http://{address}"),
            access_token: String::new(),
            observe_only: true,
        });
        assert!(matches!(
            adapter
                .qzone_read(QzoneReadAction::GetFeeds, json!({"page_num":1,"count":10}))
                .await,
            Err(GatewayError::UnsupportedAction(action)) if action == "get_qzone_feeds"
        ));
        server.await.unwrap();
    }
}
