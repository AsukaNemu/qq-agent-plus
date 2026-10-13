CREATE TABLE IF NOT EXISTS qzone_interaction_items (
    kind TEXT NOT NULL CHECK (kind IN ('feed','comment','watched-post','run')),
    item_key TEXT NOT NULL,
    status TEXT NOT NULL,
    item_json TEXT NOT NULL,
    imported_at INTEGER NOT NULL,
    PRIMARY KEY (kind, item_key)
);

CREATE INDEX IF NOT EXISTS idx_qzone_interaction_items_kind_status
    ON qzone_interaction_items(kind, status, imported_at DESC);

CREATE TABLE IF NOT EXISTS qzone_interaction_metadata (
    key TEXT PRIMARY KEY,
    value_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS qzone_interactions_schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at INTEGER NOT NULL
);
