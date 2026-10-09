-- Decisions mirrored as `decision` beads in the Project's Beads database,
-- and standing rules whose words can change.

ALTER TABLE decisions ADD COLUMN bead_id TEXT;
ALTER TABLE standing_rules ADD COLUMN changed_at TEXT;
ALTER TABLE standing_rules ADD COLUMN changed_by TEXT;
