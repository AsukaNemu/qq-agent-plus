CREATE TABLE IF NOT EXISTS daily_moment_records (
    id TEXT PRIMARY KEY,
    day_key TEXT NOT NULL,
    source TEXT NOT NULL,
    status TEXT NOT NULL,
    publish_attempted INTEGER NOT NULL CHECK (publish_attempted IN (0,1)),
    record_json TEXT NOT NULL,
    imported_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_daily_moment_records_day
    ON daily_moment_records(day_key DESC, imported_at DESC);

CREATE TABLE IF NOT EXISTS daily_moment_schedule_slots (
    id TEXT PRIMARY KEY,
    day_key TEXT NOT NULL,
    status TEXT NOT NULL,
    slot_json TEXT NOT NULL,
    imported_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS daily_moments_schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at INTEGER NOT NULL
);
