-- Messages sent to a Project's coordinator, recorded by the daemon when they
-- are sent: queued while the coordinator works (editable, cancellable), then
-- typed into its session. The transcript records each one where it was sent;
-- the harness's own copy of it is matched here and not shown twice.

CREATE TABLE coordinator_outbox (
    id          TEXT PRIMARY KEY,
    project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    text        TEXT NOT NULL,
    -- queued | sending | sent | unconfirmed | failed | cancelled
    state       TEXT NOT NULL,
    error       TEXT,
    -- The seq of the transcript entry recording it, once typed in.
    entry_seq   INTEGER,
    -- 1 once the harness's own copy was read from its session log.
    seen        INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE INDEX coordinator_outbox_project ON coordinator_outbox (project_id, state, created_at);
