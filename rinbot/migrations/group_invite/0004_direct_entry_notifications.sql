ALTER TABLE group_invite_direct_entries ADD COLUMN notify_state TEXT NOT NULL DEFAULT 'not_required'
    CHECK (notify_state IN ('not_required','pending','sending','sent','failed','held_unknown'));
ALTER TABLE group_invite_direct_entries ADD COLUMN notify_attempt_id TEXT NOT NULL DEFAULT '';
ALTER TABLE group_invite_direct_entries ADD COLUMN notify_error TEXT NOT NULL DEFAULT '';
ALTER TABLE group_invite_direct_entries ADD COLUMN notify_message_id TEXT NOT NULL DEFAULT '';
ALTER TABLE group_invite_direct_entries ADD COLUMN notified_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE group_invite_direct_entries ADD COLUMN notify_recipient TEXT NOT NULL DEFAULT '';
ALTER TABLE group_invite_direct_entries ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS idx_group_invite_direct_notifications
    ON group_invite_direct_entries(notify_state, created_at);
