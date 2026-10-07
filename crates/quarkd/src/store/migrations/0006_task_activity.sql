-- Task activity from status logs. `status_cursors` keeps each task's read
-- offset in the same transaction as the entries it covers, so a restart
-- resumes exactly where the last projection stopped.

ALTER TABLE tasks ADD COLUMN worktree_path TEXT;

CREATE TABLE task_events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id      TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    project_id   TEXT NOT NULL,
    kind         TEXT NOT NULL,
    decision_key TEXT,
    note         TEXT NOT NULL,
    raw          TEXT NOT NULL,
    ts           TEXT NOT NULL
);
CREATE INDEX task_events_by_task ON task_events (task_id, id);

CREATE TABLE status_cursors (
    task_id     TEXT PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
    byte_offset INTEGER NOT NULL
);
