use serde_json::Value;

use crate::ports::gateway::SendReceipt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendOutcome {
    ConfirmedSuccess,
    ConfirmedFailure,
    Unknown,
}

pub fn may_retry_automatically(outcome: SendOutcome) -> bool {
    matches!(outcome, SendOutcome::ConfirmedFailure)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotificationSendError {
    ConfirmedFailure,
    Unknown,
    Internal(String),
}

#[async_trait::async_trait]
pub trait NotificationSender: Send + Sync {
    /// Best-effort refresh of media references for an archived message that
    /// was just recalled. Implementations must validate message ownership.
    async fn recalled_message_media(
        &self,
        _conversation_id: &str,
        _message_id: &str,
    ) -> Result<
        Option<Vec<crate::domain::message::MessageSegment>>,
        crate::ports::gateway::GatewayError,
    > {
        Ok(None)
    }

    async fn recalled_image_alternatives(
        &self,
        _file: &str,
    ) -> Result<Vec<String>, crate::ports::gateway::GatewayError> {
        Ok(Vec::new())
    }

    async fn send_owner_notification(
        &self,
        owner_uin: &str,
        payload: Value,
    ) -> Result<SendReceipt, NotificationSendError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_send_result_is_never_auto_retried() {
        assert!(!may_retry_automatically(SendOutcome::Unknown));
        assert!(may_retry_automatically(SendOutcome::ConfirmedFailure));
    }
}
