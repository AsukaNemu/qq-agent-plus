CREATE TABLE IF NOT EXISTS outbox_text_parts (
    outbox_id TEXT NOT NULL REFERENCES outbox(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    text TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending','sending','sent','failed','unknown')),
    message_id TEXT,
    error TEXT,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (outbox_id, ordinal)
);
