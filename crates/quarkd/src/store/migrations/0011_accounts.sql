-- Accounts and pools (ADR-11) are daemon configuration, not engine state.
-- A harness's default account (`default-<harness>`) has no `accounts` row
-- but can still be in pools and hold a quota reading. A task records the
-- account it was started under; a Project records its coordinator's.

CREATE TABLE accounts (
    id         TEXT PRIMARY KEY,
    harness    TEXT NOT NULL,
    label      TEXT NOT NULL,
    config_dir TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE (harness, config_dir)
);

CREATE TABLE account_pools (
    account_id TEXT NOT NULL,
    pool       TEXT NOT NULL,
    PRIMARY KEY (account_id, pool)
);

CREATE TABLE account_quota (
    account_id TEXT PRIMARY KEY,
    quota      TEXT NOT NULL
);

ALTER TABLE tasks ADD COLUMN account_id TEXT;
ALTER TABLE projects ADD COLUMN coordinator_account_id TEXT;
