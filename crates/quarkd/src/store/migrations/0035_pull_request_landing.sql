-- Whether a merged pull request landed. `default_branch` is the repository's
-- default branch as the forge last reported it: a pull request merged into
-- another branch has not landed until that branch's own pull request merges
-- into it afterwards. NULL (never read) counts as landed, as before.
-- `stranded_told_at` is when the coordinator was told a merge missed the
-- default branch, so it is told once. Older code ignores both.

ALTER TABLE pull_requests ADD COLUMN default_branch TEXT;
ALTER TABLE pull_requests ADD COLUMN stranded_told_at TEXT;
