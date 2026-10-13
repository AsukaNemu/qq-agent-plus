ALTER TABLE relationship_evaluations
    ADD COLUMN request_json TEXT NOT NULL DEFAULT '{}';
ALTER TABLE relationship_evaluations
    ADD COLUMN response_json TEXT NOT NULL DEFAULT '{}';
