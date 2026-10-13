//! Executes one durable group-invitation decision through the narrow NapCat port.
//! Policy selection remains with the runtime; no feature code owns a NapCat client.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    application::outbox::{NotificationSendError, NotificationSender},
    domain::event::FriendRequestFlag,
    group_invite::{GroupInviteAction, GroupInviteError, GroupInviteRequest, GroupInviteStore},
    ports::gateway::{GatewayError, GroupRequestGateway},
};

#[derive(Debug, Error)]
pub enum GroupInviteExecutionError {
    #[error(transparent)]
    Store(#[from] GroupInviteError),
    #[error("group invitation writes are disabled in observation mode")]
    WritesDisabled,
    #[error("group invitation owner notifications are disabled")]
    NotificationsDisabled,
}

/// Persistent operational controls for the independently owned group-invite module.
/// Graduation is intentionally absent: enabling runtime behavior and graduating a
/// feature are separate lifecycle decisions.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GroupInviteSettings {
    pub enabled: bool,
    pub friends_only: bool,
    pub auto_whitelist: bool,
    pub notify_owner: bool,
}

impl Default for GroupInviteSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            friends_only: true,
            auto_whitelist: true,
            notify_owner: true,
        }
    }
}

pub struct GroupInviteNotificationExecutor {
    store: Arc<Mutex<GroupInviteStore>>,
    sender: Arc<dyn NotificationSender>,
    enabled: bool,
}

impl GroupInviteNotificationExecutor {
    pub fn new(
        store: Arc<Mutex<GroupInviteStore>>,
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
    ) -> Result<GroupInviteRequest, GroupInviteExecutionError> {
        if !self.enabled {
            return Err(GroupInviteExecutionError::NotificationsDisabled);
        }
        let attempt_id = uuid::Uuid::new_v4().to_string();
        let request = self
            .store
            .lock()
            .map_err(|_| GroupInviteError::Conflict("group invitation store lock poisoned".into()))?
            .begin_notification(request_id, &attempt_id, owner_uin, now)?;
        let note = if request.comment.is_empty() {
            "（无附言）"
        } else {
            &request.comment
        };
        let review = if request.review_reason.is_empty() {
            String::new()
        } else {
            format!("\n未自动同意：{}", request.review_reason)
        };
        let action_detail = if matches!(request.state.as_str(), "failed" | "held_unknown")
            && !request.action_error.is_empty()
        {
            format!("\n审批结果：{}", request.action_error)
        } else {
            String::new()
        };
        let message = format!(
            "RInBot 收到群邀请申请\n群号：{}\n邀请人：{}\n附言：{}\n处理状态：{}{}{}\n请在管理页面查看。",
            request.group_id, request.inviter_id, note, request.state, review, action_detail
        );
        let outcome = self
            .sender
            .send_owner_notification(owner_uin, serde_json::json!({"text":message}))
            .await;
        let (state, message_id, error) = match outcome {
            Ok(receipt) => ("sent", receipt.message_id, String::new()),
            Err(NotificationSendError::Unknown) => (
                "held_unknown",
                String::new(),
                "OneBot 通知结果未知；请在 QQ 客户端核对，系统不会自动重发。".into(),
            ),
            Err(NotificationSendError::ConfirmedFailure | NotificationSendError::Internal(_)) => (
                "failed",
                String::new(),
                "通知未确认送达；核对台账和 QQ 后可手动重试。".into(),
            ),
        };
        self.store
            .lock()
            .map_err(|_| GroupInviteError::Conflict("group invitation store lock poisoned".into()))?
            .complete_notification(request_id, &attempt_id, state, &message_id, &error, now)
            .map_err(Into::into)
    }

    pub async fn notify_direct_entry_owner(
        &self,
        entry_id: &str,
        owner_uin: &str,
        now: i64,
    ) -> Result<crate::group_invite::GroupInviteDirectEntry, GroupInviteExecutionError> {
        if !self.enabled {
            return Err(GroupInviteExecutionError::NotificationsDisabled);
        }
        let attempt_id = uuid::Uuid::new_v4().to_string();
        let entry = self
            .store
            .lock()
            .map_err(|_| GroupInviteError::Conflict("group invitation store lock poisoned".into()))?
            .begin_direct_notification(entry_id, &attempt_id, owner_uin, now)?;
        let message = format!(
            "RInBot 已直接加入群\n群号：{}\n新成员：{}\n操作人：{}\n{}请在管理页面查看入群记录。",
            entry.group_id,
            entry.user_id,
            if entry.operator_id.is_empty() {
                "未知"
            } else {
                &entry.operator_id
            },
            match entry.whitelist_state.as_str() {
                "applied" => "已加入聊天白名单。\n",
                "failed" => "加入聊天白名单失败。\n",
                _ if entry.whitelist_error == "自动加入聊天白名单已关闭" => {
                    "自动加入聊天白名单已关闭。\n"
                }
                _ => "",
            },
        );
        let outcome = self
            .sender
            .send_owner_notification(owner_uin, serde_json::json!({"text":message}))
            .await;
        let (state, message_id, error) = match outcome {
            Ok(receipt) => ("sent", receipt.message_id, String::new()),
            Err(NotificationSendError::Unknown) => (
                "held_unknown",
                String::new(),
                "OneBot 通知结果未知；请在 QQ 客户端核对，系统不会自动重发。".into(),
            ),
            Err(NotificationSendError::ConfirmedFailure | NotificationSendError::Internal(_)) => (
                "failed",
                String::new(),
                "通知未确认送达；核对台账和 QQ 后可手动重试。".into(),
            ),
        };
        self.store
            .lock()
            .map_err(|_| GroupInviteError::Conflict("group invitation store lock poisoned".into()))?
            .complete_direct_notification(entry_id, &attempt_id, state, &message_id, &error, now)
            .map_err(Into::into)
    }

    pub async fn notify_pending(
        &self,
        owner_uin: &str,
        now: i64,
    ) -> Result<u64, GroupInviteExecutionError> {
        if !self.enabled {
            return Ok(0);
        }
        let pending = self
            .store
            .lock()
            .map_err(|_| GroupInviteError::Conflict("group invitation store lock poisoned".into()))?
            .pending_notification_ids(500)?;
        let mut attempted = 0;
        for id in pending {
            match self.notify_owner(&id, owner_uin, now).await {
                Ok(_) => attempted += 1,
                Err(GroupInviteExecutionError::Store(GroupInviteError::Conflict(_))) => {}
                Err(error) => return Err(error),
            }
        }
        let direct_pending = self
            .store
            .lock()
            .map_err(|_| GroupInviteError::Conflict("group invitation store lock poisoned".into()))?
            .pending_direct_notification_ids(500)?;
        for id in direct_pending {
            match self.notify_direct_entry_owner(&id, owner_uin, now).await {
                Ok(_) => attempted += 1,
                Err(GroupInviteExecutionError::Store(GroupInviteError::Conflict(_))) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(attempted)
    }

    pub fn quarantine_interrupted(&self, now: i64) -> Result<u64, GroupInviteExecutionError> {
        if !self.enabled {
            return Ok(0);
        }
        let mut store = self.store.lock().map_err(|_| {
            GroupInviteError::Conflict("group invitation store lock poisoned".into())
        })?;
        Ok(store.quarantine_interrupted_notifications(now)?
            + store.quarantine_interrupted_direct_notifications(now)?)
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupInviteExecutionResult {
    pub request: Option<GroupInviteRequest>,
    pub execution: String,
    pub duplicate: bool,
}

pub struct GroupInviteExecutor {
    store: Arc<Mutex<GroupInviteStore>>,
    gateway: Arc<dyn GroupRequestGateway>,
    writes_enabled: bool,
}

impl GroupInviteExecutor {
    pub fn new(
        store: Arc<Mutex<GroupInviteStore>>,
        gateway: Arc<dyn GroupRequestGateway>,
        writes_enabled: bool,
    ) -> Self {
        Self {
            store,
            gateway,
            writes_enabled,
        }
    }

    pub async fn observe(
        &self,
        group_id: &str,
        inviter_id: &str,
        subtype: &str,
        comment: &str,
        review_reason: &str,
        flag: FriendRequestFlag,
        approve: bool,
        notify_owner: bool,
        auto_whitelist: bool,
        now: i64,
    ) -> Result<GroupInviteExecutionResult, GroupInviteExecutionError> {
        if approve && !self.writes_enabled {
            return Err(GroupInviteExecutionError::WritesDisabled);
        }
        let (action, duplicate, request_id) = {
            let mut store = self.store.lock().map_err(|_| {
                GroupInviteError::Conflict("group invitation store lock poisoned".into())
            })?;
            store.observe_request(
                group_id,
                inviter_id,
                subtype,
                comment,
                review_reason,
                flag,
                approve,
                notify_owner,
                now,
            )?
        };
        let Some(action) = action else {
            let request = self
                .store
                .lock()
                .map_err(|_| {
                    GroupInviteError::Conflict("group invitation store lock poisoned".into())
                })?
                .get(&request_id)?;
            return Ok(GroupInviteExecutionResult {
                request,
                execution: if duplicate { "duplicate" } else { "observed" }.into(),
                duplicate,
            });
        };
        let outcome = self
            .gateway
            .decide_group_request(
                action.request_flag.expose_to_gateway(),
                &action.subtype,
                true,
            )
            .await;
        self.complete(&action, outcome, auto_whitelist, now)
    }

    fn complete(
        &self,
        action: &GroupInviteAction,
        outcome: Result<(), GatewayError>,
        auto_whitelist: bool,
        now: i64,
    ) -> Result<GroupInviteExecutionResult, GroupInviteExecutionError> {
        let (state, error) = match outcome {
            Ok(()) => ("approved", String::new()),
            Err(GatewayError::UnknownOutcome) => {
                ("held_unknown", "group decision outcome is unknown".into())
            }
            Err(GatewayError::NotConnected) => ("failed", "OneBot gateway is not connected".into()),
            Err(GatewayError::Request(reason)) => ("failed", reason),
            Err(GatewayError::UnsupportedAction(_)) => {
                ("failed", "OneBot action is unsupported".into())
            }
            Err(GatewayError::WritesDisabled) => ("failed", "gateway write disabled".into()),
        };
        let mut store = self.store.lock().map_err(|_| {
            GroupInviteError::Conflict("group invitation store lock poisoned".into())
        })?;
        let request = store.complete_decision(action, state, &error, auto_whitelist, now)?;
        Ok(GroupInviteExecutionResult {
            request: Some(request),
            execution: state.into(),
            duplicate: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        application::outbox::{NotificationSendError, NotificationSender},
        ports::gateway::{GroupRequestGateway, SendReceipt},
    };
    use std::{
        collections::VecDeque,
        sync::atomic::{AtomicUsize, Ordering},
    };
    use tempfile::tempdir;

    struct Gateway {
        result: Result<(), GatewayError>,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl GroupRequestGateway for Gateway {
        async fn decide_group_request(
            &self,
            _request_flag: &str,
            _subtype: &str,
            _approve: bool,
        ) -> Result<(), GatewayError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match &self.result {
                Ok(()) => Ok(()),
                Err(GatewayError::UnknownOutcome) => Err(GatewayError::UnknownOutcome),
                Err(GatewayError::NotConnected) => Err(GatewayError::NotConnected),
                Err(GatewayError::WritesDisabled) => Err(GatewayError::WritesDisabled),
                Err(GatewayError::Request(error)) => Err(GatewayError::Request(error.clone())),
                Err(GatewayError::UnsupportedAction(action)) => {
                    Err(GatewayError::UnsupportedAction(action.clone()))
                }
            }
        }
    }

    fn executor(
        path: &std::path::Path,
        gateway: Gateway,
        writes_enabled: bool,
    ) -> GroupInviteExecutor {
        GroupInviteExecutor::new(
            Arc::new(Mutex::new(GroupInviteStore::open(path).unwrap())),
            Arc::new(gateway),
            writes_enabled,
        )
    }

    #[tokio::test]
    async fn confirmed_decision_persists_and_duplicate_event_does_not_call_gateway_again() {
        let directory = tempdir().unwrap();
        let executor = executor(
            &directory.path().join("group-invite.sqlite"),
            Gateway {
                result: Ok(()),
                calls: Arc::new(AtomicUsize::new(0)),
            },
            true,
        );
        let first = executor
            .observe(
                "12345",
                "67890",
                "invite",
                "",
                "",
                FriendRequestFlag::new("f-1".into()),
                true,
                false,
                true,
                100,
            )
            .await
            .unwrap();
        assert_eq!(first.execution, "approved");
        let request = first.request.unwrap();
        assert_eq!(request.state, "approved");
        assert_eq!(request.whitelist_state, "pending");
        let duplicate = executor
            .observe(
                "12345",
                "67890",
                "invite",
                "",
                "",
                FriendRequestFlag::new("f-1".into()),
                true,
                false,
                true,
                200,
            )
            .await
            .unwrap();
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.execution, "duplicate");
    }

    #[tokio::test]
    async fn ambiguous_decision_is_held_and_observation_mode_never_creates_a_write_attempt() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("group-invite.sqlite");
        let calls = Arc::new(AtomicUsize::new(0));
        let exec = executor(
            &path,
            Gateway {
                result: Err(GatewayError::UnknownOutcome),
                calls: calls.clone(),
            },
            true,
        );
        let result = exec
            .observe(
                "12345",
                "67890",
                "invite",
                "",
                "",
                FriendRequestFlag::new("f-2".into()),
                true,
                false,
                true,
                100,
            )
            .await
            .unwrap();
        assert_eq!(result.execution, "held_unknown");
        assert_eq!(result.request.unwrap().state, "held_unknown");
        let duplicate = exec
            .observe(
                "12345",
                "67890",
                "invite",
                "",
                "",
                FriendRequestFlag::new("f-2".into()),
                true,
                false,
                true,
                200,
            )
            .await
            .unwrap();
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.execution, "duplicate");
        assert_eq!(duplicate.request.unwrap().state, "held_unknown");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let disabled = executor(
            &directory.path().join("observe.sqlite"),
            Gateway {
                result: Ok(()),
                calls: Arc::new(AtomicUsize::new(0)),
            },
            false,
        );
        assert!(matches!(
            disabled
                .observe(
                    "12345",
                    "67890",
                    "invite",
                    "",
                    "",
                    FriendRequestFlag::new("f-3".into()),
                    true,
                    false,
                    true,
                    100,
                )
                .await,
            Err(GroupInviteExecutionError::WritesDisabled)
        ));
    }

    struct NotifyGateway {
        responses: Mutex<VecDeque<Result<SendReceipt, NotificationSendError>>>,
        sends: AtomicUsize,
        payloads: Mutex<Vec<serde_json::Value>>,
    }

    #[async_trait::async_trait]
    impl NotificationSender for NotifyGateway {
        async fn send_owner_notification(
            &self,
            _owner_uin: &str,
            payload: serde_json::Value,
        ) -> Result<SendReceipt, NotificationSendError> {
            self.sends.fetch_add(1, Ordering::SeqCst);
            self.payloads.lock().unwrap().push(payload);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(NotificationSendError::Unknown))
        }
    }

    #[tokio::test]
    async fn notification_unknown_is_quarantined_but_definite_failure_is_explicitly_retryable() {
        let directory = tempdir().unwrap();
        let store = Arc::new(Mutex::new(
            GroupInviteStore::open(directory.path().join("group-invite.sqlite")).unwrap(),
        ));
        let (_, _, unknown_id) = store
            .lock()
            .unwrap()
            .observe_request(
                "12345",
                "67890",
                "invite",
                "",
                "好友策略未满足",
                FriendRequestFlag::new("notify-unknown".into()),
                false,
                true,
                100,
            )
            .unwrap();
        let gateway = Arc::new(NotifyGateway {
            responses: Mutex::new(VecDeque::from([Err(NotificationSendError::Unknown)])),
            sends: AtomicUsize::new(0),
            payloads: Mutex::new(Vec::new()),
        });
        let executor = GroupInviteNotificationExecutor::new(store.clone(), gateway.clone(), true);
        let held = executor
            .notify_owner(&unknown_id, "998877", 110)
            .await
            .unwrap();
        assert_eq!(held.notify_state, "held_unknown");
        assert_eq!(held.review_reason, "好友策略未满足");
        assert!(gateway.payloads.lock().unwrap()[0]["text"]
            .as_str()
            .unwrap()
            .contains("未自动同意：好友策略未满足"));
        assert!(executor
            .notify_owner(&unknown_id, "998877", 120)
            .await
            .is_err());
        assert_eq!(gateway.sends.load(Ordering::SeqCst), 1);

        let (_, _, failed_id) = store
            .lock()
            .unwrap()
            .observe_request(
                "12345",
                "67890",
                "invite",
                "",
                "",
                FriendRequestFlag::new("notify-failed".into()),
                false,
                true,
                130,
            )
            .unwrap();
        gateway
            .responses
            .lock()
            .unwrap()
            .push_back(Err(NotificationSendError::ConfirmedFailure));
        let failed = executor
            .notify_owner(&failed_id, "998877", 140)
            .await
            .unwrap();
        assert_eq!(failed.notify_state, "failed");
        gateway.responses.lock().unwrap().push_back(Ok(SendReceipt {
            message_id: "confirmed-2".into(),
        }));
        let sent = executor
            .notify_owner(&failed_id, "998877", 150)
            .await
            .unwrap();
        assert_eq!(sent.notify_state, "sent");
        assert_eq!(sent.notify_message_id, "confirmed-2");
        assert_eq!(gateway.sends.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn direct_entry_notifications_quarantine_unknown_and_allow_confirmed_failure_retry() {
        let directory = tempdir().unwrap();
        let store = Arc::new(Mutex::new(
            GroupInviteStore::open(directory.path().join("group-invite.sqlite")).unwrap(),
        ));
        let (unknown_id, _) = store
            .lock()
            .unwrap()
            .record_direct_entry(
                "12345",
                "2676659414",
                "998877",
                500,
                false,
                false,
                true,
                501,
            )
            .unwrap();
        let (failed_id, _) = store
            .lock()
            .unwrap()
            .record_direct_entry(
                "12346",
                "2676659415",
                "998877",
                600,
                false,
                false,
                true,
                601,
            )
            .unwrap();
        let gateway = Arc::new(NotifyGateway {
            responses: Mutex::new(VecDeque::from([
                Err(NotificationSendError::Unknown),
                Err(NotificationSendError::ConfirmedFailure),
                Ok(SendReceipt {
                    message_id: "direct-confirmed".into(),
                }),
            ])),
            sends: AtomicUsize::new(0),
            payloads: Mutex::new(Vec::new()),
        });
        let executor = GroupInviteNotificationExecutor::new(store.clone(), gateway.clone(), true);
        let held = executor
            .notify_direct_entry_owner(&unknown_id, "998877", 700)
            .await
            .unwrap();
        assert_eq!(held.notify_state, "held_unknown");
        assert!(executor
            .notify_direct_entry_owner(&unknown_id, "998877", 701)
            .await
            .is_err());
        let failed = executor
            .notify_direct_entry_owner(&failed_id, "998877", 702)
            .await
            .unwrap();
        assert_eq!(failed.notify_state, "failed");
        let sent = executor
            .notify_direct_entry_owner(&failed_id, "998877", 703)
            .await
            .unwrap();
        assert_eq!(sent.notify_state, "sent");
        assert_eq!(sent.notify_message_id, "direct-confirmed");
        assert_eq!(gateway.sends.load(Ordering::SeqCst), 3);
    }
}
