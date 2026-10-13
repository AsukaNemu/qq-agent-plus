CREATE TABLE IF NOT EXISTS group_invite_requests (
    id TEXT PRIMARY KEY,
    request_hash TEXT NOT NULL UNIQUE,
    group_id TEXT NOT NULL,
    inviter_id TEXT NOT NULL,
    subtype TEXT NOT NULL,
    comment TEXT NOT NULL DEFAULT '',
    state TEXT NOT NULL CHECK (state IN ('observed','deciding','approved','failed','held_unknown')),
    decision TEXT NOT NULL DEFAULT '',
    attempt_id TEXT NOT NULL DEFAULT '',
    action_error TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    completed_at INTEGER NOT NULL DEFAULT 0,
    notify_state TEXT NOT NULL DEFAULT 'pending'
        CHECK (notify_state IN ('pending','sending','sent','failed','held_unknown')),
    notify_attempt_id TEXT NOT NULL DEFAULT '',
    notify_error TEXT NOT NULL DEFAULT '',
    notify_message_id TEXT NOT NULL DEFAULT '',
    notified_at INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_group_invite_created
    ON group_invite_requests(created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_group_invite_notification
    ON group_invite_requests(notify_state, created_at);

CREATE TABLE IF NOT EXISTS group_invite_direct_entries (
    id TEXT PRIMARY KEY,
    group_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    operator_id TEXT NOT NULL DEFAULT '',
    joined_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE(group_id, user_id, joined_at)
);

CREATE TABLE IF NOT EXISTS group_invite_schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at INTEGER NOT NULL
);
