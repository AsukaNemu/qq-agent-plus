use tokio::sync::mpsc;

pub const DEFAULT_MESSAGE_QUEUE_CAPACITY: usize = 256;
pub type InboxId = i64;

#[derive(Clone, Debug)]
pub enum WakeTarget {
    Inbox(InboxId),
    Conversation(String),
}

pub fn bounded_inbox_queue(
    capacity: usize,
) -> (mpsc::Sender<WakeTarget>, mpsc::Receiver<WakeTarget>) {
    mpsc::channel(capacity.max(1))
}
