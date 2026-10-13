CREATE TABLE IF NOT EXISTS sticker_entries (
    id TEXT PRIMARY KEY,
    entry_json TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS sticker_metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS sticker_usage_events (
    operation_id TEXT PRIMARY KEY,
    sticker_id TEXT NOT NULL,
    context TEXT NOT NULL,
    used_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS sticker_usage_events_sticker_idx
    ON sticker_usage_events(sticker_id, used_at DESC);

CREATE INDEX IF NOT EXISTS sticker_entries_updated_idx
    ON sticker_entries(updated_at DESC, id ASC);
