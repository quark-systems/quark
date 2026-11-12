//! The Quark home's format version, and the migrations that bring an older
//! home forward.
//!
//! `<home>/format.json` records which layout of `~/.quark` the files follow
//! (`{"format": 1, "written_by": "0.1.0"}`). quarkd calls [`migrate`] before
//! it opens anything in the home:
//!
//! - a home with no Quark state yet is stamped with [`CURRENT`];
//! - an older home is migrated one step at a time, and the file is rewritten
//!   after each step, so an interrupted upgrade carries on from where it
//!   stopped on the next start;
//! - a home a newer quarkd wrote is refused, so an older quarkd never reads
//!   (or writes) a layout it does not know, unless the newer quarkd says
//!   (`min_reader`) that every step since this one's format only added things
//!   older code ignores. Then the older quarkd (after an update rolled back)
//!   uses the home as it is and leaves the file alone.
//!
//! The databases keep their own schema versions (SQLite `user_version`)
//! and migrate themselves when opened; this version covers the files around
//! them.
//!
//! # Format 0 → 1 (homes made before 0.1)
//!
//! A home with Quark state but no `format.json` dates from before the
//! format was versioned, possibly from before firstmate was retired. Step 1
//! absorbs the shims quarkd used to run on every start:
//!
//! 1. firstmate's files are moved out of every workspace it made
//!    ([`crate::firstmate::workspace::retire`], backups under
//!    `<home>/backups/firstmate`);
//! 2. a source clone firstmate made at `<workspace>/projects/<name>` gets a
//!    link at `<workspace>/repos/<name>`, where the native engine looks. The
//!    clone itself does not move: its worktrees know it by its path;
//! 3. the start that ran it also starts a coordinator for any ready Project
//!    the engine has no coordinator record of (a Project created under
//!    firstmate), see [`Migrated::adopt_legacy_projects`].

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// The file, in the home.
pub const FILE: &str = "format.json";
/// The format this quarkd writes.
pub const CURRENT: u32 = 1;
/// The oldest format a quarkd must know to use a home at [`CURRENT`]: the
/// format of the last step that isn't additive. Leave it when a new step only
/// adds files or directories older quarkds ignore; set it to the new format
/// when a step moves, renames or reinterprets anything.
pub const MIN_READER: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Format {
    pub format: u32,
    /// The quarkd version that last wrote the file.
    #[serde(default)]
    pub written_by: String,
    /// The oldest format a quarkd must know to use this home ([`MIN_READER`]
    /// of the quarkd that wrote it). Missing means `format`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_reader: Option<u32>,
}

/// What [`migrate`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migrated {
    /// The format found; `None` for a home with no Quark state yet.
    pub from: Option<u32>,
    pub to: u32,
    /// Workspaces firstmate's files were moved out of.
    pub retired: Vec<PathBuf>,
    /// `repos/<name>` links made to firstmate's `projects/<name>` clones.
    pub linked: Vec<PathBuf>,
    /// This start migrated a home made before 0.1, so recovery should start
    /// coordinators for ready Projects the engine has no record of.
    pub adopt_legacy_projects: bool,
}

/// The home's format, or `None` when the file is missing.
pub fn read(home: &Path) -> anyhow::Result<Option<Format>> {
    let path = home.join(FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .with_context(|| format!("reading {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Bring the home at `home` to [`CURRENT`], or refuse a newer one.
pub fn migrate(home: &Path) -> anyhow::Result<Migrated> {
    std::fs::create_dir_all(home).with_context(|| format!("creating {}", home.display()))?;
    let found = read(home)?;
    let written_by = found.as_ref().map(|f| f.written_by.clone());
    let mut out = Migrated {
        from: found.as_ref().map(|f| f.format),
        to: CURRENT,
        retired: Vec::new(),
        linked: Vec::new(),
        adopt_legacy_projects: false,
    };
    let mut at = match found {
        Some(f) if f.format > CURRENT && f.min_reader.unwrap_or(f.format) <= CURRENT => {
            tracing::info!(
                format = f.format,
                ours = CURRENT,
                written_by = %f.written_by,
                "the Quark home is from a newer quarkd, with only additive changes since ours; using it as it is"
            );
            out.to = f.format;
            return Ok(out);
        }
        Some(f) if f.format > CURRENT => {
            anyhow::bail!(
            "{} is format {} (written by quarkd {}), newer than this quarkd {} knows ({CURRENT}); \
             run the newer Quark, or give this one another home with --home",
            home.display(),
            f.format,
            if f.written_by.is_empty() { "?" } else { &f.written_by },
            env!("CARGO_PKG_VERSION"),
        )
        }
        Some(f) => f.format,
        None if has_state(home) => 0,
        None => {
            write(home, CURRENT)?;
            return Ok(out);
        }
    };
    if at == CURRENT {
        // Rewrite only when another version last wrote it.
        if written_by.as_deref() != Some(env!("CARGO_PKG_VERSION")) {
            write(home, CURRENT)?;
        }
        return Ok(out);
    }
    while at < CURRENT {
        let next = at + 1;
        tracing::info!(from = at, to = next, home = %home.display(), "upgrading the Quark home");
        match next {
            1 => to_1(home, &mut out).context("upgrading the Quark home to format 1")?,
            n => anyhow::bail!("no migration to format {n}"),
        }
        write(home, next)?;
        at = next;
    }
    Ok(out)
}

/// Quark state a home made before the format file would have.
fn has_state(home: &Path) -> bool {
    ["quark.db", quark_eventlog::FILE_NAME, "workspaces"]
        .iter()
        .any(|n| home.join(n).exists())
}

fn write(home: &Path, format: u32) -> anyhow::Result<()> {
    let path = home.join(FILE);
    let tmp = home.join(format!("{FILE}.tmp"));
    let body = serde_json::to_string_pretty(&Format {
        format,
        written_by: env!("CARGO_PKG_VERSION").into(),
        min_reader: Some(MIN_READER.min(format)),
    })?;
    std::fs::write(&tmp, body + "\n").with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn to_1(home: &Path, out: &mut Migrated) -> anyhow::Result<()> {
    let layout = crate::provision::Layout::new(home);
    out.retired = crate::firstmate::workspace::retire(&layout)?
        .into_iter()
        .map(|r| r.workspace)
        .collect();
    out.linked = link_firstmate_clones(&home.join("workspaces"))?;
    out.adopt_legacy_projects = true;
    Ok(())
}

/// Firstmate cloned a Project's source repos into `<workspace>/projects/`;
/// the native engine reads `<workspace>/repos/`. Link each clone there,
/// unless `repos/` already has a checkout of that name.
fn link_firstmate_clones(workspaces: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut linked = Vec::new();
    let Ok(entries) = std::fs::read_dir(workspaces) else {
        return Ok(linked);
    };
    let mut roots: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    roots.sort();
    for root in roots {
        let Ok(clones) = std::fs::read_dir(root.join("projects")) else {
            continue;
        };
        let mut clones: Vec<PathBuf> = clones
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join(".git").exists())
            .collect();
        clones.sort();
        for clone in clones {
            let Some(name) = clone.file_name() else {
                continue;
            };
            let link = root
                .join(crate::engine::native_project::REPOS_DIR)
                .join(name);
            if link.symlink_metadata().is_ok() {
                continue;
            }
            std::fs::create_dir_all(link.parent().expect("repos dir"))?;
            symlink(&clone, &link)
                .with_context(|| format!("linking {} to {}", link.display(), clone.display()))?;
            linked.push(link);
        }
    }
    Ok(linked)
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn symlink(_target: &Path, link: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other(format!(
        "cannot link {} on this platform",
        link.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_home_is_stamped_current() {
        let dir = tempfile::tempdir().unwrap();
        let m = migrate(dir.path()).unwrap();
        assert_eq!(m.from, None);
        assert!(!m.adopt_legacy_projects);
        assert_eq!(read(dir.path()).unwrap().unwrap().format, CURRENT);
        // Again: nothing to do.
        let m = migrate(dir.path()).unwrap();
        assert_eq!(m.from, Some(CURRENT));
        assert!(!m.adopt_legacy_projects);
    }

    #[test]
    fn a_newer_home_is_refused_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"{"format": 99, "written_by": "9.0.0"}"#;
        std::fs::write(dir.path().join(FILE), body).unwrap();
        let err = migrate(dir.path()).unwrap_err().to_string();
        assert!(err.contains("format 99") && err.contains("9.0.0"), "{err}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join(FILE)).unwrap(),
            body
        );
    }

    #[test]
    fn a_newer_home_with_only_additive_steps_is_used_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let body = format!(r#"{{"format": 99, "written_by": "9.0.0", "min_reader": {CURRENT}}}"#);
        std::fs::write(dir.path().join(FILE), &body).unwrap();
        let m = migrate(dir.path()).unwrap();
        assert_eq!((m.from, m.to), (Some(99), 99));
        assert_eq!(
            std::fs::read_to_string(dir.path().join(FILE)).unwrap(),
            body,
            "an older quarkd never rewrites a newer home's format"
        );
    }

    #[test]
    fn writes_its_min_reader() {
        let dir = tempfile::tempdir().unwrap();
        migrate(dir.path()).unwrap();
        let f = read(dir.path()).unwrap().unwrap();
        assert_eq!(f.min_reader, Some(MIN_READER));
        const { assert!(MIN_READER <= CURRENT) };
    }

    #[test]
    fn a_home_with_state_and_no_file_is_format_0() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("quark.db"), "").unwrap();
        let m = migrate(dir.path()).unwrap();
        assert_eq!(m.from, None);
        assert!(m.adopt_legacy_projects);
        assert_eq!(read(dir.path()).unwrap().unwrap().format, 1);
    }

    #[cfg(unix)]
    #[test]
    fn firstmate_clones_are_linked_into_repos() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("workspaces/prj_1");
        std::fs::create_dir_all(ws.join("projects/app/.git")).unwrap();
        std::fs::create_dir_all(ws.join("projects/notes")).unwrap();
        std::fs::create_dir_all(ws.join("projects/site/.git")).unwrap();
        std::fs::create_dir_all(ws.join("repos/site/.git")).unwrap();
        let linked = link_firstmate_clones(&dir.path().join("workspaces")).unwrap();
        assert_eq!(linked, vec![ws.join("repos/app")]);
        assert_eq!(
            std::fs::read_link(ws.join("repos/app")).unwrap(),
            ws.join("projects/app")
        );
        assert!(!ws.join("repos/notes").exists());
        assert!(!ws.join("repos/site").is_symlink());
    }
}
