CREATE TABLE IF NOT EXISTS incoming_friend_requests (
    id TEXT PRIMARY KEY,
    request_flag TEXT NOT NULL UNIQUE,
    user_id TEXT NOT NULL,
    comment TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'pending',
    decision TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    decided_at INTEGER NOT NULL DEFAULT 0,
    decided_by TEXT NOT NULL DEFAULT '',
    attempt_id TEXT NOT NULL DEFAULT '',
    action_started_at INTEGER NOT NULL DEFAULT 0,
    action_completed_at INTEGER NOT NULL DEFAULT 0,
    action_error TEXT NOT NULL DEFAULT '',
    notified_at INTEGER NOT NULL DEFAULT 0,
    notify_error TEXT NOT NULL DEFAULT '',
    whitelist_applied INTEGER NOT NULL DEFAULT 0,
    whitelist_error TEXT NOT NULL DEFAULT '',
    updated_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_identity_friend_requests_recent
    ON incoming_friend_requests(created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_identity_friend_requests_state
    ON incoming_friend_requests(status, created_at DESC);
