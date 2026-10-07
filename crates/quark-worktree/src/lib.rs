//! Isolated git worktrees for tasks: the [`quark_core::WorktreeProvider`].
//!
//! [`TreehouseProvider`] leases worktrees from a [`Pool`]. Treehouse 3.1.2
//! ([`TreehouseCli`]) is the pool today, driven only through its
//! non-interactive commands. [`NativePool`] is slice 9's replacement: a
//! port that works on treehouse's own pools, so the two can take turns.
//! [`ShadowPool`] runs treehouse and asks the native pool what it would have
//! done, recording every disagreement, until slice 9 switches on.
//!
//! The provider, not the pool, owns the safety rules, so they hold whatever
//! pool sits underneath ([`checks`]):
//!
//! - the isolation assertion: never hand out a primary checkout, a
//!   subdirectory, another repository's worktree, or a tangled one;
//! - the tangle check: a primary checkout stranded on a feature branch;
//! - the landed-work check: never return a worktree with uncommitted or
//!   unpushed work, report it kept instead.

pub mod checks;
mod git;
pub mod native;
mod provider;
pub mod shadow;
pub mod treehouse;

pub use native::{AcquirePlan, NativePool, ReleasePlan};
pub use provider::{holder_label, parse_holder, TreehouseProvider};
pub use shadow::ShadowPool;
pub use treehouse::{LeaseInfo, Pool, PoolEntry, Release, TreehouseCli, TREEHOUSE_VERSION};
