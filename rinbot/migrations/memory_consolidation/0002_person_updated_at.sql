CREATE TABLE IF NOT EXISTS memory_consolidation_person_updated_at (
    user_id TEXT PRIMARY KEY,
    updated_at INTEGER NOT NULL CHECK (updated_at >= 0)
);
