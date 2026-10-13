-- Shared persistence infrastructure only. Feature task rows are written by
-- their owning runtime; a feature that was never enabled creates no task data.
CREATE TABLE IF NOT EXISTS durable_tasks (
    task_type TEXT NOT NULL,
    task_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'processing')),
    created_at INTEGER NOT NULL,
    lease_expires_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (task_type, task_id)
);

CREATE INDEX IF NOT EXISTS durable_tasks_ready
    ON durable_tasks (task_type, state, lease_expires_at, created_at, task_id);
