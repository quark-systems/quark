-- App-wide preferences: how the app shows what is there, kept on the daemon
-- so every client it serves opens the same way. One row, id 1; no row means
-- the defaults.

CREATE TABLE IF NOT EXISTS preferences (
    id                  INTEGER PRIMARY KEY CHECK (id = 1),
    group_by_collection INTEGER NOT NULL DEFAULT 1,
    updated_at          TEXT NOT NULL
);
