//! The supervisor's own events, under the `supervisor.` prefix.
//!
//! Task state changes go through the [`quark_eventlog::TaskLedger`] as
//! `task.transition` events; these record everything else the supervisor
//! must remember across a crash: what a task was asked to do, which
//! worktree and session it holds, which generation is current, steering
//! messages and their delivery, and how far worker messages were handled.
//! [`crate::Fleet`] folds them back into the supervisor's view.

use std::collections::BTreeMap;
use std::path::PathBuf;

use quark_core::isolation::{IsolationMode, Policy};
use quark_core::session::SessionId;
use quark_core::worktree::{ReturnOutcome, Worktree};
use quark_core::{EventKind, Seq};
use serde::{Deserialize, Serialize};

/// The prefix every supervisor event kind starts with.
pub const PREFIX: &str = "supervisor";

/// What a task's workers are started with. Recorded once at spawn and
/// reused by every relaunch, so the log alone can restart a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assignment {
    pub title: String,
    /// The task's instructions, given to every worker generation.
    pub brief: String,
    /// The primary checkout the worktree is cut from.
    pub repo: PathBuf,
    pub branch: String,
    pub base: Option<String>,
    /// Harness manifest id or alias.
    pub harness: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub isolation: IsolationMode,
    /// Sandbox policy; the worktree and the worker's state directory are
    /// always added as writable.
    #[serde(default)]
    pub policy: Policy,
    /// Extra environment, such as the harness's account variable.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// Why a worker generation started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    /// The first worker on the task.
    Spawn,
    /// A person or the coordinator asked for a new worker.
    Relaunch,
    /// The supervisor replaced a worker whose session ended unexpectedly.
    Recover,
}

/// Payload of every `supervisor.*` event; the kind is `supervisor.<type>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SupervisorEvent {
    /// The task was given to the supervisor.
    Assigned { assignment: Assignment },
    /// The task holds this worktree until it is released.
    Worktree { worktree: Worktree },
    /// A worker generation started in a session.
    Launched {
        generation: String,
        session: SessionId,
        backend: String,
        cause: Cause,
        /// The generation's state directory (brief, status file).
        dir: PathBuf,
        /// The harness manifest the worker runs on, after any relaunch
        /// override.
        harness: String,
        model: Option<String>,
        effort: Option<String>,
    },
    /// A steering message for the task's worker, kept until delivered.
    Steer { id: String, text: String },
    /// A steering message reached the worker of `generation`.
    Delivered { id: String, generation: String },
    /// The session of `generation` ended.
    Exited {
        generation: String,
        code: Option<i32>,
    },
    /// The worker of `generation` has shown no activity for `idle_secs`.
    Stale { generation: String, idle_secs: u64 },
    /// Worker messages up to and including `through` have been turned into
    /// task transitions.
    Handled { through: Seq },
    /// The task's worktree was given back (or kept, with unlanded work).
    Released { outcome: ReturnOutcome },
}

impl SupervisorEvent {
    /// The event kind, `supervisor.<type>`.
    pub fn kind(&self) -> EventKind {
        let name = match self {
            SupervisorEvent::Assigned { .. } => "assigned",
            SupervisorEvent::Worktree { .. } => "worktree",
            SupervisorEvent::Launched { .. } => "launched",
            SupervisorEvent::Steer { .. } => "steer",
            SupervisorEvent::Delivered { .. } => "delivered",
            SupervisorEvent::Exited { .. } => "exited",
            SupervisorEvent::Stale { .. } => "stale",
            SupervisorEvent::Handled { .. } => "handled",
            SupervisorEvent::Released { .. } => "released",
        };
        EventKind::new(format!("{PREFIX}.{name}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_matches_tag() {
        let e = SupervisorEvent::Steer {
            id: "s".into(),
            text: "t".into(),
        };
        assert_eq!(e.kind().as_str(), "supervisor.steer");
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["type"], "steer");
        let back: SupervisorEvent = serde_json::from_value(v).unwrap();
        assert_eq!(back, e);
    }
}
