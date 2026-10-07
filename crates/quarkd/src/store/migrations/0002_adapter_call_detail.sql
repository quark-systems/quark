-- Engine script detail on adapter-call records. Rows written by the
-- projector per operation leave these empty.

ALTER TABLE adapter_calls ADD COLUMN kind TEXT;
ALTER TABLE adapter_calls ADD COLUMN script TEXT;
ALTER TABLE adapter_calls ADD COLUMN args TEXT;
ALTER TABLE adapter_calls ADD COLUMN exit_code INTEGER;
ALTER TABLE adapter_calls ADD COLUMN workspace TEXT;
