CREATE TABLE IF NOT EXISTS daily_moment_operator_resolutions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    record_id TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN ('sent', 'missed')),
    note TEXT NOT NULL,
    resolved_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_daily_moment_operator_resolutions_record
    ON daily_moment_operator_resolutions(record_id, id);
