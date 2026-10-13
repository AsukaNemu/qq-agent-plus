CREATE TABLE IF NOT EXISTS recall_notifications (
    conversation_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('sending', 'sent', 'failed', 'unknown')),
    notification_message_id TEXT,
    error TEXT,
    notified_at INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (conversation_id, message_id)
);

CREATE INDEX IF NOT EXISTS recall_notifications_review
    ON recall_notifications(status, updated_at DESC);
