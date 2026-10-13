ALTER TABLE group_invite_requests ADD COLUMN notify_required INTEGER NOT NULL DEFAULT 0
    CHECK (notify_required IN (0,1));
-- Preserve terminal/attempted notifications, but do not turn historical pending
-- rows into future sends when the owner explicitly had notifications disabled.
UPDATE group_invite_requests SET notify_required=1
WHERE notify_state IN ('sending','sent','failed','held_unknown');
