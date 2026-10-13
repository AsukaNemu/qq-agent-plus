CREATE TABLE IF NOT EXISTS conversation_wakes (
    conversation_id TEXT PRIMARY KEY,
    first_pending_at INTEGER NOT NULL,
    due_at INTEGER NOT NULL,
    roll_percent REAL NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS inbox_batches (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    lease_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('processing','completed','failed','unknown','held')),
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS inbox_batch_members (
    batch_id TEXT NOT NULL REFERENCES inbox_batches(id),
    inbox_id INTEGER NOT NULL UNIQUE REFERENCES inbox(id),
    PRIMARY KEY (batch_id, inbox_id)
);

CREATE INDEX IF NOT EXISTS inbox_batches_conversation ON inbox_batches(conversation_id, created_at);
