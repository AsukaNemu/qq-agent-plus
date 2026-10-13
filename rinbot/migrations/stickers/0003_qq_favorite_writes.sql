CREATE TABLE IF NOT EXISTS sticker_qq_favorite_writes (
    source_hash TEXT PRIMARY KEY,
    state TEXT NOT NULL CHECK (state IN ('attempting', 'confirmed', 'failed', 'unknown')),
    attempted_at INTEGER NOT NULL,
    external_id TEXT NOT NULL DEFAULT '',
    note TEXT NOT NULL DEFAULT ''
);

CREATE INDEX IF NOT EXISTS sticker_qq_favorite_writes_time_idx
    ON sticker_qq_favorite_writes(attempted_at DESC);
