-- The model a task's worker runs and the branch its working copy has out.
-- `model` is what the worker was started with (absent for the harness
-- default); `model_seen` what its session log last reported.

ALTER TABLE tasks ADD COLUMN model TEXT;
ALTER TABLE tasks ADD COLUMN model_seen TEXT;
ALTER TABLE tasks ADD COLUMN branch TEXT;
