-- The PR center: pull requests opened by tasks, with the checks and reviews
-- last read from the forge, and each Project's standing approval.

ALTER TABLE projects ADD COLUMN standing_approval INTEGER NOT NULL DEFAULT 0;

CREATE TABLE pull_requests (
    id              TEXT PRIMARY KEY,
    project_id      TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    task_id         TEXT REFERENCES tasks(id) ON DELETE SET NULL,
    url             TEXT NOT NULL,
    provider        TEXT NOT NULL,
    repo            TEXT NOT NULL,
    number          INTEGER NOT NULL,
    title           TEXT,
    author          TEXT,
    state           TEXT NOT NULL,
    head_ref        TEXT,
    base_ref        TEXT,
    head_sha        TEXT,
    mergeable       TEXT NOT NULL,
    checks_state    TEXT NOT NULL,
    review_decision TEXT NOT NULL,
    additions       INTEGER,
    deletions       INTEGER,
    changed_files   INTEGER,
    opened_at       TEXT,
    updated_at      TEXT,
    merged_at       TEXT,
    closed_at       TEXT,
    synced_at       TEXT,
    sync_error      TEXT,
    created_at      TEXT NOT NULL,
    UNIQUE (project_id, url)
);

CREATE TABLE checks (
    pull_request_id TEXT NOT NULL REFERENCES pull_requests(id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    status          TEXT NOT NULL,
    conclusion      TEXT,
    details_url     TEXT,
    started_at      TEXT,
    completed_at    TEXT,
    PRIMARY KEY (pull_request_id, name)
);

CREATE TABLE reviews (
    pull_request_id TEXT NOT NULL REFERENCES pull_requests(id) ON DELETE CASCADE,
    id              TEXT NOT NULL,
    author          TEXT,
    state           TEXT NOT NULL,
    body            TEXT NOT NULL,
    submitted_at    TEXT,
    commit_sha      TEXT,
    PRIMARY KEY (pull_request_id, id)
);
