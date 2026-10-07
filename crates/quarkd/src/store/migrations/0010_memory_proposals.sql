-- Memory proposals (journey J8): learnings from finished tasks awaiting
-- review. Accepted entries live in the Project repo; `entry` keeps what was
-- committed. A proposal from a status line names it, so a line is proposed
-- once.

CREATE TABLE memory_proposals (
    id            TEXT PRIMARY KEY,
    project_id    TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    task_id       TEXT REFERENCES tasks(id) ON DELETE SET NULL,
    task_event_id INTEGER UNIQUE REFERENCES task_events(id) ON DELETE SET NULL,
    text          TEXT NOT NULL,
    evidence      TEXT NOT NULL,
    source        TEXT NOT NULL,
    state         TEXT NOT NULL,
    proposed_at   TEXT NOT NULL,
    decided_at    TEXT,
    decided_by    TEXT,
    entry         TEXT
);
CREATE INDEX memory_proposals_by_project ON memory_proposals (project_id, proposed_at);
