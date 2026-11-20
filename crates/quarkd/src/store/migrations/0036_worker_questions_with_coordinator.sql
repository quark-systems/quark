-- A worker's question goes to the coordinator first (state
-- 'with_coordinator') and reaches a person only once the coordinator frames
-- it with options. Raw worker questions already open go back to the
-- coordinator, which frames, answers or withdraws them. Older code reads
-- the new state as open, as before.

UPDATE decisions SET state = 'with_coordinator'
WHERE state = 'open'
  AND task_id IS NOT NULL
  AND json_extract(brief, '$.asked_by') = task_id
  AND json_extract(brief, '$.options') IS NULL
  AND engine_id NOT LIKE 'quarkd/%';
