CREATE TABLE projects (
    id             TEXT PRIMARY KEY,
    name           TEXT NOT NULL,
    goal           TEXT,
    workspace_path TEXT,
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL
);

CREATE TABLE tasks (
    id               TEXT PRIMARY KEY,
    project_id       TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    engine_id        TEXT NOT NULL,
    title            TEXT NOT NULL,
    kind             TEXT,
    state            TEXT NOT NULL,
    state_note       TEXT,
    harness          TEXT,
    pull_request_url TEXT,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    UNIQUE (project_id, engine_id)
);

CREATE TABLE decisions (
    id          TEXT PRIMARY KEY,
    project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    engine_id   TEXT NOT NULL,
    task_id     TEXT,
    question    TEXT NOT NULL,
    state       TEXT NOT NULL,
    answer      TEXT,
    answered_by TEXT,
    opened_at   TEXT NOT NULL,
    answered_at TEXT,
    UNIQUE (project_id, engine_id)
);

CREATE TABLE events (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT,
    type       TEXT NOT NULL,
    ts         TEXT NOT NULL,
    payload    TEXT NOT NULL
);

CREATE TABLE adapter_calls (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    ts          TEXT NOT NULL,
    project_id  TEXT,
    operation   TEXT NOT NULL,
    ok          INTEGER NOT NULL,
    duration_ms INTEGER NOT NULL,
    detail      TEXT
);
