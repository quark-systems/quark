//! Isolated git worktrees for tasks: the [`quark_core::WorktreeProvider`].
//!
//! [`TreehouseProvider`] leases worktrees from a [`Pool`]. Treehouse 3.1.2
//! ([`TreehouseCli`]) is the pool today, driven only through its
//! non-interactive commands; the native pool of slice 9 will implement
//! [`Pool`] too and be shadowed against it.
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
mod provider;
pub mod treehouse;

pub use provider::{holder_label, parse_holder, TreehouseProvider};
pub use treehouse::{LeaseInfo, Pool, PoolEntry, Release, TreehouseCli, TREEHOUSE_VERSION};
