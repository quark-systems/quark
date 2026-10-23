-- When Quark compacts a Project coordinator's conversation. A Project with
-- no row uses the daemon's defaults.

CREATE TABLE coordinator_compaction (
    project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    auto       INTEGER NOT NULL,
    at_tokens  INTEGER NOT NULL,
    updated_at TEXT NOT NULL
);
