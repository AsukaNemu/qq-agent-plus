ALTER TABLE proactive_wakes
    ADD COLUMN operation_id TEXT NOT NULL DEFAULT '';

ALTER TABLE proactive_wakes
    ADD COLUMN operation_args_json TEXT NOT NULL DEFAULT '';

CREATE UNIQUE INDEX idx_proactive_wakes_operation
    ON proactive_wakes(operation_id) WHERE operation_id <> '';
