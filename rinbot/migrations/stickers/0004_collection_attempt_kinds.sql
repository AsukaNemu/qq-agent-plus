CREATE TABLE IF NOT EXISTS sticker_collection_attempts_v2 (
    source_hash TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('manual', 'automatic', 'legacy')),
    state TEXT NOT NULL CHECK (state IN ('processing', 'collected', 'skipped', 'unknown')),
    attempted_at INTEGER NOT NULL,
    note TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (source_hash, kind)
);

CREATE INDEX IF NOT EXISTS sticker_collection_attempts_v2_kind_time_idx
    ON sticker_collection_attempts_v2(kind, attempted_at DESC);

INSERT OR IGNORE INTO sticker_collection_attempts_v2
    (source_hash, kind, state, attempted_at, note)
SELECT source_hash, 'legacy', state, attempted_at, note
FROM sticker_collection_attempts AS old_attempt
WHERE NOT EXISTS (
    SELECT 1
    FROM sticker_collection_attempts_v2 AS typed_attempt
    WHERE typed_attempt.source_hash = old_attempt.source_hash
      AND typed_attempt.kind IN ('manual', 'automatic')
);
