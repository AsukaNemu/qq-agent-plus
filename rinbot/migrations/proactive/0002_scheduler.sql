DROP INDEX IF EXISTS idx_proactive_wakes_due;
DROP INDEX IF EXISTS idx_proactive_wakes_history;
DROP INDEX IF EXISTS idx_proactive_wakes_outbox;
DROP INDEX IF EXISTS idx_proactive_one_pending_per_chat;

ALTER TABLE proactive_wakes RENAME TO proactive_wakes_v1;
CREATE TABLE proactive_wakes (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('proactive','followUp','selfWake','paced')),
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
INSERT INTO proactive_wakes SELECT * FROM proactive_wakes_v1;
DROP TABLE proactive_wakes_v1;

CREATE INDEX idx_proactive_wakes_due ON proactive_wakes(state, at, id);
CREATE INDEX idx_proactive_wakes_history ON proactive_wakes(created_at DESC, id DESC);
CREATE UNIQUE INDEX idx_proactive_wakes_outbox ON proactive_wakes(outbox_id) WHERE outbox_id <> '';
CREATE UNIQUE INDEX idx_proactive_one_pending_per_chat ON proactive_wakes(conversation_id) WHERE state='pending';

CREATE TABLE IF NOT EXISTS proactive_runtime_state (
    key TEXT PRIMARY KEY,
    value INTEGER NOT NULL
);
