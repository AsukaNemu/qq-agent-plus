CREATE TABLE IF NOT EXISTS proactive_schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS proactive_wakes (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('followUp','selfWake','paced')),
    at INTEGER NOT NULL,
    note TEXT NOT NULL DEFAULT '',
    paced INTEGER NOT NULL DEFAULT 0 CHECK (paced IN (0,1)),
    state TEXT NOT NULL CHECK (state IN (
        'pending','claimed','dispatching','sent','failed','unknown','held','cancelled'
    )),
    attempt_id TEXT NOT NULL DEFAULT '',
    outbox_id TEXT NOT NULL DEFAULT '',
    error TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_proactive_wakes_due
    ON proactive_wakes(state, at, id);
CREATE INDEX IF NOT EXISTS idx_proactive_wakes_history
    ON proactive_wakes(created_at DESC, id DESC);
CREATE UNIQUE INDEX IF NOT EXISTS idx_proactive_wakes_outbox
    ON proactive_wakes(outbox_id) WHERE outbox_id <> '';
CREATE UNIQUE INDEX IF NOT EXISTS idx_proactive_one_pending_per_chat
    ON proactive_wakes(conversation_id) WHERE state='pending';
