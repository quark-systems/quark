-- Rate-limit failovers (ADR-11) per task, as API JSON, oldest first.

ALTER TABLE tasks ADD COLUMN failovers TEXT NOT NULL DEFAULT '[]';
