use async_trait::async_trait;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendReceipt {
    pub message_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginInfo {
    pub user_id: String,
    pub nickname: String,
}

#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("outbound writes are disabled in observation mode")]
    WritesDisabled,
    #[error("gateway is not connected")]
    NotConnected,
    #[error("gateway request failed: {0}")]
    Request(String),
    #[error("gateway returned an unknown outcome")]
    UnknownOutcome,
}

#[async_trait]
pub trait MessageGateway: Send + Sync {
    async fn get_login_info(&self) -> Result<LoginInfo, GatewayError>;
    async fn send_text(
        &self,
        conversation_id: &str,
        text: &str,
    ) -> Result<SendReceipt, GatewayError>;
}
