CREATE TABLE IF NOT EXISTS identity_person_merges (
    source_uin TEXT PRIMARY KEY,
    target_uin TEXT NOT NULL,
    merged_at INTEGER NOT NULL,
    FOREIGN KEY (source_uin) REFERENCES identity_people(uin),
    FOREIGN KEY (target_uin) REFERENCES identity_people(uin),
    CHECK (source_uin != target_uin)
);

CREATE INDEX IF NOT EXISTS idx_identity_person_merges_target
    ON identity_person_merges(target_uin);
