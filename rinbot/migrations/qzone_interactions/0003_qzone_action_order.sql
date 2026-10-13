ALTER TABLE qzone_interaction_actions
    ADD COLUMN queue_sequence INTEGER NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_qzone_interaction_actions_queue
    ON qzone_interaction_actions(state, queue_sequence, created_at);
