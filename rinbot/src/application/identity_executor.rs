//! Central executor for user-approved identity actions. Feature code and HTTP
//! handlers do not talk directly to NapCat; this layer brackets the external
//! command with durable state transitions.

use std::sync::{Arc, Mutex};

use serde::Serialize;
use thiserror::Error;

use crate::{
    identity::{FriendRequestAction, IdentityStore, IdentityStoreError, IncomingFriendRequest},
    ports::gateway::{FriendRequestGateway, GatewayError},
};

#[async_trait::async_trait]
pub trait PrivateWhitelist: Send + Sync {
    async fn allow_private_user(&self, user_id: &str) -> Result<(), String>;
}

#[derive(Debug, Error)]
pub enum IdentityActionError {
    #[error(transparent)]
    Store(#[from] IdentityStoreError),
    #[error("friend-request decisions are disabled in observation mode")]
    WritesDisabled,
    #[error("the configured automatic-whitelist behavior is not yet implemented")]
    WhitelistPolicyUnsupported,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FriendRequestDecisionResult {
    pub request: IncomingFriendRequest,
    pub execution: String,
    pub note: String,
}

pub struct IdentityActionExecutor {
    store: Arc<Mutex<IdentityStore>>,
    gateway: Arc<dyn FriendRequestGateway>,
    writes_enabled: bool,
    auto_whitelist: bool,
    private_whitelist: Option<Arc<dyn PrivateWhitelist>>,
}

impl IdentityActionExecutor {
    pub fn new(
        store: Arc<Mutex<IdentityStore>>,
        gateway: Arc<dyn FriendRequestGateway>,
        writes_enabled: bool,
        auto_whitelist: bool,
        private_whitelist: Option<Arc<dyn PrivateWhitelist>>,
    ) -> Self {
        Self {
            store,
            gateway,
            writes_enabled,
            auto_whitelist,
            private_whitelist,
        }
    }

    pub async fn decide(
        &self,
        id: &str,
        decision: &str,
        decided_by: &str,
        remark: &str,
        now: i64,
    ) -> Result<FriendRequestDecisionResult, IdentityActionError> {
        if !self.writes_enabled {
            return Err(IdentityActionError::WritesDisabled);
        }
        if decision == "approve" && self.auto_whitelist && self.private_whitelist.is_none() {
            return Err(IdentityActionError::WhitelistPolicyUnsupported);
        }
        let action = {
            let mut store = self
                .store
                .lock()
                .map_err(|_| IdentityStoreError::Internal)?;
            store.begin_decision(id, decision, decided_by, now)?
        };
        let gateway_result = self
            .gateway
            .decide_friend_request(
                &action.request_flag,
                action.decision == "approve",
                if action.decision == "approve" {
                    Some(remark)
                } else {
                    None
                },
            )
            .await;
        let mut result = self.record_result(&action, gateway_result, now)?;
        if result.execution == "approved" && self.auto_whitelist {
            let applied = self
                .private_whitelist
                .as_ref()
                .expect("validated above")
                .allow_private_user(&action.user_id)
                .await;
            let (success, error) = match applied {
                Ok(()) => (true, String::new()),
                Err(error) => (false, error),
            };
            let mut store = self
                .store
                .lock()
                .map_err(|_| IdentityStoreError::Internal)?;
            store.mark_whitelist(&action.id, success, &error, now)?;
            result.request = store
                .get_request(&action.id)?
                .ok_or_else(|| IdentityStoreError::Conflict("request disappeared".into()))?;
            if !success {
                result
                    .note
                    .push_str(" 私聊白名单同步失败，可在申请记录中手动重试。");
            }
        }
        Ok(result)
    }

    pub async fn retry_private_whitelist(
        &self,
        id: &str,
        now: i64,
    ) -> Result<IncomingFriendRequest, IdentityActionError> {
        if !self.writes_enabled {
            return Err(IdentityActionError::WritesDisabled);
        }
        if !self.auto_whitelist {
            return Err(IdentityActionError::WhitelistPolicyUnsupported);
        }
        let updater = self
            .private_whitelist
            .as_ref()
            .ok_or(IdentityActionError::WhitelistPolicyUnsupported)?;
        let request = self
            .store
            .lock()
            .map_err(|_| IdentityStoreError::Internal)?
            .get_request(id)?
            .ok_or_else(|| IdentityStoreError::Conflict("request does not exist".into()))?;
        if request.status != "approved" || request.whitelist_applied {
            return Err(IdentityStoreError::Conflict(
                "only approved requests without an applied whitelist can be retried".into(),
            )
            .into());
        }
        let applied = updater.allow_private_user(&request.user_id).await;
        let (success, error) = match applied {
            Ok(()) => (true, String::new()),
            Err(error) => (false, error),
        };
        let mut store = self
            .store
            .lock()
            .map_err(|_| IdentityStoreError::Internal)?;
        store.mark_whitelist(id, success, &error, now)?;
        let updated = store
            .get_request(id)?
            .ok_or_else(|| IdentityStoreError::Conflict("request disappeared".into()))?;
        if success {
            Ok(updated)
        } else {
            Err(IdentityStoreError::Invalid("私聊白名单同步失败，请稍后重试".into()).into())
        }
    }

    fn record_result(
        &self,
        action: &FriendRequestAction,
        gateway_result: Result<(), GatewayError>,
        now: i64,
    ) -> Result<FriendRequestDecisionResult, IdentityActionError> {
        let mut store = self
            .store
            .lock()
            .map_err(|_| IdentityStoreError::Internal)?;
        let result = match gateway_result {
            Ok(()) => {
                let terminal = if action.decision == "approve" {
                    "approved"
                } else {
                    "rejected"
                };
                let request =
                    store.complete_decision(&action.id, &action.attempt_id, terminal, "", now)?;
                FriendRequestDecisionResult {
                    request,
                    execution: terminal.into(),
                    note: if terminal == "approved" {
                        "好友申请已同意。".into()
                    } else {
                        "好友申请已拒绝。".into()
                    },
                }
            }
            Err(error) => {
                let details = error.to_string();
                let (status, execution, note) = match &error {
                    GatewayError::Request(_) | GatewayError::UnsupportedAction(_) => (
                        "failed",
                        "failed",
                        "OneBot 明确拒绝了此操作；不会自动重试。",
                    ),
                    GatewayError::WritesDisabled
                    | GatewayError::NotConnected
                    | GatewayError::UnknownOutcome => (
                        "held_unknown",
                        "held-unknown",
                        "操作结果未知；系统已停止自动重试，请在 QQ 客户端核对。",
                    ),
                };
                let request = store.complete_decision(
                    &action.id,
                    &action.attempt_id,
                    status,
                    &details,
                    now,
                )?;
                FriendRequestDecisionResult {
                    request,
                    execution: execution.into(),
                    note: note.into(),
                }
            }
        };
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::gateway::{FriendRequestGateway, GatewayError};
    use async_trait::async_trait;
    use tempfile::tempdir;

    struct MockGateway(Result<(), GatewayError>);

    struct MockWhitelist(Result<(), String>);

    #[async_trait::async_trait]
    impl PrivateWhitelist for MockWhitelist {
        async fn allow_private_user(&self, _user_id: &str) -> Result<(), String> {
            self.0.clone()
        }
    }

    #[async_trait]
    impl FriendRequestGateway for MockGateway {
        async fn decide_friend_request(
            &self,
            _request_flag: &str,
            _approve: bool,
            _remark: Option<&str>,
        ) -> Result<(), GatewayError> {
            match &self.0 {
                Ok(()) => Ok(()),
                Err(GatewayError::Request(message)) => Err(GatewayError::Request(message.clone())),
                Err(GatewayError::UnsupportedAction(action)) => {
                    Err(GatewayError::UnsupportedAction(action.clone()))
                }
                Err(GatewayError::UnknownOutcome) => Err(GatewayError::UnknownOutcome),
                Err(GatewayError::NotConnected) => Err(GatewayError::NotConnected),
                Err(GatewayError::WritesDisabled) => Err(GatewayError::WritesDisabled),
            }
        }
    }

    fn create_executor(
        directory: &tempfile::TempDir,
        response: Result<(), GatewayError>,
        writes_enabled: bool,
        auto_whitelist: bool,
        whitelist: Option<Arc<dyn PrivateWhitelist>>,
    ) -> (IdentityActionExecutor, String) {
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        let (request, _) = store
            .record_request("12345", "private-flag", "hello", 10, 100)
            .unwrap();
        (
            IdentityActionExecutor::new(
                Arc::new(Mutex::new(store)),
                Arc::new(MockGateway(response)),
                writes_enabled,
                auto_whitelist,
                whitelist,
            ),
            request.id,
        )
    }

    #[tokio::test]
    async fn approved_external_write_is_persisted_without_exposing_flag() {
        let directory = tempdir().unwrap();
        let (executor, id) = create_executor(&directory, Ok(()), true, false, None);
        let result = executor
            .decide(&id, "approve", "console-admin", "welcome", 200)
            .await
            .unwrap();
        assert_eq!(result.execution, "approved");
        assert_eq!(result.request.status, "approved");
        assert!(!serde_json::to_string(&result)
            .unwrap()
            .contains("private-flag"));
    }

    #[tokio::test]
    async fn ambiguous_write_is_quarantined_and_auto_whitelist_fails_before_claim() {
        let directory = tempdir().unwrap();
        let (executor, id) = create_executor(
            &directory,
            Err(GatewayError::UnknownOutcome),
            true,
            false,
            None,
        );
        let result = executor
            .decide(&id, "approve", "console-admin", "", 200)
            .await
            .unwrap();
        assert_eq!(result.execution, "held-unknown");

        let directory = tempdir().unwrap();
        let (executor, id) = create_executor(&directory, Ok(()), true, true, None);
        assert!(matches!(
            executor
                .decide(&id, "approve", "console-admin", "", 200)
                .await,
            Err(IdentityActionError::WhitelistPolicyUnsupported)
        ));
        let store = executor.store.lock().unwrap();
        assert_eq!(store.get_request(&id).unwrap().unwrap().status, "pending");
    }

    #[tokio::test]
    async fn confirmed_approval_applies_private_whitelist_after_decision() {
        let directory = tempfile::tempdir().unwrap();
        let whitelist: Arc<dyn PrivateWhitelist> = Arc::new(MockWhitelist(Ok(())));
        let (executor, id) = create_executor(&directory, Ok(()), true, true, Some(whitelist));
        let result = executor
            .decide(&id, "approve", "console-admin", "", 200)
            .await
            .unwrap();
        assert_eq!(result.request.status, "approved");
        assert!(result.request.whitelist_applied);
    }
}
