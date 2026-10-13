CREATE TABLE IF NOT EXISTS identity_model_memories (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    uin TEXT NOT NULL,
    target TEXT NOT NULL DEFAULT '',
    content TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    last_observed_at INTEGER NOT NULL,
    origin TEXT NOT NULL DEFAULT 'model',
    deleted_at INTEGER NOT NULL DEFAULT 0,
    UNIQUE(uin, content),
    FOREIGN KEY (uin) REFERENCES identity_people(uin) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS identity_model_memory_sources (
    memory_id INTEGER NOT NULL,
    chat_key TEXT NOT NULL,
    active INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY(memory_id, chat_key),
    FOREIGN KEY (memory_id) REFERENCES identity_model_memories(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_identity_model_memory_sources_chat
    ON identity_model_memory_sources(chat_key, active, memory_id);

CREATE TABLE IF NOT EXISTS identity_legacy_memory_tombstones (
    memory_ref_id INTEGER NOT NULL,
    removed_at INTEGER NOT NULL,
    operation_id TEXT NOT NULL,
    PRIMARY KEY(memory_ref_id, operation_id),
    FOREIGN KEY (memory_ref_id) REFERENCES legacy_memory_refs(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS identity_memory_impression_tombstones (
    uin TEXT NOT NULL,
    chat_key TEXT NOT NULL,
    content TEXT NOT NULL,
    removed_at INTEGER NOT NULL,
    operation_id TEXT NOT NULL,
    PRIMARY KEY(uin, chat_key, content, operation_id)
);

CREATE TABLE IF NOT EXISTS identity_model_memory_mutations (
    operation_id TEXT PRIMARY KEY,
    uin TEXT NOT NULL DEFAULT '',
    chat_key TEXT NOT NULL,
    action TEXT NOT NULL,
    before_json TEXT NOT NULL,
    after_json TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
