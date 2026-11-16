-- Accounts the person disabled for Quark, and ones waiting to be removed
-- once the work running under them finishes. An account with no row is
-- enabled. Default accounts (`default-<harness>`) have rows here too.

CREATE TABLE account_states (
    account_id TEXT PRIMARY KEY,
    enabled INTEGER NOT NULL DEFAULT 1,
    removing INTEGER NOT NULL DEFAULT 0,
    changed_at TEXT NOT NULL
);
