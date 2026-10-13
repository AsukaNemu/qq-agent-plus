//! Durable owner notification for recalled messages already stored by RInBot.
//! A notice is claimed before any QQ write, and interrupted writes are
//! quarantined as unknown rather than replayed.

use std::sync::Arc;

use serde_json::json;
use thiserror::Error;

use crate::{
    adapters::sqlite::{SqliteStore, StoreError},
    application::daily_moment_generation::format_shanghai_time,
    application::outbox::{NotificationSendError, NotificationSender},
    domain::{event::DomainEvent, message::MessageSegment},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecallNotificationResult {
    pub status: &'static str,
    pub conversation_id: String,
    pub message_id: String,
    pub media_sent: usize,
    pub media_failures: Vec<RecallMediaFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecallMediaFailure {
    pub kind: String,
    pub outcome: &'static str,
}

#[derive(Debug, Error)]
pub enum RecallNotificationError {
    #[error(transparent)]
    Store(#[from] StoreError),
}

pub struct RecallNotificationExecutor {
    store: Arc<SqliteStore>,
    sender: Arc<dyn NotificationSender>,
    owner_uin: String,
}

impl RecallNotificationExecutor {
    pub fn new(
        store: Arc<SqliteStore>,
        sender: Arc<dyn NotificationSender>,
        owner_uin: impl Into<String>,
    ) -> Self {
        Self {
            store,
            sender,
            owner_uin: owner_uin.into(),
        }
    }

    pub async fn notify(
        &self,
        event: &DomainEvent,
        now: i64,
    ) -> Result<RecallNotificationResult, RecallNotificationError> {
        let DomainEvent::MessageRecalled {
            conversation_id,
            message_id,
            operator_id,
        } = event
        else {
            return Ok(RecallNotificationResult {
                status: "ignored",
                conversation_id: String::new(),
                message_id: String::new(),
                media_sent: 0,
                media_failures: Vec::new(),
            });
        };
        let mut result = RecallNotificationResult {
            status: "not-found",
            conversation_id: conversation_id.clone(),
            message_id: message_id.clone(),
            media_sent: 0,
            media_failures: Vec::new(),
        };
        let Some(message) = self
            .store
            .incoming_message_by_id(conversation_id, message_id)?
        else {
            return Ok(result);
        };
        if !valid_owner(&self.owner_uin) {
            result.status = "disabled";
            return Ok(result);
        }
        if !self
            .store
            .claim_recall_notification(conversation_id, message_id, now)?
        {
            result.status = match self
                .store
                .recall_notification_status(conversation_id, message_id)?
                .as_deref()
            {
                Some("sending" | "unknown") => "unknown",
                Some("sent") => "duplicate",
                _ => "duplicate",
            };
            return Ok(result);
        }

        let media = match self
            .sender
            .recalled_message_media(conversation_id, message_id)
            .await
        {
            Ok(Some(fresh)) if !recalled_media(&fresh).is_empty() => recalled_media(&fresh),
            _ => recalled_media(&message.segments),
        };
        let notice = format_recall_notice(
            conversation_id,
            message_id,
            operator_id,
            &message.sender_name,
            &message.sender_id,
            message.timestamp,
            &message.text_projection(),
            media.len(),
        );
        let receipt = match self
            .sender
            .send_owner_notification(&self.owner_uin, json!({"text": notice}))
            .await
        {
            Ok(receipt) => receipt,
            Err(error) => {
                let status = match &error {
                    NotificationSendError::Unknown => "unknown",
                    NotificationSendError::ConfirmedFailure
                    | NotificationSendError::Internal(_) => "failed",
                };
                let error_class = match &error {
                    NotificationSendError::Unknown => "unknown outcome",
                    NotificationSendError::ConfirmedFailure => "confirmed send failure",
                    NotificationSendError::Internal(_) => "outbox dispatch error",
                };
                self.store.finish_recall_notification(
                    conversation_id,
                    message_id,
                    status,
                    None,
                    Some(error_class),
                    now,
                )?;
                result.status = status;
                return Ok(result);
            }
        };
        self.store.finish_recall_notification(
            conversation_id,
            message_id,
            "sent",
            Some(&receipt.message_id),
            None,
            now,
        )?;
        result.status = "notified";

        // As in Node, the text header is the deduplication point. Media is
        // best-effort and never causes a second header or automatic retry.
        for segment in media {
            let media_kind = match &segment {
                MessageSegment::Image { .. } => "image",
                MessageSegment::Audio { .. } => "audio",
                MessageSegment::Video { .. } => "video",
                MessageSegment::Face { .. } => "face",
                _ => continue,
            };
            let mut candidates = recalled_media_candidates(&segment);
            let mut candidate_index = 0;
            let mut refreshed_image_refs = false;
            let mut sent = false;
            let mut stop_candidate_attempts = false;
            let mut failure_outcome = "failed";
            loop {
                while candidate_index < candidates.len() {
                    let candidate = candidates[candidate_index].clone();
                    candidate_index += 1;
                    match self
                        .sender
                        .send_owner_notification(&self.owner_uin, json!({"segment": candidate}))
                        .await
                    {
                        Ok(_) => {
                            result.media_sent += 1;
                            sent = true;
                            break;
                        }
                        Err(NotificationSendError::ConfirmedFailure) => {}
                        Err(NotificationSendError::Unknown) => {
                            failure_outcome = "unknown";
                            stop_candidate_attempts = true;
                            break;
                        }
                        Err(NotificationSendError::Internal(_)) => {
                            stop_candidate_attempts = true;
                            break;
                        }
                    }
                }
                if sent || stop_candidate_attempts || refreshed_image_refs {
                    break;
                }
                let MessageSegment::Image { file, .. } = &segment else {
                    break;
                };
                if file.trim().is_empty() {
                    break;
                }
                refreshed_image_refs = true;
                let Ok(alternatives) = self.sender.recalled_image_alternatives(file).await else {
                    break;
                };
                for value in alternatives {
                    let candidate = MessageSegment::Image {
                        file: value,
                        url: String::new(),
                    };
                    if !candidates.contains(&candidate) {
                        candidates.push(candidate);
                    }
                }
                if candidate_index >= candidates.len() {
                    break;
                }
            }
            if !sent {
                let _ = self.store.record_runtime_event(
                    "recall.media-send",
                    &json!({
                        "conversationId": conversation_id,
                        "messageId": message_id,
                        "mediaKind": media_kind,
                        "outcome": failure_outcome
                    }),
                );
                result.media_failures.push(RecallMediaFailure {
                    kind: media_kind.into(),
                    outcome: failure_outcome,
                });
                tracing::warn!(%conversation_id, %message_id, media_kind, "recalled media could not be sent to the owner");
            }
        }
        Ok(result)
    }
}

pub fn format_recall_notice(
    conversation_id: &str,
    message_id: &str,
    operator_id: &str,
    sender_name: &str,
    sender_id: &str,
    timestamp: i64,
    body: &str,
    media_count: usize,
) -> String {
    let (kind, peer_id) = conversation_id.split_once(':').unwrap_or(("会话", "未知"));
    let source = match kind {
        "group" => format!("群 {peer_id}"),
        "private" => format!("私聊 {peer_id}"),
        _ => "未知会话".into(),
    };
    let sender_name = clean_header(sender_name, 80);
    let sender_id = clean_header(sender_id, 20);
    let sender = match (sender_name.is_empty(), sender_id.is_empty()) {
        (false, false) => format!("{sender_name}（{sender_id}）"),
        (false, true) => sender_name,
        (true, false) => sender_id,
        (true, true) => "未知发送者".into(),
    };
    let operator = if kind == "group" && !operator_id.is_empty() {
        format!("\n撤回者：{}", clean_header(operator_id, 20))
    } else {
        String::new()
    };
    let body = clean_body(body, 6000);
    let body = if body.is_empty() {
        "[无文字内容]"
    } else {
        &body
    };
    let media_hint = if media_count > 0 {
        format!("\n媒体：{media_count} 个（随后发送）")
    } else {
        String::new()
    };
    let time = if timestamp > 0 {
        format_shanghai_time(timestamp)
    } else {
        "日期未知".into()
    };
    format!(
        "【撤回消息】\n来源：{source}\n发送者：{sender}\n时间：{time}\n消息ID：{}\n内容：{body}{operator}{media_hint}",
        clean_header(message_id, 128)
    )
}

fn recalled_media(segments: &[MessageSegment]) -> Vec<MessageSegment> {
    let mut media = Vec::new();
    for segment in segments {
        let usable = match segment {
            MessageSegment::Image { file, url }
            | MessageSegment::Audio { file, url }
            | MessageSegment::Video { file, url } => {
                !file.trim().is_empty() || !url.trim().is_empty()
            }
            MessageSegment::Face { id } => {
                !id.is_empty() && id.len() <= 10 && id.bytes().all(|byte| byte.is_ascii_digit())
            }
            _ => false,
        };
        if usable && media.len() < 8 && !media.contains(segment) {
            media.push(segment.clone());
        }
    }
    media
}

fn recalled_media_candidates(segment: &MessageSegment) -> Vec<MessageSegment> {
    let (MessageSegment::Image { file, url }
    | MessageSegment::Audio { file, url }
    | MessageSegment::Video { file, url }) = segment
    else {
        return vec![segment.clone()];
    };
    let mut references = Vec::new();
    let path = std::path::Path::new(file.trim());
    if path.is_absolute() {
        if let Ok(file_url) = url::Url::from_file_path(path) {
            references.push(file_url.to_string());
        }
        if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
            if !name.is_empty() {
                references.push(name.to_owned());
            }
        }
    } else if !file.trim().is_empty() {
        references.push(file.trim().to_owned());
    }
    if !url.trim().is_empty() {
        references.push(url.trim().to_owned());
    }
    let mut unique_references = Vec::new();
    for value in references {
        if value.len() <= 2048
            && !value.chars().any(char::is_control)
            && !value.trim().is_empty()
            && !unique_references.contains(&value)
        {
            unique_references.push(value);
        }
    }
    unique_references
        .into_iter()
        .map(|file| match segment {
            MessageSegment::Image { .. } => MessageSegment::Image {
                file,
                url: String::new(),
            },
            MessageSegment::Audio { .. } => MessageSegment::Audio {
                file,
                url: String::new(),
            },
            MessageSegment::Video { .. } => MessageSegment::Video {
                file,
                url: String::new(),
            },
            _ => unreachable!(),
        })
        .collect()
}

fn clean_header(value: &str, max_chars: usize) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_control())
        .take(max_chars)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn clean_body(value: &str, max_chars: usize) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_control() || *ch == '\n' || *ch == '\t')
        .take(max_chars)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn valid_owner(owner: &str) -> bool {
    (5..=15).contains(&owner.len()) && owner.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use tempfile::tempdir;

    use crate::{
        adapters::sqlite::SqliteStore,
        application::outbox::{NotificationSendError, NotificationSender},
        domain::{
            event::DomainEvent,
            message::{IncomingMessage, MessageSegment},
        },
        ports::gateway::SendReceipt,
    };

    use super::{format_recall_notice, recalled_media_candidates, RecallNotificationExecutor};

    #[derive(Default)]
    struct TestSender {
        payloads: Mutex<Vec<serde_json::Value>>,
        media_attempts: Mutex<Vec<serde_json::Value>>,
        fail_next: Mutex<Option<NotificationSendError>>,
        fail_media: Mutex<bool>,
        fail_media_references: Mutex<Vec<String>>,
        unknown_media_references: Mutex<Vec<String>>,
        refreshed_media: Mutex<Option<Vec<MessageSegment>>>,
        image_alternatives: Mutex<Vec<String>>,
        image_alternative_requests: Mutex<usize>,
    }

    #[async_trait]
    impl NotificationSender for TestSender {
        async fn recalled_message_media(
            &self,
            _conversation_id: &str,
            _message_id: &str,
        ) -> Result<Option<Vec<MessageSegment>>, crate::ports::gateway::GatewayError> {
            Ok(self.refreshed_media.lock().unwrap().clone())
        }

        async fn recalled_image_alternatives(
            &self,
            _file: &str,
        ) -> Result<Vec<String>, crate::ports::gateway::GatewayError> {
            *self.image_alternative_requests.lock().unwrap() += 1;
            Ok(self.image_alternatives.lock().unwrap().clone())
        }

        async fn send_owner_notification(
            &self,
            _owner_uin: &str,
            payload: serde_json::Value,
        ) -> Result<SendReceipt, NotificationSendError> {
            if let Some(segment) = payload.get("segment") {
                self.media_attempts.lock().unwrap().push(payload.clone());
                if *self.fail_media.lock().unwrap() {
                    return Err(NotificationSendError::ConfirmedFailure);
                }
                let reference = segment
                    .get("file")
                    .or_else(|| segment.get("id"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if self
                    .unknown_media_references
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|value| value == reference)
                {
                    return Err(NotificationSendError::Unknown);
                }
                if self
                    .fail_media_references
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|value| value == reference)
                {
                    return Err(NotificationSendError::ConfirmedFailure);
                }
            }
            if let Some(error) = self.fail_next.lock().unwrap().take() {
                return Err(error);
            }
            self.payloads.lock().unwrap().push(payload);
            Ok(SendReceipt {
                message_id: "sent-1".into(),
            })
        }
    }

    fn stored_message() -> IncomingMessage {
        IncomingMessage {
            message_id: "42".into(),
            conversation_id: "group:7".into(),
            sender_id: "12345".into(),
            sender_name: "小明".into(),
            is_group: true,
            is_self: false,
            segments: vec![
                MessageSegment::Text {
                    text: "周末徒步".into(),
                },
                MessageSegment::Image {
                    file: "image-id".into(),
                    url: String::new(),
                },
                MessageSegment::Audio {
                    file: "audio-id".into(),
                    url: String::new(),
                },
                MessageSegment::Video {
                    file: "video-id".into(),
                    url: String::new(),
                },
            ],
            timestamp: 1_700_000_000_000,
        }
    }

    fn recall_event() -> DomainEvent {
        DomainEvent::MessageRecalled {
            conversation_id: "group:7".into(),
            message_id: "42".into(),
            operator_id: "67890".into(),
        }
    }

    #[test]
    fn local_media_paths_try_file_url_basename_then_stored_url() {
        let candidates = recalled_media_candidates(&MessageSegment::Image {
            file: "/tmp/rinbot recall.png".into(),
            url: "https://cdn.example.invalid/image.png".into(),
        });
        assert_eq!(
            candidates,
            vec![
                MessageSegment::Image {
                    file: "file:///tmp/rinbot%20recall.png".into(),
                    url: String::new(),
                },
                MessageSegment::Image {
                    file: "rinbot recall.png".into(),
                    url: String::new(),
                },
                MessageSegment::Image {
                    file: "https://cdn.example.invalid/image.png".into(),
                    url: String::new(),
                },
            ]
        );
    }

    fn executor(sender: Arc<TestSender>) -> (RecallNotificationExecutor, tempfile::TempDir) {
        let directory = tempdir().unwrap();
        let store = Arc::new(SqliteStore::open(directory.path().join("rinbot.sqlite")).unwrap());
        store.insert_incoming(&stored_message()).unwrap();
        (
            RecallNotificationExecutor::new(store, sender, "99999"),
            directory,
        )
    }

    #[tokio::test]
    async fn sends_one_header_and_recorded_media_then_deduplicates_event() {
        let sender = Arc::new(TestSender::default());
        let (executor, _directory) = executor(sender.clone());
        let first = executor.notify(&recall_event(), 100).await.unwrap();
        assert_eq!(first.status, "notified");
        assert_eq!(first.media_sent, 3);
        assert_eq!(sender.payloads.lock().unwrap().len(), 4);
        let body = sender.payloads.lock().unwrap()[0]["text"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(body.contains("【撤回消息】"));
        assert!(body.contains("群 7"));
        assert!(body.contains("小明（12345）"));
        assert!(body.contains("撤回者：67890"));
        assert!(body.contains("媒体：3 个"));

        let duplicate = executor.notify(&recall_event(), 101).await.unwrap();
        assert_eq!(duplicate.status, "duplicate");
        assert_eq!(sender.payloads.lock().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn prefers_fresh_media_references_when_napcat_still_has_them() {
        let sender = Arc::new(TestSender::default());
        *sender.refreshed_media.lock().unwrap() = Some(vec![MessageSegment::Image {
            file: "fresh-image-id".into(),
            url: String::new(),
        }]);
        let (executor, _directory) = executor(sender.clone());

        let result = executor.notify(&recall_event(), 100).await.unwrap();

        assert_eq!(result.status, "notified");
        assert_eq!(result.media_sent, 1);
        let payloads = sender.payloads.lock().unwrap();
        assert_eq!(payloads.len(), 2);
        assert_eq!(payloads[1]["segment"]["file"], "fresh-image-id");
    }

    #[tokio::test]
    async fn retries_only_confirmed_media_failures_with_url_and_refreshed_image_refs() {
        let sender = Arc::new(TestSender::default());
        *sender.refreshed_media.lock().unwrap() = Some(vec![MessageSegment::Image {
            file: "stale-file".into(),
            url: "expired-url".into(),
        }]);
        *sender.fail_media_references.lock().unwrap() =
            vec!["stale-file".into(), "expired-url".into()];
        *sender.image_alternatives.lock().unwrap() = vec!["fresh-image".into()];
        let (executor, _directory) = executor(sender.clone());

        let result = executor.notify(&recall_event(), 100).await.unwrap();

        assert_eq!(result.media_sent, 1);
        let attempts = sender.media_attempts.lock().unwrap();
        assert_eq!(attempts.len(), 3);
        assert_eq!(attempts[0]["segment"]["file"], "stale-file");
        assert_eq!(attempts[1]["segment"]["file"], "expired-url");
        assert_eq!(attempts[2]["segment"]["file"], "fresh-image");
        assert_eq!(*sender.image_alternative_requests.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn unknown_media_write_does_not_try_an_alternate_reference() {
        let sender = Arc::new(TestSender::default());
        *sender.refreshed_media.lock().unwrap() = Some(vec![MessageSegment::Image {
            file: "uncertain-file".into(),
            url: "alternate-url".into(),
        }]);
        *sender.unknown_media_references.lock().unwrap() = vec!["uncertain-file".into()];
        *sender.image_alternatives.lock().unwrap() = vec!["fresh-image".into()];
        let (executor, _directory) = executor(sender.clone());

        let result = executor.notify(&recall_event(), 100).await.unwrap();

        assert_eq!(result.media_sent, 0);
        assert_eq!(result.media_failures[0].outcome, "unknown");
        let attempts = sender.media_attempts.lock().unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0]["segment"]["file"], "uncertain-file");
        assert_eq!(*sender.image_alternative_requests.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn forwards_stored_qq_face_segments_to_the_owner() {
        let sender = Arc::new(TestSender::default());
        *sender.refreshed_media.lock().unwrap() =
            Some(vec![MessageSegment::Face { id: "277".into() }]);
        let (executor, _directory) = executor(sender.clone());

        let result = executor.notify(&recall_event(), 100).await.unwrap();

        assert_eq!(result.media_sent, 1);
        assert_eq!(
            sender.media_attempts.lock().unwrap()[0]["segment"]["type"],
            "face"
        );
        assert_eq!(
            sender.media_attempts.lock().unwrap()[0]["segment"]["id"],
            "277"
        );
    }

    #[tokio::test]
    async fn reports_failed_recall_media_with_safe_kind_and_outcome() {
        let sender = Arc::new(TestSender::default());
        *sender.fail_media.lock().unwrap() = true;
        let (executor, _directory) = executor(sender.clone());

        let result = executor.notify(&recall_event(), 100).await.unwrap();

        assert_eq!(result.status, "notified");
        assert_eq!(result.media_sent, 0);
        assert_eq!(
            result.media_failures,
            ["image", "audio", "video"]
                .into_iter()
                .map(|kind| super::RecallMediaFailure {
                    kind: kind.into(),
                    outcome: "failed"
                })
                .collect::<Vec<_>>()
        );
        assert_eq!(sender.payloads.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn unknown_header_write_is_quarantined_and_never_retried() {
        let sender = Arc::new(TestSender::default());
        *sender.fail_next.lock().unwrap() = Some(NotificationSendError::Unknown);
        let (executor, _directory) = executor(sender.clone());
        assert_eq!(
            executor.notify(&recall_event(), 100).await.unwrap().status,
            "unknown"
        );
        assert_eq!(
            executor.notify(&recall_event(), 101).await.unwrap().status,
            "unknown"
        );
        assert!(sender.payloads.lock().unwrap().is_empty());
    }

    #[test]
    fn recall_notice_sanitizes_header_injection_and_bounds_body() {
        let notice = format_recall_notice(
            "private:12345",
            "42",
            "",
            "name\n【伪造】",
            "12345",
            0,
            &"字".repeat(7000),
            0,
        );
        assert!(!notice.contains("name\n【伪造】"));
        assert!(notice.contains("日期未知"));
        assert!(notice.chars().count() < 6200);
    }
}
