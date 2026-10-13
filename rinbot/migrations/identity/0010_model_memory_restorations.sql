CREATE TABLE IF NOT EXISTS identity_model_memory_restorations (
    removal_operation_id TEXT PRIMARY KEY,
    restore_operation_id TEXT NOT NULL UNIQUE,
    restored_at INTEGER NOT NULL,
    restored_count INTEGER NOT NULL
);
