CREATE TABLE IF NOT EXISTS person_memory_node_consolidation_backups (
    uin TEXT NOT NULL,
    backup_key TEXT NOT NULL,
    backed_up_at INTEGER NOT NULL,
    reason TEXT NOT NULL,
    source_chat_key TEXT NOT NULL DEFAULT '',
    person_json TEXT NOT NULL,
    imported_at INTEGER NOT NULL,
    PRIMARY KEY (uin, backup_key),
    FOREIGN KEY (uin) REFERENCES identity_people(uin) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_node_consolidation_backups_person_time
    ON person_memory_node_consolidation_backups(uin, backed_up_at DESC, backup_key);
