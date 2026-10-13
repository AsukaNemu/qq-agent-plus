CREATE TABLE IF NOT EXISTS conversation_handoffs (
    chat_key TEXT PRIMARY KEY,
    handoff_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_conversation_handoffs_expiry
    ON conversation_handoffs(expires_at);
