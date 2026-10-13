ALTER TABLE batch_tool_operations
    ADD COLUMN round_index INTEGER NOT NULL DEFAULT 0 CHECK (round_index >= 0);

CREATE INDEX IF NOT EXISTS batch_tool_operations_round_idx
    ON batch_tool_operations(batch_id, round_index, ordinal);
