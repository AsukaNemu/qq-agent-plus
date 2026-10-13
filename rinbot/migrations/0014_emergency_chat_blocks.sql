CREATE TABLE IF NOT EXISTS emergency_chat_blocks (
    chat_key TEXT PRIMARY KEY,
    reason TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    source TEXT NOT NULL
);
