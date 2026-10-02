use serde::Serialize;

use super::message::IncomingMessage;

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DomainEvent {
    Message(IncomingMessage),
    Unknown {
        post_type: String,
        event_type: String,
    },
}
