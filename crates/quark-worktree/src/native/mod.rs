//! The native worktree pool (slice 9): a Rust port of the parts of
//! treehouse 3.1.2 Quark drives, on the same on-disk pool.
//!
//! [`NativePool`] implements [`Pool`] by reading and writing treehouse's own
//! state file under treehouse's own lock ([`state`]), with the same pool
//! directory ([`config`]), the same slot layout (`<pool>/<n>/<repo name>`)
//! and the same rules for which slot is reused, so either implementation
//! can pick up a pool the other left. That is what lets slice 9 switch from
//! treehouse to native, and back, with no migration.
//!
//! Ported: leased acquisition (`get --lease [--branch] [--base]`, fetch
//! first, reuse the first clean, idle, provably-landed slot of this clone,
//! else create one up to `max_trees`), conditional release (`return
//! --if-lease-id`, never `--force`: a dirty worktree is not returned), the
//! status listing, the fail-safe state reads (quarantine on a bad digest, a
//! corrupt file, or an unrecorded slot) and the quarantine holders for
//! failed acquisitions.
//!
//! Recovered slots (quarantined after a lost or untrusted state) are freed
//! under the lock before each operation when they are provably safe, as
//! treehouse does, except that one holding untracked files stays
//! quarantined: treehouse moves those into a backup folder first, which the
//! native pool does not do yet.
//!
//! Not ported yet; the native pool refuses rather than diverge: jj
//! workspaces, `worktree_path` templates, `unique_leaf`, APFS sharing,
//! `post_create` hooks and seeding ignored files from `.worktreeinclude`.
//! Also not ported: interactive (owner-process) acquisition, which only
//! firstmate's `treehouse get` subshell uses (its reservations are still
//! read and respected), `prune` and `destroy`.
//!
//! [`NativePool::plan_acquire`] and [`NativePool::plan_release`] answer what
//! the native pool *would* do without doing it; the shadow
//! ([`crate::shadow`]) compares them with what treehouse did.

pub mod config;
mod git;
pub mod procs;
pub mod state;

use std::cell::OnceCell;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use quark_core::{CoreError, Result};
use serde::Serialize;

use self::config::Config;
use self::state::{Entry, Lock, State, INCOMPLETE_HOLDER, RECOVERED_HOLDER};
use crate::treehouse::{LeaseInfo, Pool, PoolEntry, Release};

/// How long lingering processes get between SIGTERM and SIGKILL on release.
const TERMINATE_GRACE: Duration = Duration::from_secs(2);

/// What an acquisition would do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AcquirePlan {
    /// Reset and lease this existing slot.
    Reuse { path: PathBuf },
    /// Create a new slot here.
    Create { path: PathBuf },
    /// Refuse, with the reason.
    Refuse { reason: String },
}

/// What a conditional release would do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ReleasePlan {
    /// Reset the worktree and put it back in the pool.
    Return,
    /// Leave it and its lease exactly as they are (treehouse's exit 3).
    NotReturned { reason: String },
    /// Fail (unknown worktree, lease no longer matches, ...).
    Refuse { reason: String },
}

/// The [`Pool`] treehouse provides, implemented natively. See the module
/// docs for what it covers.
#[derive(Debug, Clone)]
pub struct NativePool {
    root: Option<PathBuf>,
    fetch: bool,
}

impl Default for NativePool {
    fn default() -> Self {
        Self::new()
    }
}

/// One repo's pool, resolved.
struct Ctx {
    repo: PathBuf,
    cfg: Config,
    pool: PathBuf,
}

/// Why slots were passed over, for the "pool is full" message.
#[derive(Default)]
struct Skipped {
    other_flavor: usize,
    other_clone: usize,
    unverified_clone: usize,
}

impl NativePool {
    /// Pools where treehouse keeps them by default.
    pub fn new() -> Self {
        Self {
            root: None,
            fetch: true,
        }
    }

    /// Keep pools under `root` (treehouse's `--root`).
    pub fn with_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.root = Some(root.into());
        self
    }

    /// Acquire from local refs only (treehouse's `--no-fetch`).
    pub fn without_fetch(mut self) -> Self {
        self.fetch = false;
        self
    }

    fn ctx(&self, repo: &Path) -> Result<Ctx> {
        let cfg = Config::load(repo)?;
        let pool = config::pool_dir(repo, self.root.as_deref(), &cfg)?;
        Ok(Ctx {
            repo: repo.to_path_buf(),
            cfg,
            pool,
        })
    }

    /// The pool directory of the primary checkout `repo`.
    pub fn pool_dir(&self, repo: &Path) -> Result<PathBuf> {
        Ok(self.ctx(repo)?.pool)
    }

    // ---- acquire --------------------------------------------------------

    /// Checks that run before the state lock: config, branch name, base.
    /// Returns the explicitly requested base (recorded with the slot) and
    /// the base actually used.
    fn prepare(
        &self,
        ctx: &Ctx,
        branch: Option<&str>,
        base: Option<&str>,
        fetch: bool,
    ) -> Result<(String, String)> {
        if let Some(why) = ctx.cfg.unsupported(&ctx.repo) {
            return Err(CoreError::Unsupported(why));
        }
        if let Some(b) = branch {
            git::validate_branch_name(&ctx.repo, b)?;
        }
        if fetch {
            git::fetch(&ctx.repo).map_err(|e| CoreError::Backend(format!("fetch failed: {e}")))?;
        }
        let requested = base
            .map(str::to_string)
            .unwrap_or(ctx.cfg.base_branch.clone());
        let resolved = if requested.is_empty() {
            git::default_branch(&ctx.repo)?
        } else {
            git::verify_base_branch(&ctx.repo, &requested)?;
            requested.clone()
        };
        if git::has_worktree_include(&ctx.repo, &git::branch_ref(&ctx.repo, &resolved)) {
            return Err(CoreError::Unsupported(
                "the native worktree pool does not seed .worktreeinclude files yet".into(),
            ));
        }
        if let Some(b) = branch {
            if git::local_branch_exists(&ctx.repo, b)? {
                return Err(CoreError::Invalid(format!("branch {b:?} already exists")));
            }
        }
        Ok((requested, resolved))
    }

    /// Whether slot `wt` may be reset to `base` and handed out, with the
    /// commit to reset to and the HEAD that was checked.
    fn reusable(
        wt: &Entry,
        identity: &Option<(u64, u64)>,
        base: &str,
        skipped: &mut Skipped,
        processes: &OnceCell<procs::Snapshot>,
    ) -> Option<(String, String)> {
        if wt.destroying || wt.leased || procs::owner_alive(wt.owner_pid, wt.owner_started_at) {
            return None;
        }
        match state::marker(&wt.path) {
            Ok(Some("git")) => {}
            Ok(Some(_)) => {
                skipped.other_flavor += 1;
                return None;
            }
            _ => return None,
        }
        let Some(mine) = identity else {
            skipped.unverified_clone += 1;
            return None;
        };
        match clone_identity(&wt.path) {
            None => {
                skipped.unverified_clone += 1;
                return None;
            }
            Some(theirs) if theirs != *mine => {
                skipped.other_clone += 1;
                return None;
            }
            Some(_) => {}
        }
        // Seeded ignored files would need cleaning before reuse.
        if !wt.seeded_paths.is_empty() {
            return None;
        }
        if !processes
            .get_or_init(procs::Snapshot::take)
            .in_worktree(&wt.path)
            .is_empty()
        {
            return None;
        }
        if git::is_dirty(&wt.path).unwrap_or(true) {
            return None;
        }
        let (safe, target, head) = git::is_safe_to_reset(&wt.path, base).ok()?;
        if !safe && !merged_into_recorded_base(wt, base, &head) {
            return None;
        }
        Some((target, head))
    }

    fn full_error(state: &State, max: usize, skipped: &Skipped, identity_ok: bool) -> CoreError {
        let n = state.worktrees.len();
        let msg = if skipped.other_flavor > 0 {
            format!(
                "all {n} worktrees are in use, dirty, or hold the other backend's worktrees ({} jj-flavored; the repository selects git). Run 'treehouse status' to see details, destroy old-flavor worktrees to migrate the pool, or increase max_trees in treehouse.toml",
                skipped.other_flavor
            )
        } else if skipped.other_clone > 0 || skipped.unverified_clone > 0 {
            let mut m = format!(
                "all {n} worktrees are in use, dirty, or not provably this clone's ({} belong to another clone; {} whose clone identity cannot be verified; max_trees = {max}). A worktree is reused only by the clone it belongs to",
                skipped.other_clone, skipped.unverified_clone
            );
            if !identity_ok {
                m.push_str(", and this repository's clone identity cannot be verified");
            }
            m + ". Run 'treehouse status' to see details, or increase max_trees in treehouse.toml"
        } else {
            format!(
                "all {n} worktrees are in use or dirty (max_trees = {max}). Run 'treehouse status' to see details, or increase max_trees in treehouse.toml"
            )
        };
        CoreError::Refused(msg)
    }

    fn new_slot(ctx: &Ctx, state: &State) -> (String, PathBuf) {
        let name = next_slot(state).to_string();
        let leaf = ctx.repo.file_name().unwrap_or_default();
        let path = ctx.pool.join(&name).join(leaf);
        (name, path)
    }

    /// What [`Pool::acquire`] would do now, without fetching or changing
    /// anything.
    pub fn plan_acquire(
        &self,
        repo: &Path,
        branch: Option<&str>,
        base: Option<&str>,
    ) -> Result<AcquirePlan> {
        let ctx = self.ctx(repo)?;
        let (_, resolved) = match self.prepare(&ctx, branch, base, false) {
            Ok(b) => b,
            Err(e) => {
                return Ok(AcquirePlan::Refuse {
                    reason: e.to_string(),
                })
            }
        };
        let identity = clone_identity(repo);
        // No pool yet means an empty one; a plan never creates it.
        let _lock = match ctx.pool.exists() {
            true => Some(Lock::take(&ctx.pool)?),
            false => None,
        };
        let state = heal(state::read(&ctx.pool)?);
        let mut skipped = Skipped::default();
        let processes = OnceCell::new();
        for wt in &state.worktrees {
            if Self::reusable(wt, &identity, &resolved, &mut skipped, &processes).is_some() {
                return Ok(AcquirePlan::Reuse {
                    path: wt.path.clone(),
                });
            }
        }
        if state.worktrees.len() >= ctx.cfg.max_trees() {
            let e = Self::full_error(&state, ctx.cfg.max_trees(), &skipped, identity.is_some());
            return Ok(AcquirePlan::Refuse {
                reason: e.to_string(),
            });
        }
        Ok(AcquirePlan::Create {
            path: Self::new_slot(&ctx, &state).1,
        })
    }

    fn acquire_blocking(
        &self,
        repo: &Path,
        holder: &str,
        branch: Option<&str>,
        base: Option<&str>,
    ) -> Result<LeaseInfo> {
        let ctx = self.ctx(repo)?;
        if let Some(root) = ctx.pool.parent() {
            config::ensure_excluded(root);
        }
        let (requested, resolved) = self.prepare(&ctx, branch, base, self.fetch)?;
        let identity = clone_identity(repo);

        let _lock = locked(&ctx.pool)?;
        let mut state = heal(state::read(&ctx.pool)?);

        let mut skipped = Skipped::default();
        let processes = OnceCell::new();
        for i in 0..state.worktrees.len() {
            let Some((target, head)) = Self::reusable(
                &state.worktrees[i],
                &identity,
                &resolved,
                &mut skipped,
                &processes,
            ) else {
                continue;
            };
            let path = state.worktrees[i].path.clone();
            if git::reset_to_ref(&path, &target, &head, true).is_err() {
                continue;
            }
            let wt = &mut state.worktrees[i];
            wt.base_branch = requested.clone();
            wt.seed_inventory_known = false;
            wt.quarantine(INCOMPLETE_HOLDER);
            state::write(&ctx.pool, state.clone())?;
            state.worktrees[i].set_seed_inventory_empty();
            if let Some(b) = branch {
                if let Err(f) = git::create_branch(&path, b) {
                    let wt = &mut state.worktrees[i];
                    wt.owner_pid = 0;
                    wt.owner_started_at = 0;
                    let clean = matches!(git::checked_out_branch(&path), Ok(None))
                        && matches!(git::is_dirty(&path), Ok(false));
                    if f.created {
                        wt.quarantine("quarantined: branch checkout failed");
                    } else if clean {
                        wt.clear_lease();
                    } else {
                        wt.quarantine("quarantined: branch creation cleanup failed");
                    }
                    state::write(&ctx.pool, state)?;
                    return Err(CoreError::Backend(format!(
                        "failed to create branch {b:?} in {}: {}",
                        path.display(),
                        f.error
                    )));
                }
            }
            let wt = &mut state.worktrees[i];
            wt.clear_lease();
            wt.lease(holder)?;
            let info = lease_info(wt, &resolved);
            state::write(&ctx.pool, state)?;
            return Ok(info);
        }

        let max = ctx.cfg.max_trees();
        if state.worktrees.len() >= max {
            return Err(Self::full_error(&state, max, &skipped, identity.is_some()));
        }
        let (name, path) = Self::new_slot(&ctx, &state);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CoreError::Backend(format!("creating {}: {e}", parent.display())))?;
        }
        let _ = git::prune_worktrees(&ctx.repo);
        let expected = match branch {
            Some(b) => Some(git::branch_commit(&ctx.repo, &resolved).map_err(|e| {
                CoreError::Backend(format!(
                    "failed to resolve base commit for branch {b:?}: {e}"
                ))
            })?),
            None => None,
        };
        git::add_worktree(&ctx.repo, &path, &resolved)
            .map_err(|e| CoreError::Backend(format!("failed to create worktree: {e}")))?;
        state.worktrees.push(Entry::new(&name, &path, &requested));
        let i = state.worktrees.len() - 1;
        state::write(&ctx.pool, state.clone())?;
        if let (Some(b), Some(expected)) = (branch, expected) {
            if !git::worktree_at_commit(&path, &expected).unwrap_or(false) {
                state.worktrees[i].quarantine("quarantined: worktree-add checkout changed HEAD");
                state::write(&ctx.pool, state)?;
                return Err(CoreError::Backend(format!(
                    "cannot create branch {b:?} in {}: worktree is not detached at selected base commit {expected} (worktree quarantined for inspection)",
                    path.display()
                )));
            }
            if let Err(f) = git::create_branch(&path, b) {
                let holder = if f.created {
                    "quarantined: branch checkout failed"
                } else {
                    "quarantined: branch creation cleanup failed"
                };
                state.worktrees[i].quarantine(holder);
                state::write(&ctx.pool, state)?;
                return Err(CoreError::Backend(format!(
                    "failed to create branch {b:?} in {}: {} (worktree quarantined for inspection)",
                    path.display(),
                    f.error
                )));
            }
        }
        let wt = &mut state.worktrees[i];
        wt.clear_lease();
        wt.lease(holder)?;
        let info = lease_info(wt, &resolved);
        state::write(&ctx.pool, state)?;
        Ok(info)
    }

    // ---- release --------------------------------------------------------

    /// The pool holding `path`: the one it sits in when that pool records
    /// it, else `repo`'s.
    fn pool_of(&self, repo: &Path, path: &Path) -> Result<PathBuf> {
        if let Some(pool) = path.parent().and_then(Path::parent) {
            if state::is_pool_dir(pool) {
                let s = state::read(pool)?;
                if s.worktrees.iter().any(|w| same_path(&w.path, path)) {
                    return Ok(pool.to_path_buf());
                }
            }
        }
        self.pool_dir(repo)
    }

    fn releasable<'a>(state: &'a mut State, path: &Path, lease_id: &str) -> Result<&'a mut Entry> {
        let wt = state
            .worktrees
            .iter_mut()
            .find(|w| same_path(&w.path, path))
            .ok_or_else(|| {
                CoreError::NotFound(format!(
                    "worktree {} is not managed by treehouse",
                    path.display()
                ))
            })?;
        if wt.destroying {
            return Err(CoreError::Refused(format!(
                "worktree {} is being destroyed",
                path.display()
            )));
        }
        if !wt.leased {
            return Err(CoreError::Refused(format!(
                "lease precondition failed: worktree {} is not leased",
                path.display()
            )));
        }
        if wt.lease_id != lease_id {
            return Err(CoreError::Refused(format!(
                "lease precondition failed: lease identity does not match worktree {}",
                path.display()
            )));
        }
        if !wt.seeded_paths.is_empty() {
            return Err(CoreError::Unsupported(
                "the native worktree pool cannot clean seeded .worktreeinclude files yet".into(),
            ));
        }
        Ok(wt)
    }

    fn dirty_reason(path: &Path) -> Option<String> {
        let marked = matches!(state::marker(path), Ok(Some(_)));
        (marked && git::is_dirty(path).unwrap_or(false)).then(|| {
            format!(
                "worktree not returned: it has uncommitted changes and the confirmation could not be answered (stdin reached EOF); prune will not reclaim this slot. Use treehouse return --force '{}' to clean and return it",
                path.display()
            )
        })
    }

    /// What [`Pool::release`] would do now, without doing it.
    pub fn plan_release(&self, repo: &Path, path: &Path, lease_id: &str) -> Result<ReleasePlan> {
        let pool = self.pool_of(repo, path)?;
        let mut state = state::read(&pool)?;
        if let Err(e) = Self::releasable(&mut state, path, lease_id) {
            return Ok(ReleasePlan::Refuse {
                reason: e.to_string(),
            });
        }
        Ok(match Self::dirty_reason(path) {
            Some(reason) => ReleasePlan::NotReturned { reason },
            None => ReleasePlan::Return,
        })
    }

    fn release_blocking(&self, repo: &Path, path: &Path, lease_id: &str) -> Result<Release> {
        let pool = self.pool_of(repo, path)?;
        {
            let _lock = locked(&pool)?;
            Self::releasable(&mut state::read(&pool)?, path, lease_id)?;
        }
        if let Some(reason) = Self::dirty_reason(path) {
            return Ok(Release::NotReturned(reason));
        }

        let markerless = !matches!(state::marker(path), Ok(Some(_)));
        let default = if markerless {
            Ok(String::new())
        } else {
            git::default_branch_for_worktree(path)
        };
        let configured = if markerless {
            String::new()
        } else {
            configured_base(path)
        };

        let _lock = locked(&pool)?;
        let mut state = state::read(&pool)?;
        let wt = Self::releasable(&mut state, path, lease_id)?;
        if !markerless {
            let requested = if configured.is_empty() {
                wt.base_branch.clone()
            } else {
                configured
            };
            let fallback = match (&default, requested.is_empty()) {
                (Ok(d), _) => d.clone(),
                (Err(e), true) => return Err(CoreError::Backend(e.to_string())),
                (Err(_), false) => String::new(),
            };
            let branch = if requested.is_empty() {
                fallback.clone()
            } else {
                requested.clone()
            };

            git::detach(path)
                .map_err(|e| CoreError::Backend(format!("failed to detach worktree HEAD: {e}")))?;
            stop_processes(path)?;
            let mut recorded = requested;
            if let Err(e) = git::reset_worktree(path, &branch) {
                if fallback.is_empty() || fallback == branch {
                    return Err(e);
                }
                git::reset_worktree(path, &fallback)?;
                recorded = String::new();
            }
            wt.base_branch = recorded;
        } else {
            stop_processes(path)?;
        }
        wt.release();
        state::write(&pool, state)?;
        Ok(Release::Returned)
    }

    // ---- status ---------------------------------------------------------

    /// Every slot of `repo`'s pool as `treehouse status --json` reports it.
    /// With `write_back`, the healed state is written as treehouse does;
    /// without, nothing on disk changes (for the shadow).
    pub fn list_blocking(&self, repo: &Path, write_back: bool) -> Result<Vec<PoolEntry>> {
        let ctx = self.ctx(repo)?;
        if !ctx.pool.exists() && !write_back {
            return Ok(Vec::new());
        }
        let _lock = if write_back {
            locked(&ctx.pool)?
        } else {
            Lock::take(&ctx.pool)?
        };
        let state = heal(state::read(&ctx.pool)?);
        if write_back {
            state::write(&ctx.pool, state.clone())?;
        }
        let cwd = std::env::current_dir().unwrap_or_default();
        let processes = procs::Snapshot::take();
        Ok(state
            .worktrees
            .iter()
            .filter(|w| !w.destroying)
            .map(|w| status_of(w, &cwd, &processes))
            .collect())
    }
}

fn status_of(wt: &Entry, cwd: &Path, processes: &procs::Snapshot) -> PoolEntry {
    let flavor = state::marker(&wt.path).ok().flatten();
    let owned = procs::owner_alive(wt.owner_pid, wt.owner_started_at);
    let mut status = if wt.leased {
        "leased"
    } else if owned || !processes.in_worktree(&wt.path).is_empty() {
        "in-use"
    } else if flavor.is_none() {
        "damaged"
    } else if git::is_dirty(&wt.path).unwrap_or(false) {
        "dirty"
    } else {
        "available"
    };
    if !wt.leased && !owned && status != "damaged" && procs::within(&wt.path, cwd) {
        status = "you're here";
    }
    if !wt.recovery_error.is_empty() {
        status = "damaged";
    }
    let (branch, detached) = match flavor {
        Some("git") => match git::checked_out_branch(&wt.path) {
            Ok(Some(b)) => (b, false),
            Ok(None) => (String::new(), true),
            Err(_) => (String::new(), false),
        },
        _ => (String::new(), false),
    };
    let leased = wt.leased;
    PoolEntry {
        name: wt.name.clone(),
        path: wt.path.clone(),
        status: status.into(),
        branch,
        detached,
        recovery_reason: wt.recovery_reason.clone(),
        lease_id: if leased {
            wt.lease_id.clone()
        } else {
            String::new()
        },
        lease_holder: if leased {
            wt.lease_holder.clone()
        } else {
            String::new()
        },
    }
}

/// The pool's lock, after freeing recovered slots proven safe (what
/// treehouse does under its lock before every operation).
fn locked(pool: &Path) -> Result<Lock> {
    let lock = Lock::take(pool)?;
    let Ok(mut state) = state::read(pool) else {
        return Ok(lock);
    };
    let mut changed = false;
    for wt in &mut state.worktrees {
        if !wt.leased || wt.lease_holder != RECOVERED_HOLDER || std::fs::metadata(&wt.path).is_err()
        {
            continue;
        }
        match unsafe_to_free(wt) {
            None => {
                wt.release();
                changed = true;
            }
            Some(reason) if reason != wt.recovery_reason => {
                wt.recovery_reason = reason;
                changed = true;
            }
            Some(_) => {}
        }
    }
    if changed {
        state::write(pool, state)?;
    }
    Ok(lock)
}

/// Why a recovered slot must stay quarantined, or `None` when nothing uses
/// it, it holds no edits and its HEAD is safely reachable.
fn unsafe_to_free(wt: &Entry) -> Option<String> {
    if !wt.recovery_error.is_empty() {
        return Some(
            "its VCS marker could not be read, so it cannot be verified automatically".into(),
        );
    }
    if !matches!(state::marker(&wt.path), Ok(Some("git"))) {
        return Some("automatic safety verification is only available for Git worktrees".into());
    }
    if procs::owner_alive(wt.owner_pid, wt.owner_started_at)
        || !procs::in_worktree(&wt.path).is_empty()
    {
        return Some("a process is using this worktree (a shell standing in it counts); stop it or leave the worktree".into());
    }
    let untracked = match git::recovery_worktree(&wt.path) {
        Ok(u) => u,
        Err(reason) => return Some(reason),
    };
    let base = if wt.base_branch.is_empty() {
        git::default_branch_for_worktree(&wt.path).unwrap_or_default()
    } else {
        wt.base_branch.clone()
    };
    if !git::recovery_head_contained(&wt.path, &base) {
        return Some("HEAD is not contained in a remote-tracking ref or the slot's base branch; push or preserve it".into());
    }
    if !untracked.is_empty() {
        return Some("it holds untracked files, which the native pool does not back up yet; check them and return the worktree by name".into());
    }
    None
}

/// Drop entries whose worktree is gone; clear reservations of dead owners.
fn heal(mut state: State) -> State {
    state
        .worktrees
        .retain(|w| std::fs::metadata(&w.path).is_ok());
    for w in &mut state.worktrees {
        if w.owner_pid != 0 && !procs::owner_alive(w.owner_pid, w.owner_started_at) {
            w.owner_pid = 0;
            w.owner_started_at = 0;
            w.destroying = false;
        }
    }
    state
}

fn next_slot(state: &State) -> u64 {
    state
        .worktrees
        .iter()
        .filter_map(|w| w.name.parse::<u64>().ok())
        .max()
        .unwrap_or(0)
        + 1
}

fn lease_info(wt: &Entry, base: &str) -> LeaseInfo {
    LeaseInfo {
        path: wt.path.clone(),
        lease_id: wt.lease_id.clone(),
        lease_holder: wt.lease_holder.clone(),
        base_branch: base.to_string(),
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    state::clean(a) == state::clean(b)
}

/// The physical identity (device, inode) of the clone `dir` belongs to:
/// its common git dir, after symlinks.
fn clone_identity(dir: &Path) -> Option<(u64, u64)> {
    let common = git::common_git_dir(dir).ok()?;
    let m = std::fs::metadata(common).ok()?;
    Some((m.dev(), m.ino()))
}

/// A slot whose HEAD is not merged into the requested base may still hold
/// nothing beyond the base it was parked on (another acquisition's base, or
/// the default for an inferred one); then it is as disposable.
fn merged_into_recorded_base(wt: &Entry, requested: &str, head: &str) -> bool {
    let base = if wt.base_branch.is_empty() {
        match git::default_branch_for_worktree(&wt.path) {
            Ok(b) => b,
            Err(_) => return false,
        }
    } else {
        wt.base_branch.clone()
    };
    if base == requested {
        return false;
    }
    matches!(git::is_safe_to_reset(&wt.path, &base), Ok((true, _, h)) if h == head)
}

/// `base_branch` from the config of the repository `wt` belongs to, when
/// it still names a branch.
fn configured_base(wt: &Path) -> String {
    let Ok(common) = git::common_git_dir(wt) else {
        return String::new();
    };
    let repo = match common.file_name() {
        Some(n) if n == ".git" => common.parent().map(Path::to_path_buf),
        _ => None,
    };
    let Some(repo) = repo else {
        return String::new();
    };
    match Config::load(&repo) {
        Ok(cfg) if !cfg.base_branch.is_empty() && git::branch_exists(&repo, &cfg.base_branch) => {
            cfg.base_branch
        }
        _ => String::new(),
    }
}

/// Stop whatever still runs in `path`, refusing when something survives.
fn stop_processes(path: &Path) -> Result<()> {
    let (_, left) = procs::terminate(path, TERMINATE_GRACE);
    if left.is_empty() {
        return Ok(());
    }
    let names: Vec<String> = left
        .iter()
        .map(|p| format!("{} ({})", p.name, p.pid))
        .collect();
    Err(CoreError::Refused(format!(
        "processes still running in {}: {}",
        path.display(),
        names.join(", ")
    )))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| CoreError::Backend(format!("native pool task: {e}")))?
}

#[async_trait]
impl Pool for NativePool {
    async fn acquire(
        &self,
        repo: &Path,
        holder: &str,
        branch: Option<&str>,
        base: Option<&str>,
    ) -> Result<LeaseInfo> {
        let (me, repo, holder) = (self.clone(), repo.to_path_buf(), holder.to_string());
        let (branch, base) = (branch.map(str::to_string), base.map(str::to_string));
        blocking(move || me.acquire_blocking(&repo, &holder, branch.as_deref(), base.as_deref()))
            .await
    }

    async fn release(&self, repo: &Path, path: &Path, lease_id: &str) -> Result<Release> {
        let (me, repo, path, id) = (
            self.clone(),
            repo.to_path_buf(),
            path.to_path_buf(),
            lease_id.to_string(),
        );
        blocking(move || me.release_blocking(&repo, &path, &id)).await
    }

    async fn list(&self, repo: &Path) -> Result<Vec<PoolEntry>> {
        let (me, repo) = (self.clone(), repo.to_path_buf());
        blocking(move || me.list_blocking(&repo, true)).await
    }
}
