ALTER TABLE identity_people ADD COLUMN legacy_memory_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE identity_people ADD COLUMN profile_json TEXT NOT NULL DEFAULT '{}';
ALTER TABLE identity_people ADD COLUMN profile_updated_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE identity_people ADD COLUMN created_at INTEGER NOT NULL DEFAULT 0;

CREATE TABLE IF NOT EXISTS legacy_memory_refs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    uin TEXT NOT NULL,
    chat_key TEXT NOT NULL,
    content TEXT NOT NULL,
    observed_at INTEGER NOT NULL DEFAULT 0,
    source_file TEXT NOT NULL DEFAULT '',
    UNIQUE (uin, chat_key, content),
    FOREIGN KEY (uin) REFERENCES identity_people(uin) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS identity_asset_overrides (
    uin TEXT PRIMARY KEY,
    primary_name TEXT NOT NULL DEFAULT '',
    profile_json TEXT NOT NULL DEFAULT '{}',
    is_friend INTEGER,
    source_chat_key TEXT NOT NULL DEFAULT '',
    deleted INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_identity_memories_person
    ON legacy_memory_refs(uin, observed_at DESC);
