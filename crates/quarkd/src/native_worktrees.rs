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

use std::sync::Arc;

use quark_core::{EventLog, HostId};
use quark_worktree::{Pool, ShadowPool, TreehouseCli};

/// Set to `1` to shadow treehouse with the native pool.
pub const ENV: &str = "QUARK_NATIVE_WORKTREES";

pub fn enabled() -> bool {
    std::env::var(ENV).is_ok_and(|v| v == "1")
}

/// The pool quarkd's worktree providers use: treehouse, shadowed by the
/// native pool when [`enabled`].
pub fn pool(log: Arc<dyn EventLog>, host: HostId) -> Arc<dyn Pool> {
    if enabled() {
        tracing::info!(
            "slice 9 (worktree pool) in shadow: comparing the native pool with treehouse"
        );
        Arc::new(ShadowPool::treehouse().with_events(log, host))
    } else {
        Arc::new(TreehouseCli::new())
    }
}
