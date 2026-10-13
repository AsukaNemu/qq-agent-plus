ALTER TABLE incoming_friend_requests ADD COLUMN notify_state TEXT NOT NULL DEFAULT 'pending';
ALTER TABLE incoming_friend_requests ADD COLUMN notify_attempt_id TEXT NOT NULL DEFAULT '';
ALTER TABLE incoming_friend_requests ADD COLUMN notify_recipient TEXT NOT NULL DEFAULT '';
ALTER TABLE incoming_friend_requests ADD COLUMN notify_message_id TEXT NOT NULL DEFAULT '';

UPDATE incoming_friend_requests
SET notify_state = CASE
    WHEN notified_at > 0 THEN 'sent'
    WHEN notify_error <> '' THEN 'held_unknown'
    ELSE 'pending'
END;

CREATE INDEX IF NOT EXISTS idx_identity_friend_notifications
    ON incoming_friend_requests(notify_state, created_at DESC);
