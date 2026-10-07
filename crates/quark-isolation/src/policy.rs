//! Checking a [`Policy`] and building one for a git worktree.
//!
//! The rules both sandboxes enforce:
//!
//! - every path is absolute and exists; it is canonicalized, because
//!   Seatbelt matches real paths (`/tmp` is `/private/tmp` on macOS);
//! - the working directory is always writable;
//! - a readable path is read-only even when it sits inside a writable one;
//! - the whole filesystem stays readable, since harnesses and toolchains
//!   read from all over it;
//! - `allowed_hosts` is refused: neither sandbox can filter by host name
//!   without an egress proxy, and silently widening or dropping the list
//!   would both be wrong.

use std::path::{Path, PathBuf};

use quark_core::isolation::{Policy, ProcessSpec};
use quark_core::{CoreError, Result};

/// A policy checked and canonicalized for one process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub cwd: PathBuf,
    /// Writable paths, the working directory first, parents before
    /// children.
    pub writable: Vec<PathBuf>,
    /// Read-only carve-outs, parents before children.
    pub readable: Vec<PathBuf>,
    pub network: bool,
}

impl Resolved {
    pub fn from_spec(spec: &ProcessSpec) -> Result<Self> {
        let policy = &spec.policy;
        if !policy.allowed_hosts.is_empty() {
            return Err(CoreError::Unsupported(format!(
                "host allowlists ({}) need an egress proxy; use network on or off",
                policy.allowed_hosts.join(", ")
            )));
        }
        let cwd = canonical(&spec.cwd, "working directory")?;
        let mut writable = vec![cwd.clone()];
        for p in &policy.writable {
            push_unique(&mut writable, canonical(p, "writable path")?);
        }
        let mut readable = Vec::new();
        for p in &policy.readable {
            push_unique(&mut readable, canonical(p, "readable path")?);
        }
        // The working directory stays first; the rest go parents first so
        // nested bind mounts stack the right way.
        writable[1..].sort_by_key(|p| p.components().count());
        readable.sort_by_key(|p| p.components().count());
        Ok(Self {
            cwd,
            writable,
            readable,
            network: policy.network,
        })
    }
}

fn canonical(p: &Path, what: &str) -> Result<PathBuf> {
    if !p.is_absolute() {
        return Err(CoreError::Invalid(format!(
            "{what} {} is not absolute",
            p.display()
        )));
    }
    std::fs::canonicalize(p).map_err(|e| CoreError::Invalid(format!("{what} {}: {e}", p.display())))
}

fn push_unique(v: &mut Vec<PathBuf>, p: PathBuf) {
    if !v.contains(&p) {
        v.push(p);
    }
}

/// A sandbox policy for a worker in `worktree`: it can edit the tree and
/// commit, and nothing more.
///
/// For a linked worktree that means writing its own git dir and the shared
/// object store, refs and reflogs. It also pins as read-only every file
/// that tells git where its config lives (`<worktree>/.git`, the git dir's
/// `commondir` and `gitdir`, and `config.worktree`). Otherwise a worker
/// could point the worktree at a config it wrote, with say a
/// `core.fsmonitor` command, and Quark's own `git status` outside the
/// sandbox would run it. For a primary checkout, `.git/config` and
/// `.git/hooks` are pinned read-only for the same reason.
///
/// Network is off; callers add writable paths a harness needs (its state
/// directory, package caches) and turn network on as the task requires.
pub fn git_worktree_policy(worktree: &Path) -> Result<Policy> {
    let worktree = canonical(worktree, "worktree")?;
    let dot_git = worktree.join(".git");
    let mut policy = Policy::default();
    if dot_git.is_dir() {
        for name in ["config", "hooks"] {
            push_existing(&mut policy.readable, dot_git.join(name));
        }
        return Ok(policy);
    }
    let text = std::fs::read_to_string(&dot_git)
        .map_err(|e| CoreError::Invalid(format!("{}: {e}", dot_git.display())))?;
    let gitdir = text
        .lines()
        .find_map(|l| l.strip_prefix("gitdir:"))
        .map(|s| worktree.join(s.trim()))
        .ok_or_else(|| CoreError::Invalid(format!("{} has no gitdir line", dot_git.display())))?;
    let gitdir = canonical(&gitdir, "git dir")?;
    let common = match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(s) => canonical(&gitdir.join(s.trim()), "git common dir")?,
        Err(_) => gitdir.clone(),
    };
    policy.writable.push(gitdir.clone());
    for name in ["objects", "refs", "logs"] {
        push_existing(&mut policy.writable, common.join(name));
    }
    push_existing(&mut policy.readable, dot_git);
    for name in ["commondir", "gitdir", "config.worktree"] {
        push_existing(&mut policy.readable, gitdir.join(name));
    }
    Ok(policy)
}

fn push_existing(v: &mut Vec<PathBuf>, p: PathBuf) {
    if p.exists() {
        v.push(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quark_core::IsolationMode;
    use std::collections::BTreeMap;

    fn spec(cwd: &Path, policy: Policy) -> ProcessSpec {
        ProcessSpec {
            argv: vec!["true".into()],
            cwd: cwd.into(),
            env: BTreeMap::new(),
            mode: IsolationMode::Sandbox,
            policy,
        }
    }

    #[test]
    fn cwd_is_always_writable_and_first() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("a/b");
        std::fs::create_dir_all(&sub).unwrap();
        let policy = Policy {
            writable: vec![sub.clone(), dir.path().into(), dir.path().into()],
            ..Policy::default()
        };
        let cwd = tempfile::tempdir().unwrap();
        let r = Resolved::from_spec(&spec(cwd.path(), policy)).unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(
            r.writable,
            vec![
                std::fs::canonicalize(cwd.path()).unwrap(),
                root.clone(),
                root.join("a/b")
            ]
        );
    }

    #[test]
    fn refuses_relative_missing_and_host_lists() {
        let dir = tempfile::tempdir().unwrap();
        let rel = Policy {
            writable: vec!["rel".into()],
            ..Policy::default()
        };
        assert!(matches!(
            Resolved::from_spec(&spec(dir.path(), rel)),
            Err(CoreError::Invalid(_))
        ));
        let missing = Policy {
            readable: vec![dir.path().join("nope")],
            ..Policy::default()
        };
        assert!(matches!(
            Resolved::from_spec(&spec(dir.path(), missing)),
            Err(CoreError::Invalid(_))
        ));
        let hosts = Policy {
            allowed_hosts: vec!["github.com".into()],
            ..Policy::default()
        };
        assert!(matches!(
            Resolved::from_spec(&spec(dir.path(), hosts)),
            Err(CoreError::Unsupported(_))
        ));
    }

    #[test]
    fn linked_worktree_policy() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let common = root.join("repo/.git");
        let gitdir = common.join("worktrees/wt");
        for d in ["objects", "refs", "logs", "hooks"] {
            std::fs::create_dir_all(common.join(d)).unwrap();
        }
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        std::fs::write(gitdir.join("gitdir"), "x\n").unwrap();
        let wt = root.join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: ../repo/.git/worktrees/wt\n").unwrap();

        let p = git_worktree_policy(&wt).unwrap();
        assert_eq!(
            p.writable,
            vec![
                gitdir.clone(),
                common.join("objects"),
                common.join("refs"),
                common.join("logs")
            ]
        );
        assert_eq!(
            p.readable,
            vec![
                wt.join(".git"),
                gitdir.join("commondir"),
                gitdir.join("gitdir")
            ]
        );
        assert!(!p.network);
    }

    #[test]
    fn primary_checkout_policy_pins_config_and_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join(".git/hooks")).unwrap();
        std::fs::write(root.join(".git/config"), "").unwrap();
        let p = git_worktree_policy(&root).unwrap();
        assert!(p.writable.is_empty());
        assert_eq!(
            p.readable,
            vec![root.join(".git/config"), root.join(".git/hooks")]
        );
    }
}
