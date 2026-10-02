PRAGMA journal_mode = WAL;
PRAGMA synchronous = FULL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS inbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    received_at INTEGER NOT NULL,
    event_json TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'queued',
    attempts INTEGER NOT NULL DEFAULT 0,
    lease_id TEXT,
    lease_expires_at INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    UNIQUE (conversation_id, message_id)
);

CREATE INDEX IF NOT EXISTS inbox_state_lookup
    ON inbox (state, received_at, id);

CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    inbox_id INTEGER NOT NULL UNIQUE REFERENCES inbox(id),
    conversation_id TEXT NOT NULL,
    sender_id TEXT NOT NULL,
    is_group INTEGER NOT NULL,
    text TEXT NOT NULL,
    segments_json TEXT NOT NULL,
    message_timestamp INTEGER NOT NULL,
    stored_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS messages_conversation_time
    ON messages (conversation_id, message_timestamp DESC, id DESC);

CREATE TABLE IF NOT EXISTS outbox (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN (
        'pending', 'sending', 'sent', 'failed', 'unknown',
        'reconciled_sent', 'reconciled_failed'
    )),
    attempts INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    message_id TEXT,
    error TEXT
);

CREATE INDEX IF NOT EXISTS outbox_review_lookup
    ON outbox (state, updated_at DESC);

CREATE TABLE IF NOT EXISTS runtime_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
