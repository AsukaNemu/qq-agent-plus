ALTER TABLE relationship_evaluations
    ADD COLUMN session_json TEXT NOT NULL DEFAULT '{}';
