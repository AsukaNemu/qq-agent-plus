//! Durable, fail-closed administrator alerts for the incident feature.
//!
//! The delivery intent is marked `sending` before calling OneBot. Interruption
//! or an ambiguous response becomes `unknown` and is never retried automatically.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use thiserror::Error;

use crate::{
    application::outbox::{NotificationSendError, NotificationSender},
    incident::{Incident, IncidentError, IncidentStore},
};

#[derive(Debug, Error)]
pub enum IncidentNotificationError {
    #[error(transparent)]
    Store(#[from] IncidentError),
    #[error("incident alerts are disabled")]
    Disabled,
}

pub struct IncidentNotificationExecutor {
    store: Arc<IncidentStore>,
    sender: Arc<dyn NotificationSender>,
    owner_uin: String,
    enabled: AtomicBool,
}

impl IncidentNotificationExecutor {
    pub fn new(
        store: Arc<IncidentStore>,
        sender: Arc<dyn NotificationSender>,
        owner_uin: String,
        enabled: bool,
    ) -> Self {
        Self {
            store,
            sender,
            owner_uin,
            enabled: AtomicBool::new(enabled),
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
    }

    pub async fn notify(&self, id: &str) -> Result<bool, IncidentNotificationError> {
        if !self.enabled.load(Ordering::Acquire) {
            return Err(IncidentNotificationError::Disabled);
        }
        if !valid_uin(&self.owner_uin) {
            return Ok(false);
        }
        let Some(incident) = self.store.begin_notification(id)? else {
            return Ok(false);
        };
        let message = format_incident_alert(&incident);
        let (state, error) = match self
            .sender
            .send_owner_notification(&self.owner_uin, serde_json::json!({"text":message}))
            .await
        {
            Ok(_) => ("sent", ""),
            Err(NotificationSendError::Unknown) => (
                "unknown",
                "管理员告警结果未知；请在 QQ 客户端核对，系统不会自动重发。",
            ),
            Err(NotificationSendError::ConfirmedFailure | NotificationSendError::Internal(_)) => (
                "failed",
                "告警未确认送达；请核对异常台账和 QQ 后再由管理员手动重试。",
            ),
        };
        self.store
            .finish_notification(id, state, error, incident_now_ms())?;
        Ok(true)
    }

    pub async fn notify_pending(&self, limit: u32) -> Result<u64, IncidentNotificationError> {
        if !self.enabled.load(Ordering::Acquire) {
            return Ok(0);
        }
        let ids = self.store.pending_notification_ids(limit)?;
        let mut attempted = 0;
        for id in ids {
            if self.notify(&id).await? {
                attempted += 1;
            }
        }
        Ok(attempted)
    }

    pub async fn retry_failed(&self, id: &str) -> Result<Incident, IncidentNotificationError> {
        if !self.enabled.load(Ordering::Acquire) {
            return Err(IncidentNotificationError::Disabled);
        }
        self.store.requeue_failed_notification(id)?;
        self.notify(id).await?;
        self.store
            .get(id)?
            .ok_or(IncidentError::NotFound)
            .map_err(Into::into)
    }
}

pub fn format_incident_alert(incident: &Incident) -> String {
    let severity = match incident.severity.as_str() {
        "critical" => "严重",
        "error" => "错误",
        "warning" => "警告",
        "info" => "信息",
        _ => "提示",
    };
    let mut lines = vec![
        "【实验功能 · QQ Agent 异常】".to_owned(),
        format!("等级：{severity}"),
        format!("模块：{}", incident.source),
    ];
    if !incident.chat_key.is_empty() {
        lines.push(format!("会话：{}", incident.chat_key));
    }
    lines.extend([
        format!("错误代码：{}", incident.error_code),
        format!("中文说明：{}", incident.error_note),
        format!(
            "原始结果：{}",
            incident.message.chars().take(300).collect::<String>()
        ),
        format!("次数：{}", incident.count),
        format!("编号：{}", incident.id),
        String::new(),
        "处理入口：控制台 → 异常".to_owned(),
    ]);
    lines.join("\n")
}

fn valid_uin(value: &str) -> bool {
    (5..=15).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn incident_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        application::outbox::{NotificationSendError, NotificationSender},
        incident::{IncidentInput, IncidentSettings},
        ports::gateway::SendReceipt,
    };
    use async_trait::async_trait;
    use serde_json::json;

    struct Sender(Result<SendReceipt, NotificationSendError>);

    #[async_trait]
    impl NotificationSender for Sender {
        async fn send_owner_notification(
            &self,
            _owner_uin: &str,
            _payload: serde_json::Value,
        ) -> Result<SendReceipt, NotificationSendError> {
            match &self.0 {
                Ok(receipt) => Ok(receipt.clone()),
                Err(NotificationSendError::ConfirmedFailure) => {
                    Err(NotificationSendError::ConfirmedFailure)
                }
                Err(NotificationSendError::Unknown) => Err(NotificationSendError::Unknown),
                Err(NotificationSendError::Internal(error)) => {
                    Err(NotificationSendError::Internal(error.clone()))
                }
            }
        }
    }

    fn setup(
        directory: &tempfile::TempDir,
        result: Result<SendReceipt, NotificationSendError>,
        enabled: bool,
    ) -> (Arc<IncidentStore>, IncidentNotificationExecutor, String) {
        setup_with_owner(directory, result, "998877", enabled)
    }

    fn setup_with_owner(
        directory: &tempfile::TempDir,
        result: Result<SendReceipt, NotificationSendError>,
        owner_uin: &str,
        enabled: bool,
    ) -> (Arc<IncidentStore>, IncidentNotificationExecutor, String) {
        let store = Arc::new(
            IncidentStore::open(
                directory.path().join("incident-pilot.sqlite"),
                IncidentSettings {
                    enabled: true,
                    graduated: true,
                    notify_warnings: true,
                    duplicate_window_minutes: 10,
                    retention_days: 90,
                    unknown_writes_block_chat: false,
                },
            )
            .unwrap(),
        );
        let incident = store
            .capture(&IncidentInput {
                code: "TEST_ALERT".into(),
                category: "internal".into(),
                severity: "critical".into(),
                source: "test".into(),
                chat_key: "group:12345".into(),
                session_id: String::new(),
                operation_id: String::new(),
                message: "运行失败".into(),
                details: json!({}),
                now_ms: 1000,
            })
            .unwrap()
            .unwrap();
        let executor = IncidentNotificationExecutor::new(
            store.clone(),
            Arc::new(Sender(result)),
            owner_uin.into(),
            enabled,
        );
        (store, executor, incident.id)
    }

    #[tokio::test]
    async fn confirmed_alert_is_durable_and_pending_queue_is_drained_once() {
        let directory = tempfile::tempdir().unwrap();
        let (store, executor, id) = setup(
            &directory,
            Ok(SendReceipt {
                message_id: "m1".into(),
            }),
            true,
        );
        assert_eq!(executor.notify_pending(20).await.unwrap(), 1);
        assert_eq!(store.get(&id).unwrap().unwrap().notify_state, "sent");
        assert_eq!(executor.notify_pending(20).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn unknown_alert_is_quarantined_and_never_retried() {
        let directory = tempfile::tempdir().unwrap();
        let (store, executor, id) = setup(&directory, Err(NotificationSendError::Unknown), true);
        assert_eq!(executor.notify_pending(20).await.unwrap(), 1);
        assert_eq!(store.get(&id).unwrap().unwrap().notify_state, "unknown");
        assert_eq!(executor.notify_pending(20).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn observation_mode_does_not_send_or_claim_alerts() {
        let directory = tempfile::tempdir().unwrap();
        let (store, executor, id) = setup(
            &directory,
            Ok(SendReceipt {
                message_id: "unused".into(),
            }),
            false,
        );
        assert_eq!(executor.notify_pending(20).await.unwrap(), 0);
        assert_eq!(store.get(&id).unwrap().unwrap().notify_state, "pending");
        executor.set_enabled(true);
        assert_eq!(executor.notify_pending(20).await.unwrap(), 1);
        assert_eq!(store.get(&id).unwrap().unwrap().notify_state, "sent");
    }

    #[tokio::test]
    async fn missing_incident_specific_owner_keeps_notification_pending() {
        let directory = tempfile::tempdir().unwrap();
        let (store, executor, id) = setup_with_owner(
            &directory,
            Ok(SendReceipt {
                message_id: "must-not-send".into(),
            }),
            "",
            true,
        );
        assert_eq!(executor.notify_pending(20).await.unwrap(), 0);
        assert_eq!(store.get(&id).unwrap().unwrap().notify_state, "pending");
    }

    #[test]
    fn alert_matches_node_fields_and_omits_empty_chat() {
        let mut incident = Incident {
            id: "inc_123".into(),
            fingerprint: "fingerprint".into(),
            code: "ERR_TEST".into(),
            raw_code: "ERR_TEST".into(),
            error_code: "ERR_TEST".into(),
            error_note: "暂无预置中文说明".into(),
            category: "internal".into(),
            severity: "info".into(),
            source: "pipeline".into(),
            chat_key: String::new(),
            session_id: String::new(),
            operation_id: String::new(),
            state: "open".into(),
            count: 2,
            first_at: 1,
            last_at: 2,
            message: "任务已取消".into(),
            details: serde_json::json!({}),
            stack_hash: String::new(),
            notify_state: "pending".into(),
            notified_at: 0,
            notify_error: String::new(),
            acknowledged_at: 0,
            resolved_at: 0,
            resolution: String::new(),
            version: 1,
        };

        let alert = format_incident_alert(&incident);
        assert!(alert.starts_with("【实验功能 · QQ Agent 异常】\n等级：信息\n模块：pipeline"));
        assert!(!alert.contains("会话："));
        assert!(alert.contains("错误代码：ERR_TEST"));
        assert!(alert.contains("原始结果：任务已取消\n次数：2\n编号：inc_123"));
        assert!(alert.ends_with("处理入口：控制台 → 异常"));

        incident.chat_key = "group:12345".into();
        assert!(format_incident_alert(&incident).contains("会话：group:12345"));
    }
}
