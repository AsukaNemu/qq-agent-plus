ALTER TABLE group_invite_requests ADD COLUMN whitelist_state TEXT NOT NULL DEFAULT 'not_required'
    CHECK (whitelist_state IN ('not_required','pending','applied','failed'));
ALTER TABLE group_invite_requests ADD COLUMN whitelist_error TEXT NOT NULL DEFAULT '';
ALTER TABLE group_invite_direct_entries ADD COLUMN whitelist_state TEXT NOT NULL DEFAULT 'not_required'
    CHECK (whitelist_state IN ('not_required','pending','applied','failed'));
ALTER TABLE group_invite_direct_entries ADD COLUMN whitelist_error TEXT NOT NULL DEFAULT '';
