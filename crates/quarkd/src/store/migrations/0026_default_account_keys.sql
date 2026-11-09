-- Default accounts that sign in with an API key Quark keeps in the Keychain
-- (Kiro CLI's or IBM Bob's usual login, for a quarkd started without the
-- person's shell). The key is in the Keychain item `quark/accounts/<id>`;
-- only the fact that there is one is stored here.

CREATE TABLE default_account_keys (
    account_id TEXT PRIMARY KEY,
    created_at TEXT NOT NULL
);
