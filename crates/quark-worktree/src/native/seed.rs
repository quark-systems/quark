//! Seeding ignored files from `.worktreeinclude`, ported from treehouse
//! 3.1.2 (`gitvcs.SeedWorktreeWithInventory`, `RemoveSeededPaths`,
//! `HasUnseededBranchCreationOutput`).
//!
//! When a slot is handed out, the `.worktreeinclude` committed at the slot's
//! HEAD is read as a git exclude file (so negation, escaping and `**` mean
//! what they mean to git) and every file of the primary checkout that it
//! selects *and* that git ignores is copied into the slot, unless the path
//! is tracked at the slot's commit. The copied paths are the slot's seed
//! inventory: the pool records them (HMAC-signed, [`super::state`]) and
//! removes exactly those, and nothing else ignored, before the slot is reset
//! for its next use or returned. A dirty or untracked `.worktreeinclude` in
//! the primary checkout is never trusted.
//!
//! The copy goes through plain file operations, not git's index, so no
//! repository-configured filter runs; a symlink is copied as a regular file
//! holding its target, as treehouse does. Paths are resolved inside each
//! checkout without following a symlinked directory out of it.

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use quark_core::{CoreError, Result};

use super::git::{self, exec, Out};

const WORKTREE_INCLUDE: &str = ".worktreeinclude";

fn backend(e: impl std::fmt::Display) -> CoreError {
    CoreError::Backend(e.to_string())
}

fn nul_split(data: &[u8]) -> Vec<String> {
    data.split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect()
}

fn git_out(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = exec(dir, args)?;
    if !out.ok() {
        return Err(CoreError::Backend(format!(
            "git {}: {}",
            args.join(" "),
            out.stderr
        )));
    }
    Ok(out.stdout)
}

/// The `.worktreeinclude` blob committed at `wt`'s HEAD, empty when there
/// is none.
fn committed_manifest(wt: &Path) -> Result<Vec<u8>> {
    let listed = git_out(
        wt,
        &[
            "ls-tree",
            "-z",
            "--name-only",
            "--full-tree",
            "HEAD",
            "--",
            WORKTREE_INCLUDE,
        ],
    )?;
    if !nul_split(&listed).iter().any(|n| n == WORKTREE_INCLUDE) {
        return Ok(Vec::new());
    }
    let spec = format!("HEAD:{WORKTREE_INCLUDE}");
    let kind = git_out(wt, &["cat-file", "-t", &spec])?;
    if String::from_utf8_lossy(&kind).trim() != "blob" {
        return Err(CoreError::Backend(format!(
            "committed {WORKTREE_INCLUDE} is not a file"
        )));
    }
    git_out(wt, &["cat-file", "blob", &spec])
}

/// A private temporary file holding `data`, removed on drop.
struct TempFile(PathBuf);

impl TempFile {
    fn new(data: &[u8]) -> Result<Self> {
        let mut suffix = [0u8; 8];
        getrandom::fill(&mut suffix).map_err(backend)?;
        let name: String = suffix.iter().map(|b| format!("{b:02x}")).collect();
        let path = std::env::temp_dir().join(format!("treehouse-worktreeinclude-{name}"));
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| backend(format!("creating {}: {e}", path.display())))?;
        let file = TempFile(path);
        f.write_all(data).map_err(backend)?;
        Ok(file)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The files of `repo` the manifest selects that git also ignores.
fn selected_paths(repo: &Path, manifest: &[u8]) -> Result<Vec<String>> {
    let tmp = TempFile::new(manifest)?;
    let exclude = format!("--exclude-from={}", tmp.0.display());
    let selected = nul_split(&git_out(
        repo,
        &["ls-files", "-z", "--others", "--ignored", &exclude],
    )?);
    if selected.is_empty() {
        return Ok(selected);
    }
    let ignored: std::collections::HashSet<String> = nul_split(&git_out(
        repo,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
        ],
    )?)
    .into_iter()
    .collect();
    Ok(selected
        .into_iter()
        .filter(|p| ignored.contains(p))
        .collect())
}

/// Whether the filesystem holding `wt` folds case (decided by the
/// filesystem, not by a git setting).
fn case_insensitive(wt: &Path) -> Result<bool> {
    let marker = std::fs::metadata(wt.join(".git")).map_err(backend)?;
    match std::fs::metadata(wt.join(".GIT")) {
        Ok(alias) => Ok(alias.dev() == marker.dev() && alias.ino() == marker.ino()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(backend(e)),
    }
}

/// `rel` as normal components only (no `..`, no root), or `None`.
fn components(rel: &str) -> Option<Vec<&std::ffi::OsStr>> {
    let p = Path::new(rel);
    let mut out = Vec::new();
    for c in p.components() {
        match c {
            Component::Normal(n) => out.push(n),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!out.is_empty()).then_some(out)
}

/// `root/rel` when no directory between them is a symlink (Go's `os.Root`
/// refuses to leave the root; a symlinked ancestor is refused outright).
fn rooted(root: &Path, rel: &str) -> Result<PathBuf> {
    let parts = components(rel)
        .ok_or_else(|| CoreError::Backend(format!("path {rel:?} escapes from parent")))?;
    let mut cur = root.to_path_buf();
    for part in &parts[..parts.len() - 1] {
        cur.push(part);
        match std::fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(CoreError::Backend(format!(
                    "path {rel:?} escapes through symlink {}",
                    cur.display()
                )))
            }
            _ => {}
        }
    }
    cur.push(parts[parts.len() - 1]);
    Ok(cur)
}

/// Create the directories of `rel` under `root`, refusing to go through
/// anything that is not a real directory.
fn ensure_dirs(root: &Path, rel: &str) -> Result<()> {
    let Some(parts) = components(rel) else {
        return Ok(());
    };
    let mut cur = root.to_path_buf();
    for part in &parts[..parts.len() - 1] {
        cur.push(part);
        match std::fs::symlink_metadata(&cur) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => {
                return Err(CoreError::Backend(format!(
                    "refusing to replace existing seed ancestor {}",
                    Path::new(rel).parent().unwrap_or(Path::new(rel)).display()
                )))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::DirBuilder::new()
                    .mode(0o755)
                    .create(&cur)
                    .map_err(backend)?;
            }
            Err(e) => return Err(backend(e)),
        }
    }
    Ok(())
}

/// The (device, inode) of `dir` itself, refusing a symlink or non-directory.
fn dir_identity(dir: &Path) -> Result<(u64, u64)> {
    let m = std::fs::symlink_metadata(dir).map_err(backend)?;
    if !m.is_dir() {
        return Err(CoreError::Backend(format!(
            "refusing to use replaced worktree {}",
            dir.display()
        )));
    }
    Ok((m.dev(), m.ino()))
}

/// A seeding that stopped part way: what it had created, and why.
pub(crate) struct SeedFailure {
    pub copied: Vec<String>,
    pub error: CoreError,
}

/// Copy the ignored files of `repo` that the `.worktreeinclude` committed at
/// `wt`'s HEAD selects into `wt`. Returns the paths it created (relative,
/// `/`-separated); on failure, the ones created so far, which belong in
/// cleanup.
pub(crate) fn seed_worktree(
    repo: &Path,
    wt: &Path,
) -> std::result::Result<Vec<String>, SeedFailure> {
    let mut copied = Vec::new();
    let r = seed_into(repo, wt, &mut copied);
    match r {
        Ok(()) => Ok(copied),
        Err(error) => Err(SeedFailure { copied, error }),
    }
}

fn seed_into(repo: &Path, wt: &Path, copied: &mut Vec<String>) -> Result<()> {
    let manifest = committed_manifest(wt)?;
    if manifest.is_empty() {
        return Ok(());
    }
    let selected = selected_paths(repo, &manifest)?;
    if selected.is_empty() {
        return Ok(());
    }

    // Source ignore rules do not authorize replacing paths tracked at the
    // destination, which may be checked out at another commit.
    let fold = case_insensitive(wt)?;
    let canon = |s: &str| {
        if fold {
            s.to_lowercase()
        } else {
            s.to_string()
        }
    };
    let tracked: std::collections::HashSet<String> = nul_split(&git_out(wt, &["ls-files", "-z"])?)
        .iter()
        .map(|s| canon(s))
        .collect();
    let is_tracked = |name: &str| {
        let mut name = canon(name);
        let prefix = format!("{name}/");
        if tracked.iter().any(|t| t.starts_with(&prefix)) {
            return true;
        }
        loop {
            if tracked.contains(&name) {
                return true;
            }
            match name.rfind('/') {
                Some(i) => name.truncate(i),
                None => return false,
            }
        }
    };
    let eligible: Vec<String> = selected.into_iter().filter(|n| !is_tracked(n)).collect();
    if eligible.is_empty() {
        return Ok(());
    }

    let identity = dir_identity(wt)?;
    for name in eligible {
        let src = rooted(repo, &name)?;
        let meta = std::fs::symlink_metadata(&src).map_err(backend)?;
        let (data, mode) = if meta.file_type().is_file() {
            (
                std::fs::read(&src).map_err(backend)?,
                meta.permissions().mode() & 0o777,
            )
        } else if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&src).map_err(backend)?;
            (target.as_os_str().as_encoded_bytes().to_vec(), 0o666)
        } else {
            continue;
        };
        if dir_identity(wt)? != identity {
            return Err(CoreError::Backend(format!(
                "refusing to seed replaced worktree {}",
                wt.display()
            )));
        }
        ensure_dirs(wt, &name)?;
        let dst = rooted(wt, &name)?;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&dst)
            .map_err(|e| backend(format!("creating {}: {e}", dst.display())))?;
        // Once created, the path belongs in cleanup even if writing fails.
        copied.push(name.clone());
        f.write_all(&data).map_err(backend)?;
        f.set_permissions(std::fs::Permissions::from_mode(mode))
            .map_err(backend)?;
    }
    Ok(())
}

/// Whether `name` may appear in a seed inventory (treehouse's check before
/// any removal).
fn valid_seeded_path(name: &str) -> bool {
    let first = name.split('/').next().unwrap_or_default();
    !name.is_empty()
        && !name.starts_with('/')
        && super::state::clean(Path::new(name)) == Path::new(name)
        && !name.ends_with('/')
        && !name.starts_with("./")
        && !name.contains(['\\', '\0'])
        && !first.eq_ignore_ascii_case(".git")
        && !first.eq_ignore_ascii_case(".jj")
}

/// `wt` is a linked worktree its repository still registers: its `.git`
/// file names a gitdir whose `gitdir` back-link is that very file.
fn authenticate_linked(wt: &Path) -> Result<()> {
    let refuse = || {
        CoreError::Refused(format!(
            "refusing to clean unregistered worktree {}",
            wt.display()
        ))
    };
    let marker_path = wt.join(".git");
    let marker = std::fs::symlink_metadata(&marker_path).map_err(|_| refuse())?;
    if !marker.file_type().is_file() {
        return Err(refuse());
    }
    let text = std::fs::read_to_string(&marker_path).map_err(backend)?;
    let dir = text
        .trim()
        .strip_prefix("gitdir: ")
        .filter(|d| !d.is_empty())
        .ok_or_else(refuse)?;
    let dir = if Path::new(dir).is_absolute() {
        PathBuf::from(dir)
    } else {
        wt.join(dir)
    };
    let back =
        std::fs::read_to_string(super::state::clean(&dir).join("gitdir")).map_err(|_| refuse())?;
    let linked =
        std::fs::metadata(super::state::clean(Path::new(back.trim()))).map_err(|_| refuse())?;
    if linked.dev() != marker.dev() || linked.ino() != marker.ino() {
        return Err(refuse());
    }
    Ok(())
}

/// Remove exactly the seeded `paths` from `wt`, whose identity must still be
/// `expected` (taken before the caller's checks).
pub(crate) fn remove_seeded(wt: &Path, paths: &[String], expected: (u64, u64)) -> Result<()> {
    if let Some(bad) = paths.iter().find(|p| !valid_seeded_path(p)) {
        return Err(CoreError::Backend(format!("invalid seeded path {bad:?}")));
    }
    if dir_identity(wt)? != expected {
        return Err(CoreError::Refused(format!(
            "refusing to clean replaced worktree {}",
            wt.display()
        )));
    }
    authenticate_linked(wt)?;
    for name in paths {
        let p = rooted(wt, name)?;
        match std::fs::remove_file(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(backend(format!("removing {}: {e}", p.display()))),
        }
    }
    Ok(())
}

/// The identity [`remove_seeded`] checks, taken now.
pub(crate) fn identity(wt: &Path) -> Result<(u64, u64)> {
    dir_identity(wt)
}

/// After a failed branch creation in a slot: whether it may hold output
/// that is not the seeded files (a `reference-transaction` hook is
/// installed, an untracked or ignored file outside the inventory, or a
/// dirty tree). Errors count as "may".
pub(crate) fn has_unseeded_branch_output(wt: &Path, seeded: &[String]) -> bool {
    inspect_branch_output(wt, seeded).unwrap_or(true)
}

fn inspect_branch_output(wt: &Path, seeded: &[String]) -> Result<bool> {
    let configured = exec(wt, &["config", "--get", "core.hooksPath"])?;
    let hook = match configured.code {
        Some(0) => PathBuf::from(configured.text()).join("reference-transaction"),
        Some(1) => PathBuf::from(git::run(
            wt,
            &["rev-parse", "--git-path", "hooks/reference-transaction"],
        )?),
        _ => return Ok(true),
    };
    let hook = if hook.is_absolute() {
        hook
    } else {
        wt.join(hook)
    };
    match std::fs::metadata(&hook) {
        Ok(m) if m.is_file() && m.permissions().mode() & 0o111 != 0 => return Ok(true),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Ok(true),
    }
    let known: std::collections::HashSet<&str> = seeded.iter().map(String::as_str).collect();
    for args in [
        &["ls-files", "-z", "--others", "--exclude-standard"][..],
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
        ][..],
    ] {
        let out: Out = exec(wt, args)?;
        if !out.ok() {
            return Ok(true);
        }
        if nul_split(&out.stdout)
            .iter()
            .any(|n| !known.contains(n.as_str()))
        {
            return Ok(true);
        }
    }
    git::is_dirty(wt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_path_rules() {
        assert!(valid_seeded_path(".env"));
        assert!(valid_seeded_path("config/local.toml"));
        for bad in [
            "",
            "/etc/passwd",
            "a/../b",
            "./a",
            ".git/config",
            ".JJ/x",
            "a\\b",
            "a/",
        ] {
            assert!(!valid_seeded_path(bad), "{bad}");
        }
    }

    #[test]
    fn rooted_refuses_escapes_and_symlinked_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::os::unix::fs::symlink(tmp.path(), root.join("link")).unwrap();
        assert!(rooted(&root, "real/x").is_ok());
        assert!(rooted(&root, "../x").is_err());
        assert!(rooted(&root, "link/x").is_err());
        // The final component may be a symlink (it is read, not followed).
        assert!(rooted(&root, "link").is_ok());
    }
}
