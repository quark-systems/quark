-- When a Project was archived; NULL while it is active.

ALTER TABLE projects ADD COLUMN archived_at TEXT;
