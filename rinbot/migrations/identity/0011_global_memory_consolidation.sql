CREATE TABLE IF NOT EXISTS person_memory_global_consolidation_revisions (
    uin TEXT NOT NULL,
    revision INTEGER NOT NULL,
    previous_impressions_json TEXT NOT NULL,
    impressions_json TEXT NOT NULL,
    source_message_ids_json TEXT NOT NULL,
    model TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY(uin, revision)
);
CREATE INDEX IF NOT EXISTS idx_person_memory_global_revisions_created
    ON person_memory_global_consolidation_revisions(uin, created_at DESC, revision DESC);
