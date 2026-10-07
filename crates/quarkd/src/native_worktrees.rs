//! The native worktree pool (slice 9) in shadow beside treehouse.
//!
//! Slice 9 switches on last, so treehouse keeps serving every worktree. With
//! [`ENV`] set to `1`, the pool behind quarkd's worktree providers (the Hosts
//! view's pool reports, and the native supervisor's worktrees when it runs)
//! becomes a [`ShadowPool`]: treehouse still answers every call, and the
//! native pool is asked what it would have done on the same pool. Each
//! disagreement is appended to the event log as a `shadow.divergence` with
//! slice `worktree_pool`. Nothing here creates, resets or returns a
//! worktree that treehouse would not.
//!
//! The native pool is a port of treehouse [`TREEHOUSE_VERSION`], so it is
//! only compared with that release or a later 3.x. An older treehouse
//! answers differently by design (2.x reports no checked-out branch, and
//! exits 0 where 3.x exits 3 on a dirty return), so every comparison with
//! it would be a version difference, not a native bug. Against one, the
//! shadow stays off and quarkd says which version it found.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use quark_core::{EventLog, HostId};
use quark_worktree::{Pool, ShadowPool, TreehouseCli, TREEHOUSE_VERSION};

/// Set to `1` to shadow treehouse with the native pool; unset, it follows
/// `QUARK_SHADOWS=all`.
pub const ENV: &str = "QUARK_NATIVE_WORKTREES";

pub fn enabled() -> bool {
    crate::shadows::opt_in(ENV)
}

static SHADOWING: AtomicBool = AtomicBool::new(false);

/// Whether a pool this daemon made is shadowed by the native pool: [`enabled`]
/// and the installed treehouse is one the native pool can be compared with.
pub fn shadowing() -> bool {
    SHADOWING.load(Ordering::Relaxed)
}

/// The pool quarkd's worktree providers use: treehouse, shadowed by the
/// native pool when [`enabled`] and the installed treehouse is
/// [`TREEHOUSE_VERSION`] or a later 3.x.
pub async fn pool(log: Arc<dyn EventLog>, host: HostId) -> Arc<dyn Pool> {
    let treehouse = TreehouseCli::new();
    if !enabled() {
        return Arc::new(treehouse);
    }
    match treehouse.check_version().await {
        Ok(version) => {
            tracing::info!(
                treehouse = %version,
                "slice 9 (worktree pool) in shadow: comparing the native pool with treehouse"
            );
            SHADOWING.store(true, Ordering::Relaxed);
            Arc::new(ShadowPool::treehouse().with_events(log, host))
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "slice 9 (worktree pool) shadow not started: the native pool is a port of treehouse {TREEHOUSE_VERSION}; install it to compare"
            );
            Arc::new(treehouse)
        }
    }
}
