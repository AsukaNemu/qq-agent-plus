ALTER TABLE inbox ADD COLUMN legacy_source_json TEXT;
ALTER TABLE inbox ADD COLUMN available_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN sender_name TEXT NOT NULL DEFAULT '';
ALTER TABLE messages ADD COLUMN is_self INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN reply_json TEXT;
ALTER TABLE messages ADD COLUMN media_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE messages ADD COLUMN mentions_self INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN target_user_id TEXT NOT NULL DEFAULT '';
ALTER TABLE messages ADD COLUMN event_kind TEXT NOT NULL DEFAULT 'message';
ALTER TABLE messages ADD COLUMN legacy_source_json TEXT;
ALTER TABLE outbox ADD COLUMN legacy_run_id TEXT;
ALTER TABLE outbox ADD COLUMN legacy_source_json TEXT;

CREATE TABLE IF NOT EXISTS legacy_runs (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    state TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    error TEXT,
    legacy_source_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS migration_imports (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_inventory_json TEXT NOT NULL,
    source_message_rows INTEGER NOT NULL,
    source_outbox_rows INTEGER NOT NULL,
    source_run_rows INTEGER NOT NULL,
    imported_at INTEGER NOT NULL
);
