//! The verification workflow and the merge and dispatch guards.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{ProjectId, Result, TaskId};

/// One stage of the gate set, in the order they run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateStage {
    /// The repo's own checks (lint, format, typecheck, tests).
    RepoChecks,
    /// The `verify-<app>` journeys from the feature map.
    Journeys,
    /// Hidden tests the worker never sees.
    Holdout,
    /// Review and fix loop (no-mistakes folded in).
    ReviewFix,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageResult {
    pub stage: GateStage,
    pub passed: bool,
    pub summary: String,
}

/// The outcome of running the gates on one head commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    /// The commit the gates ran on.
    pub head: String,
    pub stages: Vec<StageResult>,
    /// Set when the branch could not be rebased onto main.
    pub conflict: Option<String>,
}

impl Verdict {
    pub fn passed(&self) -> bool {
        self.conflict.is_none() && !self.stages.is_empty() && self.stages.iter().all(|s| s.passed)
    }
}

/// A pull request (or local-only branch) under verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub project: ProjectId,
    pub task: TaskId,
    /// Pull request URL, or `None` for a local-only branch.
    pub pull_request: Option<String>,
    pub branch: String,
    /// The head the caller last saw; a merge on any other head is refused.
    pub head: String,
}

/// Whether the default branch is green.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MainHealth {
    Green,
    /// Names the failing check and the commit that broke it.
    Red {
        check: String,
        commit: String,
    },
    Unknown {
        reason: String,
    },
}

/// A guard's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Permit {
    Allow,
    /// Refused, with the reason a person reads.
    Deny {
        reason: String,
    },
}

impl Permit {
    pub fn allowed(&self) -> bool {
        matches!(self, Permit::Allow)
    }
}

/// Runs the gate set.
#[async_trait]
pub trait VerifyPipeline: Send + Sync {
    /// Run every stage on the change's current head.
    async fn verify(&self, change: &Change) -> Result<Verdict>;

    /// Rebase the change onto current main and re-run every stage on the
    /// rebased head. The verdict's `head` is the rebased commit.
    async fn rebase_and_reverify(&self, change: &Change) -> Result<Verdict>;
}

/// Decides whether a merge or a dispatch may go ahead.
///
/// Red-main rule: while main is red, `may_merge` allows only a verified
/// change that turns main green, and `may_dispatch` allows only the one
/// automatic fix-main task. Work already running is unaffected. Only a
/// person overrides.
#[async_trait]
pub trait MergeGuard: Send + Sync {
    async fn main_health(&self, project: &ProjectId) -> Result<MainHealth>;

    /// `verdict` must come from [`VerifyPipeline::rebase_and_reverify`] on
    /// `change.head`; any other head is a stale-head denial.
    async fn may_merge(&self, change: &Change, verdict: &Verdict) -> Result<Permit>;

    async fn may_dispatch(&self, project: &ProjectId, task: &TaskId) -> Result<Permit>;
}
