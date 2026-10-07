-- Why each task got its agent (ADR-11), one row per worker spawn, as API
-- JSON. History kept for scoring: no foreign key, so nothing that removes or
-- cleans up a task removes its records, and nothing prunes them.

CREATE TABLE dispatch_records (
    id          TEXT PRIMARY KEY,
    task_id     TEXT NOT NULL,
    project_id  TEXT NOT NULL,
    generation  TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    record      TEXT NOT NULL,
    UNIQUE (task_id, generation)
);
