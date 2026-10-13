CREATE TABLE IF NOT EXISTS identity_observed_messages (
    chat_key TEXT NOT NULL,
    message_id TEXT NOT NULL,
    observed_at INTEGER NOT NULL,
    PRIMARY KEY (chat_key, message_id)
);

CREATE TABLE IF NOT EXISTS identity_people (
    uin TEXT PRIMARY KEY,
    primary_name TEXT NOT NULL DEFAULT '',
    first_seen_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    message_count INTEGER NOT NULL DEFAULT 0,
    chat_count INTEGER NOT NULL DEFAULT 0,
    is_friend INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS identity_sources (
    uin TEXT NOT NULL,
    chat_key TEXT NOT NULL,
    message_count INTEGER NOT NULL DEFAULT 0,
    first_seen_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    PRIMARY KEY (uin, chat_key),
    FOREIGN KEY (uin) REFERENCES identity_people(uin) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS identity_aliases (
    uin TEXT NOT NULL,
    chat_key TEXT NOT NULL,
    alias TEXT NOT NULL,
    seen_count INTEGER NOT NULL DEFAULT 0,
    first_seen_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    PRIMARY KEY (uin, chat_key, alias),
    FOREIGN KEY (uin) REFERENCES identity_people(uin) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_identity_people_recent
    ON identity_people(last_seen_at DESC, message_count DESC);

CREATE INDEX IF NOT EXISTS idx_identity_aliases_person
    ON identity_aliases(uin, last_seen_at DESC);
