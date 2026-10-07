-- Where each transcript source was last read. The session logs stay the
-- source of truth: this holds offsets, not copies, and moves in the same
-- transaction as the events read from them.

CREATE TABLE transcript_index (
    source     TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    path       TEXT NOT NULL,
    "offset"   INTEGER NOT NULL,
    updated_at TEXT NOT NULL
);
