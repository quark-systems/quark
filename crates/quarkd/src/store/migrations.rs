//! Schema migrations, one SQL file each under `migrations/`.
//!
//! A migration's version is its position in [`MIGRATIONS`] (first is 1), and
//! its file is named `NNNN_name.sql` with that version. Add a migration by
//! adding a file and one line at the end of the list. If another branch took
//! the same number first, renumber yours at rebase: rename the file and its
//! line. Never edit a migration that has shipped.
//!
//! # Older quarkds on a newer database
//!
//! After an update rolls back, an older quarkd can meet a database a newer one
//! migrated. A migration marked `additive` only adds what older code ignores
//! (a table, a column with a default, an index), so an older quarkd may keep
//! using a database that has it. [`MIN_READER`] is the schema version of the
//! last migration that isn't additive, and is stored in the `schema_compat`
//! table: a quarkd whose [`SCHEMA_VERSION`] is at least that opens the newer
//! database as it is, without migrating it down. Anything else that renames,
//! drops or changes the meaning of data is not additive.

use rusqlite::Connection;

use super::{Result, StoreError};

pub(super) struct Migration {
    pub name: &'static str,
    pub sql: &'static str,
    /// Older code keeps working with it applied (see the module docs).
    pub additive: bool,
}

macro_rules! migration {
    ($name:literal) => {
        Migration {
            name: $name,
            sql: include_str!(concat!("migrations/", $name, ".sql")),
            additive: false,
        }
    };
    ($name:literal, additive) => {
        Migration {
            name: $name,
            sql: include_str!(concat!("migrations/", $name, ".sql")),
            additive: true,
        }
    };
}

pub(super) const MIGRATIONS: &[Migration] = &[
    migration!("0001_initial"),
    migration!("0002_adapter_call_detail"),
    migration!("0003_project_creation"),
    migration!("0004_event_subjects"),
    migration!("0005_transcript_index"),
    migration!("0006_task_activity"),
    migration!("0007_pr_center"),
    migration!("0008_gate_evidence"),
    migration!("0009_repeat_decisions"),
    migration!("0010_memory_proposals"),
    migration!("0011_accounts"),
    migration!("0012_dispatch_records"),
    migration!("0013_task_failovers"),
    migration!("0014_task_agent"),
    migration!("0015_decision_log"),
    migration!("0016_issue_drafts"),
    migration!("0017_decision_beads"),
    migration!("0018_pr_conversation"),
    migration!("0019_project_archive"),
    migration!("0020_account_keys"),
    migration!("0021_task_cleared"),
    migration!("0022_coordinator_compaction"),
    migration!("0023_compaction_percent"),
    migration!("0024_coordinator_switches"),
    migration!("0025_task_worktree_at"),
    migration!("0026_default_account_keys"),
];

/// The schema version a fully migrated database has.
pub(super) const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// The oldest schema a quarkd must know to use a database at
/// [`SCHEMA_VERSION`]: the version after the last migration that isn't
/// additive. Migrations up to 0026 shipped before this existed and count as
/// not additive.
pub(super) const MIN_READER: i64 = {
    let mut v = MIGRATIONS.len();
    while v > 0 && MIGRATIONS[v - 1].additive {
        v -= 1;
    }
    v as i64
};

/// The `min_reader` a newer quarkd recorded, if any.
fn recorded_min_reader(conn: &Connection) -> Result<Option<i64>> {
    let has_table: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_compat')",
        [],
        |r| r.get(0),
    )?;
    if !has_table {
        return Ok(None);
    }
    Ok(conn
        .query_row("SELECT min_reader FROM schema_compat", [], |r| r.get(0))
        .ok())
}

/// Bring `conn` up to [`SCHEMA_VERSION`], one transaction per migration.
pub(super) fn migrate(conn: &mut Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > SCHEMA_VERSION {
        return match recorded_min_reader(conn)? {
            Some(min) if min <= SCHEMA_VERSION => {
                tracing::info!(
                    schema = version,
                    ours = SCHEMA_VERSION,
                    "database is from a newer quarkd, with only additive changes since ours; using it as it is"
                );
                Ok(())
            }
            _ => Err(StoreError::Invalid(format!(
                "database schema {version} is newer than this quarkd ({SCHEMA_VERSION})"
            ))),
        };
    }
    for (i, m) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(m.sql)?;
        tx.pragma_update(None, "user_version", i as i64 + 1)?;
        tx.commit()?;
        tracing::info!(migration = m.name, "applied schema migration");
    }
    // Said again on every open, so it stays right after this quarkd migrates.
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS schema_compat (min_reader INTEGER NOT NULL);
         DELETE FROM schema_compat;
         INSERT INTO schema_compat (min_reader) VALUES ({MIN_READER});"
    ))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_reader_follows_the_last_non_additive_migration() {
        const {
            assert!(MIN_READER <= SCHEMA_VERSION);
            // Migrations through 0026 shipped before `additive` existed.
            assert!(MIN_READER >= 26);
        };
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        assert_eq!(recorded_min_reader(&conn).unwrap(), Some(MIN_READER));
    }

    #[test]
    fn opens_a_newer_database_only_when_its_changes_are_additive() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        // A newer quarkd added an additive migration.
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        conn.execute("CREATE TABLE later (x INTEGER)", []).unwrap();
        migrate(&mut conn).unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION + 1, "not migrated down");
        // One whose newer migration wasn't additive.
        conn.execute(
            "UPDATE schema_compat SET min_reader = ?1",
            [SCHEMA_VERSION + 1],
        )
        .unwrap();
        assert!(migrate(&mut conn).is_err());
        // One from before schema_compat.
        conn.execute("DROP TABLE schema_compat", []).unwrap();
        assert!(migrate(&mut conn).is_err());
    }

    #[test]
    fn file_numbers_match_positions() {
        for (i, m) in MIGRATIONS.iter().enumerate() {
            let prefix = format!("{:04}_", i + 1);
            assert!(
                m.name.starts_with(&prefix),
                "migration {} is at position {}; rename it {prefix}...",
                m.name,
                i + 1
            );
        }
    }

    #[test]
    fn every_file_is_listed() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/store/migrations");
        let mut files: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|f| f.ends_with(".sql"))
            .collect();
        files.sort();
        let listed: Vec<String> = MIGRATIONS
            .iter()
            .map(|m| format!("{}.sql", m.name))
            .collect();
        assert_eq!(files, listed);
    }
}
