-- A question asked again after its answer reuses its engine id, so a
-- Project may hold several decisions for one engine id.

CREATE TABLE decisions_v9 (
    id          TEXT PRIMARY KEY,
    project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    engine_id   TEXT NOT NULL,
    task_id     TEXT,
    question    TEXT NOT NULL,
    state       TEXT NOT NULL,
    answer      TEXT,
    answered_by TEXT,
    opened_at   TEXT NOT NULL,
    answered_at TEXT
);
INSERT INTO decisions_v9 (id, project_id, engine_id, task_id, question, state, answer,
                          answered_by, opened_at, answered_at)
    SELECT id, project_id, engine_id, task_id, question, state, answer,
           answered_by, opened_at, answered_at
    FROM decisions;
DROP TABLE decisions;
ALTER TABLE decisions_v9 RENAME TO decisions;
CREATE INDEX decisions_by_engine_id ON decisions (project_id, engine_id);
