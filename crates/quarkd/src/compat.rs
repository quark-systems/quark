//! What this build is compatible with, for the update feed.
//!
//! `quarkd compat --json` prints it; the release workflow saves it as each
//! release's `compat.json`, and `packaging/update-feed.mjs` copies it into the
//! channel manifests, so an updater can tell what an update would change
//! before it downloads one (see docs/development/releasing.md, Updates).

use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Compat {
    /// Release version (Cargo.toml).
    pub version: String,
    /// The `/v1` contract it serves (`quark_systems::API_CONTRACT`).
    pub api_version: u32,
    /// The oldest contract it still serves (`quark_systems::API_CONTRACT_MIN`).
    pub api_min: u32,
    /// The `~/.quark` layout it writes (`format.json`).
    pub home_format: u32,
    /// The oldest home format a quarkd must know to use a home it wrote.
    pub home_min_reader: u32,
    /// The store database schema (`user_version`).
    pub store_schema: i64,
    /// The oldest store schema a quarkd must know to use its store.
    pub store_min_reader: i64,
    /// The event log database schema (`user_version`).
    pub eventlog_schema: i64,
    /// The oldest event log schema a quarkd must know to use its log.
    pub eventlog_min_reader: i64,
}

impl Compat {
    /// Whether a quarkd described by `older` can still use a home this build
    /// migrated, so a rollback to it needs no restore from backup.
    pub fn readable_by(&self, older: &Compat) -> bool {
        self.home_min_reader <= older.home_format
            && self.store_min_reader <= older.store_schema
            && self.eventlog_min_reader <= older.eventlog_schema
    }
}

impl Compat {
    pub fn current() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            api_version: quark_systems::API_CONTRACT,
            api_min: quark_systems::API_CONTRACT_MIN,
            home_format: crate::home_format::CURRENT,
            home_min_reader: crate::home_format::MIN_READER,
            store_schema: crate::store::SCHEMA_VERSION,
            store_min_reader: crate::store::SCHEMA_MIN_READER,
            eventlog_schema: quark_eventlog::SCHEMA_VERSION,
            eventlog_min_reader: quark_eventlog::MIN_READER,
        }
    }
}

impl fmt::Display for Compat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "version          {}", self.version)?;
        writeln!(f, "api_version      {}", self.api_version)?;
        writeln!(f, "api_min          {}", self.api_min)?;
        writeln!(f, "home_format      {}", self.home_format)?;
        writeln!(f, "home_min_reader  {}", self.home_min_reader)?;
        writeln!(f, "store_schema     {}", self.store_schema)?;
        writeln!(f, "store_min_reader {}", self.store_min_reader)?;
        writeln!(f, "eventlog_schema  {}", self.eventlog_schema)?;
        writeln!(f, "eventlog_min_reader {}", self.eventlog_min_reader)
    }
}

#[cfg(test)]
mod tests {
    use super::Compat;

    #[test]
    fn reports_this_build() {
        let c = Compat::current();
        assert_eq!(c.version, env!("CARGO_PKG_VERSION"));
        assert!(c.api_version >= 1 && c.home_format >= 1 && c.store_schema >= 1);
        let json = serde_json::to_value(&c).unwrap();
        for key in [
            "version",
            "api_version",
            "api_min",
            "home_format",
            "store_schema",
            "eventlog_schema",
            "store_min_reader",
        ] {
            assert!(json.get(key).is_some(), "{key}");
        }
        assert!(c.readable_by(&c));
        let mut older = c.clone();
        older.store_schema = c.store_min_reader - 1;
        assert!(!c.readable_by(&older));
    }
}
