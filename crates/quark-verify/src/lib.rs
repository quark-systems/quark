//! The native engine's verification workflow and merge guard (slice 2).
//!
//! - [`NativePipeline`]: the [`quark_core::VerifyPipeline`]. Runs a Project's
//!   gates (checks, journeys, holdout) on a head in a scratch worktree, after
//!   rebasing onto main when re-verifying.
//! - [`NativeGuard`]: the [`quark_core::MergeGuard`]. Refuses a stale head,
//!   keeps red-main episodes in the event log, and while main is red allows
//!   only a change that turns it green and one fix-main task.
//! - [`GuardedMerge`]: the native guarded merge. Rebases and re-verifies,
//!   asks the guard, then merges exactly the verified head; also keeps
//!   standing approval.
//! - [`rules`]: the guardrail as pure functions, shared by both of the above.
//!
//! `docs/engine/verify.md` describes the rules and the events.

pub mod forge;
mod guard;
pub mod health;
mod merge;
mod pipeline;
pub mod rules;

pub use forge::{FakeForge, Forge, GhForge, MergeMethod, PullHead, PullState};
pub use guard::{EpisodeChange, GuardDecision, NativeGuard, Override, Target};
pub use merge::{GuardedMerge, LandRequest, Landing, Merged, Outcome, StandingApproval};
pub use pipeline::{
    check_log, journeys_log_prefix, logs_dir, CheckGate, GatesConfig, HoldoutGate, JourneysGate,
    NativePipeline, PipelineTarget, RepoGates,
};

/// Event kinds this crate owns (prefix `verify`).
pub mod kinds {
    /// A red-main episode opened, changed or closed. Payload:
    /// [`crate::EpisodeChange`].
    pub const RED_MAIN: &str = "verify.red_main";
    /// A person answered an episode's decision. Payload: [`crate::Override`].
    pub const OVERRIDE: &str = "verify.override";
    /// A native merge or dispatch decision. Payload: [`crate::GuardDecision`].
    pub const DECISION: &str = "verify.decision";
    /// A native guarded merge landed a pull request. Payload:
    /// [`crate::Merged`].
    pub const MERGED: &str = "verify.merged";
    /// Standing approval was set for a repo. Payload:
    /// [`crate::StandingApproval`].
    pub const STANDING_APPROVAL: &str = "verify.standing_approval";
}
