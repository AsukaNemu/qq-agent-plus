CREATE TABLE IF NOT EXISTS batch_tool_operations (
    batch_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    tool_call_id TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    arguments_json TEXT NOT NULL,
    assistant_content TEXT NOT NULL,
    outbox_id TEXT,
    state TEXT NOT NULL CHECK (state IN ('pending','completed','failed','unknown')),
    result_json TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (batch_id, ordinal),
    UNIQUE (outbox_id)
);

CREATE INDEX IF NOT EXISTS batch_tool_operations_state_idx
    ON batch_tool_operations(batch_id, state, ordinal);
