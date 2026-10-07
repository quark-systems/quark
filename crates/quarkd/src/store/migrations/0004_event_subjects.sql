-- Terminal output is the one high-volume event. `subject` names the terminal
-- a `worker.output` event belongs to and `size` its byte count, so old
-- output can be pruned per terminal.

ALTER TABLE events ADD COLUMN subject TEXT;
ALTER TABLE events ADD COLUMN size INTEGER;
CREATE INDEX events_subject ON events (subject, seq) WHERE subject IS NOT NULL;
