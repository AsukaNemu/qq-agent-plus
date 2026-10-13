CREATE TABLE IF NOT EXISTS legacy_sessions (
    session_id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL DEFAULT '',
    started_at INTEGER NOT NULL DEFAULT 0,
    ended_at INTEGER,
    status TEXT NOT NULL DEFAULT 'unknown',
    summary_json TEXT NOT NULL,
    source_bytes INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_legacy_sessions_recent
    ON legacy_sessions(started_at DESC, session_id DESC);
