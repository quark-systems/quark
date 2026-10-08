//! Schema migrations, one SQL file each under `migrations/`.
//!
//! A migration's version is its position in [`MIGRATIONS`] (first is 1), and
//! its file is named `NNNN_name.sql` with that version. Add a migration by
//! adding a file and one line at the end of the list. If another branch took
//! the same number first, renumber yours at rebase: rename the file and its
//! line. Never edit a migration that has shipped.

use rusqlite::Connection;

use super::{Result, StoreError};

pub(super) struct Migration {
    pub name: &'static str,
    pub sql: &'static str,
}

macro_rules! migration {
    ($name:literal) => {
        Migration {
            name: $name,
            sql: include_str!(concat!("migrations/", $name, ".sql")),
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
];

/// The schema version a fully migrated database has.
pub(super) const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// Bring `conn` up to [`SCHEMA_VERSION`], one transaction per migration.
pub(super) fn migrate(conn: &mut Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(StoreError::Invalid(format!(
            "database schema {version} is newer than this quarkd ({SCHEMA_VERSION})"
        )));
    }
    for (i, m) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(m.sql)?;
        tx.pragma_update(None, "user_version", i as i64 + 1)?;
        tx.commit()?;
        tracing::info!(migration = m.name, "applied schema migration");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
