//! Isolated git worktrees for tasks.
//!
//! Treehouse 3.1.2 backs this first (`get`, `return`, `lease`,
//! `status --json`); a native pool replaces it in slice 9, shadowed against
//! it. Every handout, return and lease is mirrored into the event log as a
//! [`WorktreeEvent`].

use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{ProjectId, Result, TaskId};

/// What a caller wants a worktree for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeRequest {
    pub project: ProjectId,
    pub task: TaskId,
    /// The repo's primary checkout the worktree is cut from. Never handed
    /// out itself.
    pub repo: PathBuf,
    /// Branch to create in the worktree before any edit.
    pub branch: String,
    /// Commit-ish to start from; the default branch's tip when `None`.
    pub base: Option<String>,
}

/// A worktree held by a task or a lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worktree {
    /// Provider-assigned id, stable while the worktree exists.
    pub id: String,
    pub path: PathBuf,
    pub repo: PathBuf,
    pub branch: String,
    pub holder: Holder,
}

/// Who holds a worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Holder {
    Task {
        task: TaskId,
    },
    /// A durable lease, such as a sub-coordinator's home.
    Lease {
        owner: String,
    },
}

/// Landed-work check run before a worktree goes back to the pool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandedWork {
    pub uncommitted: bool,
    /// Commits on the branch that are on no remote and not on the default
    /// branch.
    pub unpushed_commits: u32,
}

impl LandedWork {
    pub fn is_landed(&self) -> bool {
        !self.uncommitted && self.unpushed_commits == 0
    }
}

/// How a return went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReturnOutcome {
    /// Back in the pool, cleaned.
    Returned,
    /// Kept because work has not landed; nothing was discarded.
    Kept { work: LandedWork },
}

/// One worktree's state as the provider reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotState {
    Idle,
    InUse,
    Dirty,
    Leased,
    /// Set aside after a crash until someone looks at it.
    Quarantined,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slot {
    pub path: PathBuf,
    pub repo: PathBuf,
    pub state: SlotState,
    pub holder: Option<Holder>,
}

/// Provider-wide status, for the Hosts view.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub slots: Vec<Slot>,
}

/// Payload of a [`crate::event::kinds::WORKTREE`] event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorktreeEvent {
    HandedOut { worktree: Worktree },
    Returned { id: String, outcome: ReturnOutcome },
    Leased { worktree: Worktree },
}

/// Hands out isolated worktrees and takes them back.
///
/// Invariants every implementation keeps:
/// - `get` and `lease` never return the repo's primary checkout
///   ([`crate::CoreError::Refused`]), and refuse a tangled checkout (a
///   worktree whose git dir points at another worktree).
/// - `return_worktree` runs the landed-work check first and never discards
///   uncommitted or unpushed work; it reports [`ReturnOutcome::Kept`].
#[async_trait]
pub trait WorktreeProvider: Send + Sync {
    /// A fresh worktree for a task, on a new branch.
    async fn get(&self, request: &WorktreeRequest) -> Result<Worktree>;

    /// Give a worktree back.
    async fn return_worktree(&self, id: &str) -> Result<ReturnOutcome>;

    /// A durable worktree for `owner` that survives restarts until returned.
    async fn lease(&self, request: &WorktreeRequest, owner: &str) -> Result<Worktree>;

    async fn status(&self) -> Result<ProviderStatus>;
}
