-- When a finished task was cleared from the board; NULL while it is shown.
-- Its activity log and events are kept.

ALTER TABLE tasks ADD COLUMN cleared_at TEXT;
