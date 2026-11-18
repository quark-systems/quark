-- Collections: named sets of Projects for the left list. A Project may be in
-- several, or none; membership decides nothing about how a Project runs.

CREATE TABLE IF NOT EXISTS collections (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    position   INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS collection_projects (
    collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    project_id    TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    position      INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (collection_id, project_id)
);

CREATE INDEX IF NOT EXISTS collection_projects_project ON collection_projects(project_id);
