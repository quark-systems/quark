-- Verification gate evidence (ADR-15) per pull request, as API JSON.

ALTER TABLE pull_requests ADD COLUMN evidence TEXT;
