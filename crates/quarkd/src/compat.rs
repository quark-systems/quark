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
    /// The `~/.quark` layout it writes (`format.json`).
    pub home_format: u32,
    /// The store database schema (`user_version`).
    pub store_schema: i64,
    /// The event log database schema (`user_version`).
    pub eventlog_schema: i64,
}

impl Compat {
    pub fn current() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            api_version: quark_systems::API_CONTRACT,
            home_format: crate::home_format::CURRENT,
            store_schema: crate::store::SCHEMA_VERSION,
            eventlog_schema: quark_eventlog::SCHEMA_VERSION,
        }
    }
}

impl fmt::Display for Compat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "version          {}", self.version)?;
        writeln!(f, "api_version      {}", self.api_version)?;
        writeln!(f, "home_format      {}", self.home_format)?;
        writeln!(f, "store_schema     {}", self.store_schema)?;
        writeln!(f, "eventlog_schema  {}", self.eventlog_schema)
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
            "home_format",
            "store_schema",
            "eventlog_schema",
        ] {
            assert!(json.get(key).is_some(), "{key}");
        }
    }
}
