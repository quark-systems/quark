//! The worktree pool behind quarkd's worktree providers (slice 9).
//!
//! The native pool ([`NativePool`]) is the default: a port of treehouse
//! [`TREEHOUSE_VERSION`] that works on treehouse's own pools, so nothing
//! needs migrating either way. [`ENV`] picks another:
//!
//! | `QUARK_WORKTREE_POOL` | Pool |
//! |---|---|
//! | unset, `native` | the native pool; treehouse is not needed |
//! | `treehouse` | treehouse from `PATH` (the fallback; needs treehouse 3.1.2+) |
//! | `shadow` | treehouse serves every call and the native pool is asked what it would have done; each disagreement is appended to the event log as a `shadow.divergence` with slice `worktree_pool` |
//!
//! [`LEGACY_SHADOW_ENV`]`=1` (the flag from before the switch) still means
//! `shadow` when [`ENV`] is unset.
//!
//! The native pool refuses the treehouse settings it does not support
//! (jj, `worktree_path`, `unique_leaf`, APFS sharing, `post_create` hooks,
//! `.worktreeinclude` seeding) with an error naming the setting and this
//! fallback. The shadow is only compared with treehouse 3.1.2 or a later
//! 3.x: an older one answers differently by design, so against one quarkd
//! says which version it found and runs the native pool alone.
//!
//! One pool serves the whole daemon ([`pool`] makes it once; [`shared`]
//! hands it out), and the same pool frees disk for dispatch ([`IdlePrune`])
//! and removes a deleted Project's slots ([`retire_workspace`]).

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use quark_core::{EventLog, HostId, Result};
use quark_dispatch::IdlePool;
use quark_worktree::{
    Cleanup, NativePool, Pool, ShadowPool, TreehouseCli, TreehouseProvider, TREEHOUSE_VERSION,
};

use crate::store::Store;

/// Which pool: `native` (default), `treehouse` or `shadow`.
pub const ENV: &str = "QUARK_WORKTREE_POOL";

/// `1` asks for `shadow` when [`ENV`] is unset.
pub const LEGACY_SHADOW_ENV: &str = "QUARK_NATIVE_WORKTREES";

/// The pool quarkd runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Native,
    Treehouse,
    Shadow,
}

impl Mode {
    /// From [`ENV`] and [`LEGACY_SHADOW_ENV`]; an unknown value is the
    /// default, with a warning.
    pub fn from_env() -> Self {
        Self::parse(
            std::env::var(ENV).ok().as_deref(),
            std::env::var(LEGACY_SHADOW_ENV).ok().as_deref(),
        )
    }

    pub(crate) fn parse(value: Option<&str>, legacy: Option<&str>) -> Self {
        match value.map(str::trim) {
            None | Some("") if legacy == Some("1") => Mode::Shadow,
            None | Some("") | Some("native") => Mode::Native,
            Some("treehouse") => Mode::Treehouse,
            Some("shadow") => Mode::Shadow,
            Some(other) => {
                tracing::warn!(
                    value = other,
                    "{ENV} is not native, treehouse or shadow; using the native worktree pool"
                );
                Mode::Native
            }
        }
    }
}

static POOL: OnceLock<Arc<dyn Pool>> = OnceLock::new();
static SHADOWING: OnceLock<bool> = OnceLock::new();

/// Whether the daemon's pool is the native one shadowing treehouse.
pub fn shadowing() -> bool {
    SHADOWING.get().copied().unwrap_or(false)
}

/// The daemon's pool, once [`pool`] made it.
pub fn shared() -> Option<Arc<dyn Pool>> {
    POOL.get().cloned()
}

/// The pool quarkd's worktree providers use, made on the first call (see
/// the module docs) and the same one after.
pub async fn pool(log: Arc<dyn EventLog>, host: HostId) -> Arc<dyn Pool> {
    if let Some(p) = shared() {
        return p;
    }
    let (made, shadow) = make(Mode::from_env(), log, host).await;
    let _ = SHADOWING.set(shadow);
    POOL.get_or_init(|| made).clone()
}

async fn make(mode: Mode, log: Arc<dyn EventLog>, host: HostId) -> (Arc<dyn Pool>, bool) {
    match mode {
        Mode::Native => {
            tracing::info!("worktrees come from the native pool");
            (Arc::new(NativePool::new()), false)
        }
        Mode::Treehouse => {
            let treehouse = TreehouseCli::new();
            match treehouse.check_version().await {
                Ok(version) => {
                    tracing::info!(treehouse = %version, "worktrees come from treehouse ({ENV}=treehouse)")
                }
                Err(e) => tracing::warn!(
                    error = %e,
                    "{ENV}=treehouse, but treehouse {TREEHOUSE_VERSION} or a later 3.x is not installed; worktrees will fail until it is"
                ),
            }
            (Arc::new(treehouse), false)
        }
        Mode::Shadow => match TreehouseCli::new().check_version().await {
            Ok(version) => {
                tracing::info!(
                    treehouse = %version,
                    "worktree pool in shadow: treehouse serves, the native pool is compared"
                );
                (
                    Arc::new(ShadowPool::treehouse().with_events(log, host)),
                    true,
                )
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "worktree pool shadow not started: it compares with treehouse {TREEHOUSE_VERSION} or a later 3.x; using the native pool alone"
                );
                (Arc::new(NativePool::new()), false)
            }
        },
    }
}

/// Every repo clone of every Project with a workspace.
fn project_clones(store: &Store) -> Vec<PathBuf> {
    let projects = store.list_projects().unwrap_or_default();
    let mut out: Vec<PathBuf> = projects
        .into_iter()
        .filter_map(|p| p.workspace_path)
        .flat_map(|root| crate::host_sources::clones(Path::new(&root)))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Dispatch's low-disk relief: prunes the pools of every Project's repos on
/// this host.
pub struct IdlePrune {
    pool: Arc<dyn Pool>,
    store: Arc<Store>,
    host: HostId,
}

impl IdlePrune {
    pub fn new(pool: Arc<dyn Pool>, store: Arc<Store>, host: HostId) -> Self {
        Self { pool, store, host }
    }
}

#[async_trait]
impl IdlePool for IdlePrune {
    async fn prune_idle(&self, host: &HostId) -> Result<String> {
        if host != &self.host {
            return Err(quark_core::CoreError::Unsupported(format!(
                "pruning worktrees on host {host}"
            )));
        }
        let store = self.store.clone();
        let repos = tokio::task::spawn_blocking(move || project_clones(&store))
            .await
            .unwrap_or_default();
        let provider = TreehouseProvider::new(self.pool.clone());
        for r in &repos {
            provider.add_repo(r);
        }
        let out = provider.prune().await;
        tracing::info!(summary = %out.summary(), kept = ?out.kept, "pruned idle worktrees");
        Ok(out.summary())
    }
}

/// Remove the pool slots of a Project's repo clones under `root`, before the
/// clones are deleted. Only disposable slots go; any other is logged and
/// left (it holds work, or is leased by something else).
pub async fn retire_workspace(root: &Path) -> Cleanup {
    let mut out = Cleanup::default();
    let Some(pool) = shared() else {
        return out;
    };
    let provider = TreehouseProvider::new(pool);
    for clone in crate::host_sources::clones(root) {
        match provider.retire_clone(&clone).await {
            Ok(c) => out.extend(c),
            Err(e) => {
                tracing::warn!(clone = %clone.display(), error = %e, "could not retire the clone's worktrees")
            }
        }
    }
    if !out.kept.is_empty() {
        tracing::warn!(kept = ?out.kept, "worktree slots left behind for a deleted Project");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes() {
        assert_eq!(Mode::parse(None, None), Mode::Native);
        assert_eq!(Mode::parse(Some("native"), Some("1")), Mode::Native);
        assert_eq!(Mode::parse(Some("treehouse"), None), Mode::Treehouse);
        assert_eq!(Mode::parse(Some(" shadow "), None), Mode::Shadow);
        assert_eq!(Mode::parse(None, Some("1")), Mode::Shadow);
        assert_eq!(Mode::parse(Some(""), Some("1")), Mode::Shadow);
        assert_eq!(Mode::parse(None, Some("0")), Mode::Native);
        assert_eq!(Mode::parse(Some("bogus"), None), Mode::Native);
    }
}
