-- Accounts that sign in with an API key kept in the Keychain (IBM Bob).
-- Such an account's `config_dir` holds `keychain:<service>`, the Keychain
-- item its key is in; the key itself is never stored here. `team_id` is the
-- key's team, when the harness needs one.

ALTER TABLE accounts ADD COLUMN team_id TEXT;
