CREATE TABLE IF NOT EXISTS sticker_collection_attempts (
    source_hash TEXT PRIMARY KEY,
    state TEXT NOT NULL CHECK (state IN ('processing', 'collected', 'skipped', 'unknown')),
    attempted_at INTEGER NOT NULL,
    note TEXT NOT NULL DEFAULT ''
);

CREATE INDEX IF NOT EXISTS sticker_collection_attempts_time_idx
    ON sticker_collection_attempts(attempted_at DESC);
