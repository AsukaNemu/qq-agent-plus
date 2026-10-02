use tokio::sync::mpsc;

pub const DEFAULT_MESSAGE_QUEUE_CAPACITY: usize = 256;
pub type InboxId = i64;

pub fn bounded_inbox_queue(capacity: usize) -> (mpsc::Sender<InboxId>, mpsc::Receiver<InboxId>) {
    mpsc::channel(capacity.max(1))
}
