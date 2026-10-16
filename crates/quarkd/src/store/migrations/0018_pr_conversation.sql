-- A pull request's description and its comments as last read from the
-- forge: comments on the conversation, and inline comments on the diff.

ALTER TABLE pull_requests ADD COLUMN body TEXT;

CREATE TABLE pr_comments (
    pull_request_id TEXT NOT NULL REFERENCES pull_requests(id) ON DELETE CASCADE,
    id              TEXT NOT NULL,
    kind            TEXT NOT NULL,
    author          TEXT,
    body            TEXT NOT NULL,
    created_at      TEXT,
    updated_at      TEXT,
    url             TEXT,
    review_id       TEXT,
    path            TEXT,
    line            INTEGER,
    original_line   INTEGER,
    side            TEXT,
    commit_sha      TEXT,
    in_reply_to     TEXT,
    PRIMARY KEY (pull_request_id, id)
);
