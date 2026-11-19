-- Pull requests the Project's coordinator opened from its own shell, which no
-- task reports. `opened_by` is 'coordinator' for those and NULL for every
-- other row; `merge_told_at` is when the coordinator was told one merged, so
-- it is told once. Older code ignores both.

ALTER TABLE pull_requests ADD COLUMN opened_by TEXT;
ALTER TABLE pull_requests ADD COLUMN merge_told_at TEXT;
