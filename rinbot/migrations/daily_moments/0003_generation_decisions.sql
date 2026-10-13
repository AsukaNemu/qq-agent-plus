CREATE TABLE IF NOT EXISTS daily_moment_generation_decisions (
    slot_id TEXT PRIMARY KEY,
    day_key TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN ('draft', 'skipped')),
    decision_json TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_daily_moment_generation_day
    ON daily_moment_generation_decisions(day_key DESC, created_at DESC);
