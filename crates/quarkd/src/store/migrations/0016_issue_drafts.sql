-- New issue drafts: the side chat in which the coordinator drafts beads
-- before any exist. `body` is the whole IssueDraft as JSON; nothing in it is
-- queried apart from the state.

CREATE TABLE issue_drafts (
    id         TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    state      TEXT NOT NULL,
    body       TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX issue_drafts_by_project ON issue_drafts (project_id, state, created_at);
