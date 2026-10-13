CREATE TABLE IF NOT EXISTS memory_consolidation_chat_runs (
    chat_key TEXT PRIMARY KEY,
    last_attempt_at INTEGER NOT NULL,
    last_finished_at INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL CHECK (status IN ('running','completed','failed','interrupted'))
);

CREATE TABLE IF NOT EXISTS memory_consolidation_person_cooldowns (
    user_id TEXT PRIMARY KEY,
    last_consolidated_at INTEGER NOT NULL CHECK (last_consolidated_at >= 0)
);
