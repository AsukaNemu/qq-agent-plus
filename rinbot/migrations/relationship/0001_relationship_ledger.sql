CREATE TABLE IF NOT EXISTS relationship_states (
    uin TEXT PRIMARY KEY,
    familiarity REAL NOT NULL DEFAULT 0,
    affinity REAL NOT NULL DEFAULT 0,
    friction REAL NOT NULL DEFAULT 0,
    friction_updated_at INTEGER NOT NULL DEFAULT 0,
    message_count INTEGER NOT NULL DEFAULT 0,
    active_days INTEGER NOT NULL DEFAULT 0,
    direct_interactions INTEGER NOT NULL DEFAULT 0,
    chat_count INTEGER NOT NULL DEFAULT 0,
    last_interaction_at INTEGER NOT NULL DEFAULT 0,
    last_evaluated_at INTEGER NOT NULL DEFAULT 0,
    evaluator_version TEXT NOT NULL DEFAULT '',
    reducer_version TEXT NOT NULL DEFAULT '',
    policy_version TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS relationship_events (
    id TEXT PRIMARY KEY,
    uin TEXT NOT NULL,
    event_type TEXT NOT NULL,
    polarity INTEGER NOT NULL,
    strength REAL NOT NULL,
    confidence REAL NOT NULL,
    summary TEXT NOT NULL DEFAULT '',
    evidence_ids TEXT NOT NULL DEFAULT '[]',
    source_chat_keys TEXT NOT NULL DEFAULT '[]',
    dedupe_key TEXT NOT NULL UNIQUE,
    evaluator_version TEXT NOT NULL,
    delta_affinity REAL NOT NULL DEFAULT 0,
    delta_friction REAL NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    applied_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS relationship_events_person ON relationship_events(uin,applied_at DESC);

CREATE TABLE IF NOT EXISTS relationship_flags (
    id TEXT PRIMARY KEY,
    uin TEXT NOT NULL,
    type TEXT NOT NULL,
    status TEXT NOT NULL,
    source_event_id TEXT NOT NULL DEFAULT '',
    resolved_by_event_id TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    resolved_at INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS relationship_flags_person ON relationship_flags(uin,status,created_at DESC);
CREATE TRIGGER IF NOT EXISTS relationship_boundary_flag_singleton
BEFORE INSERT ON relationship_flags
WHEN NEW.status='open'
  AND NEW.type='boundary_violation'
  AND EXISTS (
      SELECT 1 FROM relationship_flags
      WHERE uin=NEW.uin AND type=NEW.type AND status='open'
  )
BEGIN
    SELECT RAISE(IGNORE);
END;

CREATE TABLE IF NOT EXISTS relationship_cursors (
    uin TEXT NOT NULL,
    chat_key TEXT NOT NULL,
    last_message_id INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (uin,chat_key)
);

CREATE TABLE IF NOT EXISTS relationship_evaluations (
    id TEXT PRIMARY KEY,
    uin TEXT NOT NULL,
    status TEXT NOT NULL,
    evidence_count INTEGER NOT NULL DEFAULT 0,
    source_chat_keys TEXT NOT NULL DEFAULT '[]',
    model TEXT NOT NULL DEFAULT '',
    usage_json TEXT NOT NULL DEFAULT '{}',
    error TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    completed_at INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS relationship_evaluations_person ON relationship_evaluations(uin,created_at DESC);

CREATE TABLE IF NOT EXISTS relationship_imports (
    source TEXT PRIMARY KEY,
    imported_states INTEGER NOT NULL,
    imported_events INTEGER NOT NULL,
    imported_flags INTEGER NOT NULL,
    imported_cursors INTEGER NOT NULL,
    imported_evaluations INTEGER NOT NULL,
    imported_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS relationship_schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at INTEGER NOT NULL
);
