-- When the task took its worktree (epoch milliseconds), where the engine
-- says. Pool slots are reused, so session logs in the directory from before
-- this belong to an earlier task.

ALTER TABLE tasks ADD COLUMN worktree_at_ms INTEGER;
