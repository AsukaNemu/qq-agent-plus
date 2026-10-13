//! Durable owner notifications for the identity module.
//!
//! The notification intent is written before calling NapCat. A process crash or
//! ambiguous OneBot result is quarantined as `held_unknown` and never retried
//! automatically; an operator may explicitly retry only a definite failure.

use std::sync::{Arc, Mutex};

use thiserror::Error;

use crate::{
    application::outbox::{NotificationSendError, NotificationSender},
    identity::{IdentityStore, IdentityStoreError, IncomingFriendRequest},
};

#[derive(Debug, Error)]
pub enum IdentityNotificationError {
    #[error(transparent)]
    Store(#[from] IdentityStoreError),
    #[error("identity notifications are disabled")]
    Disabled,
}

pub struct IdentityNotificationExecutor {
    store: Arc<Mutex<IdentityStore>>,
    sender: Arc<dyn NotificationSender>,
    enabled: bool,
}

impl IdentityNotificationExecutor {
    pub fn new(
        store: Arc<Mutex<IdentityStore>>,
        sender: Arc<dyn NotificationSender>,
        enabled: bool,
    ) -> Self {
        Self {
            store,
            sender,
            enabled,
        }
    }

    pub async fn notify_owner(
        &self,
        request_id: &str,
        owner_uin: &str,
        now: i64,
    ) -> Result<IncomingFriendRequest, IdentityNotificationError> {
        if !self.enabled {
            return Err(IdentityNotificationError::Disabled);
        }
        let attempt_id = uuid::Uuid::new_v4().to_string();
        let request = self
            .store
            .lock()
            .map_err(|_| IdentityStoreError::Internal)?
            .begin_notification(request_id, &attempt_id, owner_uin, now)?;
        let message = format!(
            "RInBot 收到新的 QQ 好友申请\n申请人：{}\n附言：{}\n请在管理页面查看并处理。",
            request.user_id,
            if request.comment.is_empty() {
                "（无附言）"
            } else {
                &request.comment
            }
        );
        let outcome = self
            .sender
            .send_owner_notification(owner_uin, serde_json::json!({"text":message}))
            .await;
        let (status, message_id, error) = match outcome {
            Ok(receipt) => ("sent", receipt.message_id, String::new()),
            Err(NotificationSendError::Unknown) => (
                "held_unknown",
                String::new(),
                "OneBot 通知结果未知；请在 QQ 客户端核对，系统不会自动重发。".into(),
            ),
            Err(NotificationSendError::ConfirmedFailure | NotificationSendError::Internal(_)) => (
                "failed",
                String::new(),
                "通知未确认送达；请核对 QQ 和状态后手动重试。".into(),
            ),
        };
        let mut store = self
            .store
            .lock()
            .map_err(|_| IdentityStoreError::Internal)?;
        Ok(store.complete_notification(
            request_id,
            &attempt_id,
            status,
            &message_id,
            &error,
            now,
        )?)
    }

    pub async fn notify_pending(
        &self,
        owner_uin: &str,
        now: i64,
    ) -> Result<u64, IdentityNotificationError> {
        if !self.enabled {
            return Err(IdentityNotificationError::Disabled);
        }
        let pending = self
            .store
            .lock()
            .map_err(|_| IdentityStoreError::Internal)?
            .pending_notification_ids(500)?;
        let mut attempted = 0;
        for id in pending {
            match self.notify_owner(&id, owner_uin, now).await {
                Ok(_) => attempted += 1,
                Err(IdentityNotificationError::Store(IdentityStoreError::Conflict(_))) => {
                    // A live event or an operator action claimed it first.
                }
                Err(error) => return Err(error),
            }
        }
        Ok(attempted)
    }

    pub fn quarantine_interrupted(&self, now: i64) -> Result<u64, IdentityNotificationError> {
        if !self.enabled {
            return Ok(0);
        }
        Ok(self
            .store
            .lock()
            .map_err(|_| IdentityStoreError::Internal)?
            .quarantine_interrupted_notifications(now)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        application::outbox::{NotificationSendError, NotificationSender},
        ports::gateway::SendReceipt,
    };
    use async_trait::async_trait;
    use tempfile::tempdir;

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
    ) -> (IdentityNotificationExecutor, String) {
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        let (request, _) = store
            .record_request("12345", "opaque-flag", "请处理申请", 10, 100)
            .unwrap();
        (
            IdentityNotificationExecutor::new(
                Arc::new(Mutex::new(store)),
                Arc::new(Sender(result)),
                true,
            ),
            request.id,
        )
    }

    #[tokio::test]
    async fn confirmed_notification_is_durable_and_notified_at_is_set() {
        let directory = tempdir().unwrap();
        let (executor, id) = setup(
            &directory,
            Ok(SendReceipt {
                message_id: "m-1".into(),
            }),
        );
        let request = executor.notify_owner(&id, "998877", 200).await.unwrap();
        assert_eq!(request.notification_state, "sent");
        assert_eq!(request.notification_message_id, "m-1");
        assert_eq!(request.notified_at, 200);
        assert!(executor.notify_owner(&id, "998877", 300).await.is_err());
    }

    #[tokio::test]
    async fn unknown_notification_is_held_and_cannot_be_retried() {
        let directory = tempdir().unwrap();
        let (executor, id) = setup(&directory, Err(NotificationSendError::Unknown));
        let request = executor.notify_owner(&id, "998877", 200).await.unwrap();
        assert_eq!(request.notification_state, "held_unknown");
        assert_eq!(request.notified_at, 0);
        assert!(executor.notify_owner(&id, "998877", 300).await.is_err());
    }

    #[tokio::test]
    async fn interrupted_notification_is_quarantined_as_unknown() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        let (request, _) = store
            .record_request("12345", "opaque-flag", "", 10, 100)
            .unwrap();
        store
            .begin_notification(&request.id, "attempt", "998877", 150)
            .unwrap();
        let executor = IdentityNotificationExecutor::new(
            Arc::new(Mutex::new(store)),
            Arc::new(Sender(Err(NotificationSendError::Unknown))),
            true,
        );
        assert_eq!(executor.quarantine_interrupted(200).unwrap(), 1);
        assert!(executor
            .notify_owner(&request.id, "998877", 300)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn startup_replays_only_never_attempted_notifications() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        let (pending, _) = store
            .record_request("11111", "flag-1", "", 10, 100)
            .unwrap();
        let (failed, _) = store
            .record_request("22222", "flag-2", "", 10, 100)
            .unwrap();
        store
            .begin_notification(&failed.id, "failed-attempt", "998877", 110)
            .unwrap();
        store
            .complete_notification(
                &failed.id,
                "failed-attempt",
                "failed",
                "",
                "definite failure",
                120,
            )
            .unwrap();
        let (unknown, _) = store
            .record_request("33333", "flag-3", "", 10, 100)
            .unwrap();
        store
            .begin_notification(&unknown.id, "unknown-attempt", "998877", 110)
            .unwrap();
        store.quarantine_interrupted_notifications(120).unwrap();
        let executor = IdentityNotificationExecutor::new(
            Arc::new(Mutex::new(store)),
            Arc::new(Sender(Ok(SendReceipt {
                message_id: "replayed".into(),
            }))),
            true,
        );
        assert_eq!(executor.notify_pending("998877", 200).await.unwrap(), 1);
        let store = executor.store.lock().unwrap();
        assert_eq!(
            store
                .get_request(&pending.id)
                .unwrap()
                .unwrap()
                .notification_state,
            "sent"
        );
        assert_eq!(
            store
                .get_request(&failed.id)
                .unwrap()
                .unwrap()
                .notification_state,
            "failed"
        );
        assert_eq!(
            store
                .get_request(&unknown.id)
                .unwrap()
                .unwrap()
                .notification_state,
            "held_unknown"
        );
    }
}
