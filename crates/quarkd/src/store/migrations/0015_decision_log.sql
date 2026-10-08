-- Decisions as a project log: each gets a per-Project number (D-n), the
-- asker's brief (context, options, recommendation, what it blocks, as
-- JSON), the answerer's reason and channel, what the asker did with the
-- answer, and links to standing rules. Existing decisions are numbered in
-- the order they opened.

ALTER TABLE decisions ADD COLUMN number INTEGER NOT NULL DEFAULT 0;
ALTER TABLE decisions ADD COLUMN brief TEXT NOT NULL DEFAULT '{}';
ALTER TABLE decisions ADD COLUMN answered_via TEXT;
ALTER TABLE decisions ADD COLUMN answer_why TEXT;
ALTER TABLE decisions ADD COLUMN outcome TEXT;
ALTER TABLE decisions ADD COLUMN acted_at TEXT;
ALTER TABLE decisions ADD COLUMN rule_id TEXT;
ALTER TABLE decisions ADD COLUMN made_rule_id TEXT;

UPDATE decisions SET number = (
    SELECT COUNT(*) FROM decisions d
    WHERE d.project_id = decisions.project_id
      AND (d.opened_at < decisions.opened_at
           OR (d.opened_at = decisions.opened_at AND d.rowid <= decisions.rowid))
);

CREATE TABLE standing_rules (
    id           TEXT PRIMARY KEY,
    project_id   TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    text         TEXT NOT NULL,
    decision_id  TEXT,
    created_by   TEXT,
    created_at   TEXT NOT NULL,
    revoked_at   TEXT,
    revoked_by   TEXT
);
CREATE INDEX standing_rules_by_project ON standing_rules (project_id);
