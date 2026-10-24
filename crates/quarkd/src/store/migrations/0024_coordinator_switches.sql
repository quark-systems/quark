-- Changes of a Project coordinator's agent, one row per change, each step
-- recorded before its effect (docs/engine/coordinator-switch.md).

CREATE TABLE coordinator_switches (
    id          TEXT PRIMARY KEY,
    project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    from_agent  TEXT,
    to_agent    TEXT NOT NULL,
    kind        TEXT NOT NULL,
    state       TEXT NOT NULL,
    -- The event log position when it was asked for: the brief is the
    -- coordinator's last reply after it.
    after_seq   INTEGER NOT NULL,
    -- When the wrap up's turn ended.
    wrapped_at  TEXT,
    brief       TEXT,
    error       TEXT,
    requested_at TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE INDEX coordinator_switches_project ON coordinator_switches (project_id, requested_at);
