ALTER TABLE legacy_memory_refs ADD COLUMN created_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE legacy_memory_refs ADD COLUMN origin TEXT NOT NULL DEFAULT '';
