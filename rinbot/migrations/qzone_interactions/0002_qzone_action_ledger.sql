CREATE TABLE IF NOT EXISTS qzone_interaction_actions (
    action_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    item_kind TEXT NOT NULL CHECK (item_kind IN ('feed','comment')),
    item_key TEXT NOT NULL,
    action TEXT NOT NULL CHECK (action IN ('like','comment','reply')),
    state TEXT NOT NULL CHECK (state IN ('pending','acting','done','unknown','cancelled')),
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count BETWEEN 0 AND 1),
    payload_json TEXT NOT NULL,
    result_json TEXT NOT NULL DEFAULT '{}',
    error TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE (run_id, item_kind, item_key, action)
);

CREATE INDEX IF NOT EXISTS idx_qzone_interaction_actions_state
    ON qzone_interaction_actions(state, created_at, action_id);

CREATE TABLE IF NOT EXISTS qzone_interaction_action_audit (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    action_id TEXT NOT NULL,
    from_state TEXT NOT NULL,
    to_state TEXT NOT NULL,
    detail_json TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER NOT NULL,
    FOREIGN KEY(action_id) REFERENCES qzone_interaction_actions(action_id)
);
