CREATE TABLE IF NOT EXISTS person_memory_consolidation_revisions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    uin TEXT NOT NULL,
    chat_key TEXT NOT NULL,
    revision INTEGER NOT NULL,
    previous_impressions_json TEXT NOT NULL,
    impressions_json TEXT NOT NULL,
    source_message_ids_json TEXT NOT NULL,
    model TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE(uin, chat_key, revision)
);
CREATE INDEX IF NOT EXISTS idx_person_memory_revisions_chat
    ON person_memory_consolidation_revisions(chat_key, uin, revision DESC);
