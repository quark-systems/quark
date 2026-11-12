//! Backups of the Quark home's databases, taken before an update migrates them.
//!
//! A backup is `<home>/backups/<label>/`: a consistent copy of each database
//! (`VACUUM INTO`, safe while quarkd runs) and `format.json`. Worktrees, repo
//! clones and logs are left out; they don't change format and are large. Only
//! the newest [`KEEP`] backups stay. Restoring is for a rollback to a quarkd
//! that can't read the migrated home ([`crate::compat::Compat::readable_by`]):
//! the updater asks for it with [`request_restore`], and the next quarkd to
//! start copies the files back before it opens anything ([`restore_pending`]).

use std::path::{Path, PathBuf};

use anyhow::Context;

/// Backups kept; older ones are removed when a new one is taken.
pub const KEEP: usize = 2;
/// The databases a backup copies, by file name in the home.
pub const DATABASES: &[&str] = &["quark.db", quark_eventlog::FILE_NAME];
const DIR: &str = "backups";
/// Backups taken here start with this, so pruning leaves other backups alone
/// (firstmate's retirement keeps its own under `backups/firstmate`).
const PREFIX: &str = "pre-";

/// Copies the home's databases and format file to `<home>/backups/pre-<label>`,
/// replacing an earlier backup with that label, then prunes to [`KEEP`].
pub fn snapshot(home: &Path, label: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        !label.is_empty()
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)),
        "backup label {label:?}: letters, digits, '.', '_' and '-' only"
    );
    let root = home.join(DIR);
    let dest = root.join(format!("{PREFIX}{label}"));
    let tmp = root.join(format!(".{PREFIX}{label}.partial"));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    for name in DATABASES {
        let src = home.join(name);
        if !src.exists() {
            continue;
        }
        let conn = rusqlite::Connection::open(&src)
            .with_context(|| format!("opening {}", src.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let out = tmp.join(name);
        conn.execute("VACUUM INTO ?1", [out.to_string_lossy().as_ref()])
            .with_context(|| format!("copying {}", src.display()))?;
    }
    let format = home.join(crate::home_format::FILE);
    if format.exists() {
        std::fs::copy(&format, tmp.join(crate::home_format::FILE))?;
    }
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&tmp, &dest).with_context(|| format!("writing {}", dest.display()))?;
    prune(&root, &dest)?;
    Ok(dest)
}

/// Names the backup the next quarkd start restores (`<home>/backups/restore`).
pub fn request_restore(home: &Path, backup: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(backup.is_dir(), "no backup at {}", backup.display());
    let root = home.join(DIR);
    std::fs::create_dir_all(&root)?;
    std::fs::write(root.join(RESTORE), backup.to_string_lossy().as_bytes())?;
    Ok(())
}

const RESTORE: &str = "restore";

/// Restores the backup [`request_restore`] named, if any, over the home's
/// databases and format file. Runs at quarkd start, before anything in the
/// home is opened. Returns the backup restored.
pub fn restore_pending(home: &Path) -> anyhow::Result<Option<PathBuf>> {
    let marker = home.join(DIR).join(RESTORE);
    let Ok(named) = std::fs::read_to_string(&marker) else {
        return Ok(None);
    };
    let backup = PathBuf::from(named.trim());
    anyhow::ensure!(
        backup.starts_with(home.join(DIR)) && backup.is_dir(),
        "restore names {}, which is not a backup in this home",
        backup.display()
    );
    for name in DATABASES {
        let src = backup.join(name);
        if !src.exists() {
            continue;
        }
        let dest = home.join(name);
        for suffix in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(home.join(format!("{name}{suffix}")));
        }
        let tmp = home.join(format!("{name}.restore"));
        std::fs::copy(&src, &tmp).with_context(|| format!("copying {}", src.display()))?;
        std::fs::rename(&tmp, &dest)?;
    }
    let format = backup.join(crate::home_format::FILE);
    if format.exists() {
        std::fs::copy(&format, home.join(crate::home_format::FILE))?;
    }
    std::fs::remove_file(&marker)?;
    Ok(Some(backup))
}

/// Removes all but the newest [`KEEP`] `pre-*` backups; `keep` always stays.
fn prune(root: &Path, keep: &Path) -> anyhow::Result<()> {
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(root)?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(PREFIX))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    found.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    let older = found.into_iter().map(|(_, p)| p).filter(|p| p != keep);
    for path in older.skip(KEEP - 1) {
        std::fs::remove_dir_all(&path)
            .with_context(|| format!("removing old backup {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_the_databases_and_keeps_the_newest_two() {
        let home = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(&home.path().join("quark.db")).unwrap();
        drop(
            quark_eventlog::SqliteEventLog::open(home.path().join(quark_eventlog::FILE_NAME))
                .unwrap(),
        );
        crate::home_format::migrate(home.path()).unwrap();

        let a = snapshot(home.path(), "0.1.1").unwrap();
        for f in [
            "quark.db",
            quark_eventlog::FILE_NAME,
            crate::home_format::FILE,
        ] {
            assert!(a.join(f).is_file(), "{f}");
        }
        // The copy opens as a store of the same schema.
        let v: i64 = rusqlite::Connection::open(a.join("quark.db"))
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, crate::store::SCHEMA_VERSION);
        drop(store);

        std::thread::sleep(std::time::Duration::from_millis(20));
        let b = snapshot(home.path(), "0.1.2").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let c = snapshot(home.path(), "0.1.3").unwrap();
        assert!(!a.exists(), "oldest pruned");
        assert!(b.exists() && c.exists());
        // Someone else's backups are left alone.
        std::fs::create_dir_all(home.path().join("backups/firstmate")).unwrap();
        snapshot(home.path(), "0.1.4").unwrap();
        assert!(home.path().join("backups/firstmate").exists());
        assert!(snapshot(home.path(), "../x").is_err());
    }

    #[test]
    fn restores_a_requested_backup_once() {
        let home = tempfile::tempdir().unwrap();
        let db = home.path().join("quark.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE t(v); INSERT INTO t VALUES ('before');")
            .unwrap();
        let backup = snapshot(home.path(), "0.2.0").unwrap();
        conn.execute_batch("UPDATE t SET v = 'after'").unwrap();
        drop(conn);

        assert_eq!(restore_pending(home.path()).unwrap(), None);
        request_restore(home.path(), &backup).unwrap();
        assert_eq!(restore_pending(home.path()).unwrap(), Some(backup));
        let v: String = rusqlite::Connection::open(&db)
            .unwrap()
            .query_row("SELECT v FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, "before");
        assert_eq!(restore_pending(home.path()).unwrap(), None, "only once");

        std::fs::write(home.path().join("backups/restore"), "/etc").unwrap();
        assert!(restore_pending(home.path()).is_err(), "outside the home");
    }
}
