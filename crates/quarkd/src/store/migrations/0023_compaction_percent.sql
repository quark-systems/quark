-- The compaction threshold is a share of the context window, in percent.
-- `at_tokens` is no longer read.

ALTER TABLE coordinator_compaction ADD COLUMN at_percent INTEGER NOT NULL DEFAULT 70;
