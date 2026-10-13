use serde::Serialize;

use super::message::IncomingMessage;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FriendSummary {
    pub user_id: String,
    pub nickname: String,
}

#[derive(Clone, PartialEq, Eq)]
pub struct FriendRequestFlag(String);

impl FriendRequestFlag {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn expose_to_gateway(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for FriendRequestFlag {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("FriendRequestFlag([REDACTED])")
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DomainEvent {
    Message(IncomingMessage),
    FriendListSnapshot {
        friends: Vec<FriendSummary>,
    },
    FriendListSnapshotUnavailable,
    FriendAdded {
        user_id: String,
    },
    FriendRequest {
        user_id: String,
        #[serde(skip_serializing)]
        flag: FriendRequestFlag,
        comment: String,
    },
    GroupRequest {
        group_id: String,
        user_id: String,
        sub_type: String,
        #[serde(skip_serializing)]
        flag: FriendRequestFlag,
        comment: String,
    },
    GroupIncreased {
        group_id: String,
        user_id: String,
        operator_id: String,
        time: i64,
        is_bot: bool,
    },
    GroupDecreased {
        group_id: String,
        user_id: String,
        sub_type: String,
        is_bot: bool,
    },
    MessageRecalled {
        conversation_id: String,
        message_id: String,
        operator_id: String,
    },
    Unknown {
        post_type: String,
        event_type: String,
    },
}
