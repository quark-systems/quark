//! App-wide preferences, in one row. A database with no row reads as the
//! defaults, so an upgrade needs nothing written.

use quark_systems::{Preferences, UpdatePreferences};
use rusqlite::{params, OptionalExtension};

use super::{Result, Store};
use crate::now_rfc3339;

impl Store {
    /// The preferences, or their defaults when none are set.
    pub fn preferences(&self) -> Result<Preferences> {
        self.read(|c| {
            Ok(c.query_row(
                "SELECT group_by_collection FROM preferences WHERE id = 1",
                [],
                |r| Ok(Preferences { group_by_collection: r.get(0)? }),
            )
            .optional()?
            .unwrap_or_default())
        })
    }

    /// Changes the preferences named, leaving the rest as they are.
    pub fn set_preferences(&self, input: UpdatePreferences) -> Result<Preferences> {
        self.write(|tx, _events| {
            let mut prefs = tx
                .query_row(
                    "SELECT group_by_collection FROM preferences WHERE id = 1",
                    [],
                    |r| Ok(Preferences { group_by_collection: r.get(0)? }),
                )
                .optional()?
                .unwrap_or_default();
            if let Some(on) = input.group_by_collection {
                prefs.group_by_collection = on;
            }
            tx.execute(
                "INSERT INTO preferences (id, group_by_collection, updated_at)
                 VALUES (1, ?1, ?2)
                 ON CONFLICT(id) DO UPDATE SET group_by_collection = ?1, updated_at = ?2",
                params![prefs.group_by_collection, now_rfc3339()],
            )?;
            Ok(prefs)
        })
    }
}
