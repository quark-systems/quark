//! `prune` and `destroy`, ported from treehouse 3.1.2 (`internal/pool/
//! prune.go` and `destroy.go`) for git pools.
//!
//! Both classify a slot with the same safety facts: a slot is **disposable**
//! only when it is unleased, unreserved, nothing runs in it, it is clean
//! (no tracked or untracked changes), its backing repository exists, and
//! its HEAD is merged into the default branch's merge ref (origin's
//! default tracking ref, refused when stale), or into the base it was cut
//! from. Anything that cannot be proven is kept.
//!
//! - **Prune** sweeps a pool and removes every disposable slot, skipping
//!   busy ones (leased, reserved, in use) silently and reporting the rest
//!   it kept. Orphans (backing repository gone) are reported, never
//!   removed: treehouse only removes them with `--prune-orphans`, which
//!   Quark does not use.
//! - **Destroy** removes one named slot. A bare destroy removes only a
//!   disposable slot; each risky class (leased, in use, unlanded) needs its
//!   own opt-in in [`DestroyOptions`], as treehouse's `--include-*` flags.
//!   The bulk `destroy --all` is not ported: it crosses clones that share a
//!   pool, and Quark retires one clone's slots by name instead.
//!
//! Removal is treehouse's two-phase reservation: under the lock each slot
//! is re-checked and marked `destroying` with this process as its owner;
//! `pre_destroy` hooks run with the lock released; then, locked again, only
//! slots whose reservation is intact are checked a last time and removed
//! (`git worktree remove`, then the slot directory). A slot re-acquired in
//! between is left alone.
//!
//! jj slots are kept (the native pool is git only), and so is anything
//! whose VCS marker is missing.

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use quark_core::{CoreError, Result};

use super::state::{self, Entry, State};
use super::{git, heal, locked, procs, same_path, NativePool, TERMINATE_GRACE};
use crate::treehouse::{Cleanup, Kept, Removed};

/// Which risky slots a [`NativePool::destroy_blocking`] may remove, each
/// opt-in like treehouse's `--include-*` flags. The default removes only a
/// disposable slot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DestroyOptions {
    /// Dirty, unmerged or unverifiable slots (data loss).
    pub include_unlanded: bool,
    /// Slots something runs in or holds reserved; processes are stopped first.
    pub include_in_use: bool,
    /// A leased slot.
    pub include_leased: bool,
}

/// The merge ref a slot's repository compares with, resolved once per
/// repository per operation (treehouse's `worktreePruneContextResolver`).
struct Contexts {
    fetch: bool,
    cache: HashMap<PathBuf, std::result::Result<String, String>>,
}

/// One slot's repository and its merge ref (or why there is none).
struct Context {
    repo: Option<PathBuf>,
    merge_ref: std::result::Result<String, String>,
}

impl Contexts {
    fn new(fetch: bool) -> Self {
        Self {
            fetch,
            cache: HashMap::new(),
        }
    }

    fn resolve(&mut self, wt: &Path) -> Context {
        let repo = match git::find_main_repo_root_from(wt) {
            Ok(r) => r,
            Err(e) => {
                return Context {
                    repo: None,
                    merge_ref: Err(format!("cannot resolve the repository: {e}")),
                }
            }
        };
        let fetch = self.fetch;
        let merge_ref = self
            .cache
            .entry(repo.clone())
            .or_insert_with(|| {
                if fetch {
                    git::fetch(&repo).map_err(|e| format!("cannot fetch origin: {e}"))?;
                }
                git::default_branch_merge_ref(&repo)
                    .map_err(|e| format!("cannot resolve the default branch: {e}"))
            })
            .clone();
        Context {
            repo: Some(repo),
            merge_ref,
        }
    }
}

/// What prune makes of one slot.
enum Verdict {
    /// Leased, reserved or in use: not a candidate, not reported.
    Busy,
    Keep(String),
    Remove(u64),
}

/// Destroy's safety classes, in treehouse's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Leased,
    InUse,
    Dirty,
    Unmerged,
    Unverified,
}

impl Class {
    fn name(self) -> &'static str {
        match self {
            Class::Leased => "leased",
            Class::InUse => "in use",
            Class::Dirty => "dirty",
            Class::Unmerged => "unmerged",
            Class::Unverified => "unverified",
        }
    }
}

/// A slot classified for destroy.
struct Target {
    classes: Vec<Class>,
    /// The first class's diagnostic.
    detail: String,
}

impl Target {
    fn add(&mut self, class: Class, detail: impl Into<String>) {
        if self.classes.contains(&class) {
            return;
        }
        if self.classes.is_empty() {
            self.detail = detail.into();
        }
        self.classes.push(class);
    }

    fn has(&self, class: Class) -> bool {
        self.classes.contains(&class)
    }

    fn unlanded(&self) -> bool {
        self.has(Class::Dirty) || self.has(Class::Unmerged) || self.has(Class::Unverified)
    }

    /// The opt-ins it still needs under `opts`, as treehouse's flags.
    fn missing(&self, opts: DestroyOptions) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.has(Class::Leased) && !opts.include_leased {
            out.push("--include-leased");
        }
        if self.has(Class::InUse) && !opts.include_in_use {
            out.push("--include-in-use");
        }
        if self.unlanded() && !opts.include_unlanded {
            out.push("--include-unlanded");
        }
        out
    }

    fn reason(&self) -> String {
        let class = self.classes.first().map_or("kept", |c| c.name());
        if self.detail.is_empty() {
            class.to_string()
        } else {
            format!("{class}: {}", self.detail)
        }
    }
}

impl NativePool {
    // ---- prune ----------------------------------------------------------

    /// treehouse `prune [--yes]` in `repo`: with `dry_run`, what would be
    /// removed (nothing changes but the healed state); otherwise remove it.
    pub fn prune_blocking(&self, repo: &Path, dry_run: bool) -> Result<Cleanup> {
        let ctx = self.ctx(repo)?;
        let pool = ctx.pool;
        if !state::is_pool_dir(&pool) {
            return Ok(Cleanup::default());
        }
        let entries = {
            let _lock = locked(&pool)?;
            let s = heal(state::read(&pool)?);
            state::write(&pool, s.clone())?;
            s.worktrees
        };

        let mut contexts = Contexts::new(self.fetch);
        let mut out = Cleanup::default();
        let mut planned: BTreeMap<PathBuf, u64> = BTreeMap::new();
        for wt in &entries {
            match analyze(&pool, &mut contexts, wt) {
                Verdict::Busy => {}
                Verdict::Keep(reason) => out.kept.push(kept(&wt.path, reason)),
                Verdict::Remove(bytes) => {
                    planned.insert(wt.path.clone(), bytes);
                }
            }
        }
        if dry_run || planned.is_empty() {
            out.removed = planned
                .into_iter()
                .map(|(path, bytes)| Removed { path, bytes })
                .collect();
            return Ok(out);
        }

        // Reserve what is still a candidate.
        let mut reserved: Vec<Entry> = Vec::new();
        {
            let _lock = locked(&pool)?;
            let mut s = heal(state::read(&pool)?);
            for wt in &mut s.worktrees {
                if !planned.contains_key(&wt.path) {
                    continue;
                }
                match analyze(&pool, &mut contexts, wt) {
                    Verdict::Busy => continue,
                    Verdict::Keep(reason) => {
                        out.kept.push(kept(&wt.path, reason));
                        continue;
                    }
                    Verdict::Remove(_) => {}
                }
                wt.destroying = true;
                reserve_owner(wt)?;
                reserved.push(wt.clone());
            }
            state::write(&pool, s)?;
        }

        for wt in &reserved {
            run_hooks(&ctx.cfg.hooks.pre_destroy, &wt.path);
        }

        let _lock = locked(&pool)?;
        let mut s = state::read(&pool)?;
        let mut removed = Vec::new();
        for r in &reserved {
            let Some(wt) = s.worktrees.iter_mut().find(|w| w.path == r.path) else {
                continue;
            };
            if !same_reservation(wt, r) {
                continue;
            }
            let check = if !procs::in_worktree(&wt.path).is_empty() {
                Verdict::Keep("in use".into())
            } else {
                analyze_idle(&pool, &mut contexts, wt)
            };
            let bytes = match check {
                Verdict::Remove(b) if b > 0 => b,
                Verdict::Remove(_) | Verdict::Busy => planned.get(&wt.path).copied().unwrap_or(0),
                Verdict::Keep(reason) => {
                    clear_reservation(wt);
                    out.kept.push(kept(&wt.path, reason));
                    continue;
                }
            };
            let removal = contexts
                .resolve(&wt.path)
                .repo
                .ok_or_else(|| "cannot resolve the repository".to_string())
                .and_then(|repo| {
                    git::remove_worktree(&repo, &wt.path, false)
                        .map_err(|e| format!("git refused to remove the worktree: {e}"))
                })
                .and_then(|()| remove_container(&pool, &wt.path));
            if let Err(reason) = removal {
                clear_reservation(wt);
                out.kept.push(kept(&wt.path, reason));
                continue;
            }
            removed.push(Removed {
                path: wt.path.clone(),
                bytes,
            });
        }
        drop_removed(&mut s, &removed);
        state::write(&pool, s)?;
        out.removed = removed;
        Ok(out)
    }

    // ---- destroy --------------------------------------------------------

    /// treehouse `destroy [--yes] <path> [--include-*]` for the slot at
    /// `path` of `repo`'s pool. A slot `opts` does not cover comes back in
    /// [`Cleanup::kept`], with its class and the opt-in it would need.
    pub fn destroy_blocking(
        &self,
        repo: &Path,
        path: &Path,
        opts: DestroyOptions,
        dry_run: bool,
    ) -> Result<Cleanup> {
        let pool = self.pool_of(repo, path)?;
        let hooks = self.ctx(repo)?.cfg.hooks.pre_destroy;
        let entry = {
            let _lock = locked(&pool)?;
            let s = heal(state::read(&pool)?);
            state::write(&pool, s.clone())?;
            s.worktrees.into_iter().find(|w| same_path(&w.path, path))
        };
        let Some(entry) = entry else {
            return Err(CoreError::NotFound(format!(
                "worktree {} is not managed by treehouse",
                path.display()
            )));
        };

        let mut contexts = Contexts::new(self.fetch);
        let target = classify(&mut contexts, &entry);
        let missing = target.missing(opts);
        if !missing.is_empty() {
            return Ok(refused(&entry.path, &target, &missing));
        }
        let bytes = container_size(&pool, &entry.path);
        if dry_run {
            return Ok(Cleanup {
                removed: vec![Removed {
                    path: entry.path,
                    bytes,
                }],
                kept: Vec::new(),
            });
        }

        // Reserve it, remembering the owner it had.
        let original;
        let reservation;
        {
            let _lock = locked(&pool)?;
            let mut s = heal(state::read(&pool)?);
            let Some(wt) = s.worktrees.iter_mut().find(|w| w.path == entry.path) else {
                return Ok(Cleanup::default());
            };
            if wt.destroying && procs::owner_alive(wt.owner_pid, wt.owner_started_at) {
                return Ok(keep_one(&wt.path, "reserved by another destroy"));
            }
            let target = classify(&mut contexts, wt);
            let missing = target.missing(opts);
            if !missing.is_empty() {
                return Ok(refused(&wt.path, &target, &missing));
            }
            original = (wt.owner_pid, wt.owner_started_at);
            wt.destroying = true;
            reserve_owner(wt)?;
            reservation = wt.clone();
            state::write(&pool, s)?;
        }

        run_hooks(&hooks, &entry.path);

        let _lock = locked(&pool)?;
        let mut s = state::read(&pool)?;
        let Some(wt) = s.worktrees.iter_mut().find(|w| w.path == entry.path) else {
            return Ok(Cleanup::default());
        };
        if !same_reservation(wt, &reservation) {
            return Ok(keep_one(&wt.path, "re-acquired during pre-destroy hook"));
        }
        let restore = |wt: &mut Entry| {
            wt.destroying = false;
            (wt.owner_pid, wt.owner_started_at) = original;
        };
        let mut current = wt.clone();
        restore(&mut current);
        let target = classify(&mut contexts, &current);
        let missing = target.missing(opts);
        let outcome = if !missing.is_empty() {
            Err(refused(&wt.path, &target, &missing))
        } else if target.has(Class::InUse)
            && !procs::terminate(&wt.path, TERMINATE_GRACE).1.is_empty()
        {
            Err(keep_one(
                &wt.path,
                "worktree processes still running after termination",
            ))
        } else {
            remove_managed(&pool, &current).map_err(|reason| keep_one(&wt.path, reason))
        };
        let result = match outcome {
            Ok(()) => {
                let removed = vec![Removed {
                    path: entry.path.clone(),
                    bytes,
                }];
                drop_removed(&mut s, &removed);
                Cleanup {
                    removed,
                    kept: Vec::new(),
                }
            }
            Err(kept) => {
                restore(wt);
                kept
            }
        };
        state::write(&pool, s)?;
        Ok(result)
    }
}

/// Prune's reading of one slot (treehouse's `analyzePruneCandidate`).
fn analyze(pool: &Path, contexts: &mut Contexts, wt: &Entry) -> Verdict {
    if wt.destroying || wt.leased || procs::owner_alive(wt.owner_pid, wt.owner_started_at) {
        return Verdict::Busy;
    }
    if !procs::in_worktree(&wt.path).is_empty() {
        return Verdict::Busy;
    }
    analyze_idle(pool, contexts, wt)
}

/// Whether an idle slot is disposable, and its size (treehouse's
/// `analyzeIdleWorktree`).
fn analyze_idle(pool: &Path, contexts: &mut Contexts, wt: &Entry) -> Verdict {
    match state::marker(&wt.path) {
        Ok(Some("git")) => {}
        Ok(Some(_)) => {
            return Verdict::Keep("a jj workspace: the native pool handles git only".into())
        }
        _ => return Verdict::Keep("no .git or .jj marker".into()),
    }
    if let Some(detail) = backing_repository_missing(&wt.path) {
        return Verdict::Keep(format!("orphaned (backing repository missing): {detail}"));
    }
    match git::is_dirty(&wt.path) {
        Err(e) => return Verdict::Keep(format!("cannot check status: {e}")),
        Ok(true) => return Verdict::Keep("uncommitted changes".into()),
        Ok(false) => {}
    }
    let merge_ref = match contexts.resolve(&wt.path).merge_ref {
        Ok(r) => r,
        Err(reason) => return Verdict::Keep(reason),
    };
    match git::is_head_merged_into_ref(&wt.path, &merge_ref) {
        Err(e) => {
            return Verdict::Keep(format!(
                "cannot prove HEAD is merged into the default branch: {e}"
            ))
        }
        Ok(false) if !landed_on_its_base(wt) => {
            return Verdict::Keep(format!("HEAD not merged into {merge_ref}"))
        }
        Ok(_) => {}
    }
    let container = match removable_container(pool, &wt.path) {
        Ok(c) => c,
        Err(e) => return Verdict::Keep(format!("refusing unsafe cleanup path: {e}")),
    };
    match dir_size(&container) {
        Ok(bytes) => Verdict::Remove(bytes),
        Err(e) => Verdict::Keep(format!("cannot measure size: {e}")),
    }
}

/// Destroy's classes for one slot (treehouse's `classifyForDestroy`).
fn classify(contexts: &mut Contexts, wt: &Entry) -> Target {
    let mut t = Target {
        classes: Vec::new(),
        detail: String::new(),
    };
    if wt.leased {
        let detail = if wt.lease_holder.is_empty() {
            String::new()
        } else {
            format!("held by {}", wt.lease_holder)
        };
        t.add(Class::Leased, detail);
    }
    if procs::owner_alive(wt.owner_pid, wt.owner_started_at)
        || !procs::in_worktree(&wt.path).is_empty()
    {
        t.add(Class::InUse, "");
    }
    match state::marker(&wt.path) {
        Ok(Some("git")) => {}
        Ok(Some(_)) => {
            t.add(
                Class::Unverified,
                "a jj workspace: the native pool handles git only",
            );
            return t;
        }
        _ => {
            t.add(
                Class::Unverified,
                "no .git or .jj marker: contents cannot be verified",
            );
            return t;
        }
    }
    if let Some(detail) = backing_repository_missing(&wt.path) {
        t.add(
            Class::Unverified,
            format!("backing repository missing: {detail}"),
        );
        return t;
    }
    match git::is_dirty(&wt.path) {
        Err(e) => {
            t.add(Class::Unverified, format!("cannot check status: {e}"));
            return t;
        }
        Ok(true) => t.add(Class::Dirty, "uncommitted changes"),
        Ok(false) => {}
    }
    let merge_ref = match contexts.resolve(&wt.path).merge_ref {
        Ok(r) => r,
        Err(_) => {
            t.add(
                Class::Unverified,
                "cannot verify HEAD is merged into the default branch",
            );
            return t;
        }
    };
    match git::is_head_merged_into_ref(&wt.path, &merge_ref) {
        Err(e) => t.add(
            Class::Unverified,
            format!("cannot verify merge into {merge_ref}: {e}"),
        ),
        Ok(false) if !landed_on_its_base(wt) => {
            t.add(Class::Unmerged, format!("HEAD not merged into {merge_ref}"))
        }
        Ok(_) => {}
    }
    t
}

/// A slot the default ref calls unmerged may hold nothing beyond the base
/// it was explicitly cut from (treehouse's `headLandedOnItsBase`). Only an
/// explicitly requested base is recorded, so an empty one never counts.
fn landed_on_its_base(wt: &Entry) -> bool {
    let Some(r) = git::base_branch_merge_ref(&wt.path, &wt.base_branch) else {
        return false;
    };
    git::is_head_merged_into_ref(&wt.path, &r).unwrap_or(false)
}

fn kept(path: &Path, reason: impl Into<String>) -> Kept {
    Kept {
        path: path.to_path_buf(),
        reason: reason.into(),
    }
}

fn keep_one(path: &Path, reason: impl Into<String>) -> Cleanup {
    Cleanup {
        removed: Vec::new(),
        kept: vec![kept(path, reason)],
    }
}

fn refused(path: &Path, target: &Target, missing: &[&str]) -> Cleanup {
    keep_one(
        path,
        format!("{} (needs {})", target.reason(), missing.join(" ")),
    )
}

/// Reserve `wt` for this process, as treehouse's `reserveOwner`.
fn reserve_owner(wt: &mut Entry) -> Result<()> {
    let (pid, started) = procs::own_reservation()
        .ok_or_else(|| CoreError::Backend("failed to determine owner process identity".into()))?;
    wt.owner_pid = pid;
    wt.owner_started_at = started;
    Ok(())
}

fn same_reservation(current: &Entry, reserved: &Entry) -> bool {
    current.path == reserved.path
        && current.destroying
        && current.owner_pid == reserved.owner_pid
        && current.owner_started_at == reserved.owner_started_at
}

fn clear_reservation(wt: &mut Entry) {
    wt.destroying = false;
    wt.owner_pid = 0;
    wt.owner_started_at = 0;
}

fn drop_removed(s: &mut State, removed: &[Removed]) {
    s.worktrees
        .retain(|w| !removed.iter().any(|r| r.path == w.path));
}

/// Run each `pre_destroy` hook with `/bin/sh -c` in `dir`; a failing hook is
/// logged and does not stop the rest (treehouse's `hooks.Run`).
fn run_hooks(hooks: &[String], dir: &Path) {
    for hook in hooks {
        let status = Command::new("/bin/sh")
            .arg("-c")
            .arg(hook)
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => tracing::warn!(hook, dir = %dir.display(), %s, "pre_destroy hook failed"),
            Err(e) => {
                tracing::warn!(hook, dir = %dir.display(), error = %e, "pre_destroy hook failed")
            }
        }
    }
}

/// Deregister the slot from git (when its repository is still there) and
/// delete its directory (treehouse's `removeManagedWorktree`). `--force`
/// because destroy only gets here once the caller opted into what it
/// removes.
fn remove_managed(pool: &Path, wt: &Entry) -> std::result::Result<(), String> {
    let orphaned = backing_repository_missing(&wt.path).is_some();
    let markerless = wt.path.exists() && !matches!(state::marker(&wt.path), Ok(Some(_)));
    if !orphaned && !markerless {
        let repo = git::find_main_repo_root_from(&wt.path)
            .map_err(|e| format!("cannot resolve repository for worktree removal: {e}"))?;
        git::remove_worktree(&repo, &wt.path, true)
            .map_err(|e| format!("git refused to remove the worktree: {e}"))?;
    }
    remove_container(pool, &wt.path)
}

fn remove_container(pool: &Path, wt: &Path) -> std::result::Result<(), String> {
    let container =
        removable_container(pool, wt).map_err(|e| format!("refusing unsafe cleanup path: {e}"))?;
    match std::fs::remove_dir_all(&container) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("could not remove worktree directory: {e}")),
    }
}

/// Why the repository a linked worktree points at is gone, or `None` when it
/// is there or the slot is not a linked git worktree.
fn backing_repository_missing(wt: &Path) -> Option<String> {
    let text = std::fs::read_to_string(wt.join(".git")).ok()?;
    let line = text.lines().next().unwrap_or_default().trim();
    let dir = line.strip_prefix("gitdir:")?.trim();
    if dir.is_empty() {
        return None;
    }
    let dir = Path::new(dir);
    let dir = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        wt.join(dir)
    };
    match std::fs::metadata(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Some(format!("gitdir {} does not exist", dir.display()))
        }
        _ => None,
    }
}

/// The directory to delete for a slot: its numbered slot directory when the
/// pool owns that (the built-in layout), else just the worktree. Decided on
/// canonical paths, so a slot that reaches outside the pool through a
/// symlink never widens the delete (treehouse's
/// `removableWorktreeContainer`).
fn removable_container(pool: &Path, wt: &Path) -> std::result::Result<PathBuf, String> {
    let wt = state::clean(wt);
    let container = wt
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("refusing to remove {}", wt.display()))?;
    let canon_pool = canonical_prefix(pool)?;
    let canon_container = canonical_prefix(&container)?;
    let owned = canon_container != canon_pool && canon_container.starts_with(&canon_pool);
    let target = if owned { container } else { wt };
    if target.parent().is_none() || target.components().all(|c| c == Component::RootDir) {
        return Err(format!("refusing to remove {}", target.display()));
    }
    Ok(target)
}

/// `p` with its longest existing prefix canonicalized.
fn canonical_prefix(p: &Path) -> std::result::Result<PathBuf, String> {
    let p = state::clean(p);
    let mut rest = Vec::new();
    let mut head = p.as_path();
    loop {
        match std::fs::canonicalize(head) {
            Ok(mut c) => {
                for part in rest.iter().rev() {
                    c.push(part);
                }
                return Ok(c);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let (Some(parent), Some(name)) = (head.parent(), head.file_name()) else {
                    return Err(format!("cannot resolve {}: {e}", p.display()));
                };
                rest.push(name.to_os_string());
                head = parent;
            }
            Err(e) => return Err(format!("cannot resolve {}: {e}", p.display())),
        }
    }
}

fn container_size(pool: &Path, wt: &Path) -> u64 {
    removable_container(pool, wt)
        .ok()
        .and_then(|c| dir_size(&c).ok())
        .unwrap_or(0)
}

/// Bytes under `root`, symlinks not followed.
fn dir_size(root: &Path) -> std::io::Result<u64> {
    let meta = std::fs::symlink_metadata(root)?;
    let mut total = meta.len();
    if meta.is_dir() {
        for entry in std::fs::read_dir(root)? {
            total += dir_size(&entry?.path())?;
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_is_the_slot_dir_inside_the_pool() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = tmp.path().join("pool");
        std::fs::create_dir_all(pool.join("1/repo")).unwrap();
        assert_eq!(
            removable_container(&pool, &pool.join("1/repo")).unwrap(),
            pool.join("1")
        );
        // A worktree placed elsewhere: only itself.
        let other = tmp.path().join("elsewhere/repo");
        std::fs::create_dir_all(&other).unwrap();
        assert_eq!(removable_container(&pool, &other).unwrap(), other);
        // Directly in the pool: the pool is never the container.
        assert_eq!(
            removable_container(&pool, &pool.join("repo")).unwrap(),
            pool.join("repo")
        );
        assert!(removable_container(&pool, Path::new("/")).is_err());
    }

    #[test]
    fn a_symlinked_slot_dir_is_not_owned() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = tmp.path().join("pool");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(outside.join("repo")).unwrap();
        std::fs::create_dir_all(&pool).unwrap();
        std::os::unix::fs::symlink(&outside, pool.join("1")).unwrap();
        // The slot dir resolves outside the pool: remove only the worktree.
        assert_eq!(
            removable_container(&pool, &pool.join("1/repo")).unwrap(),
            pool.join("1/repo")
        );
    }

    #[test]
    fn missing_backing_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        assert_eq!(backing_repository_missing(&wt), None);
        std::fs::write(wt.join(".git"), "gitdir: /nope/.git/worktrees/x\n").unwrap();
        assert!(backing_repository_missing(&wt).is_some());
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", tmp.path().display()),
        )
        .unwrap();
        assert_eq!(backing_repository_missing(&wt), None);
    }

    #[test]
    fn destroy_reasons_name_the_opt_in() {
        let mut t = Target {
            classes: Vec::new(),
            detail: String::new(),
        };
        t.add(Class::Leased, "held by quark:task:p:t");
        t.add(Class::Dirty, "uncommitted changes");
        assert_eq!(
            t.missing(DestroyOptions::default()),
            ["--include-leased", "--include-unlanded"]
        );
        assert_eq!(t.reason(), "leased: held by quark:task:p:t");
        assert!(t
            .missing(DestroyOptions {
                include_leased: true,
                include_unlanded: true,
                ..Default::default()
            })
            .is_empty());
    }
}
