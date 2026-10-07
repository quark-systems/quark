//! The dispatcher's events, under the `dispatch.` prefix.
//!
//! Every dispatch decision is one of these before it is acted on: what a
//! task asked for, how its agent was resolved and chosen, why it waits, and
//! where it was placed. Engine-wide events (pruning, host alerts) use the
//! engine project and no task.

use std::collections::BTreeMap;
use std::path::PathBuf;

use quark_core::host::Health;
use quark_core::isolation::{IsolationMode, Policy};
use quark_core::{EventKind, HostId};
use serde::{Deserialize, Serialize};

use crate::admission::Refusal;
use crate::resolve::Resolution;
use crate::rules::Profile;

pub const PREFIX: &str = "dispatch";

/// A task waiting for a worker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub title: String,
    /// The task's instructions; the classifier reads them too.
    pub brief: String,
    /// The primary checkout the worktree is cut from.
    pub repo: PathBuf,
    pub branch: String,
    #[serde(default)]
    pub base: Option<String>,
    pub isolation: IsolationMode,
    #[serde(default)]
    pub policy: Policy,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// An agent the caller already picked; the rules are not consulted.
    #[serde(default)]
    pub profile: Option<Profile>,
}

/// Who picked the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decider {
    /// The dispatch rules resolved clearly.
    Resolution,
    /// The coordinator (or the caller) picked.
    Coordinator,
}

/// The agent and account a task's worker starts with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Choice {
    pub profile: Profile,
    /// The account chosen from the profile's pool, when the harness has
    /// accounts.
    pub account: Option<String>,
    /// The account's environment.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub by: Decider,
}

/// Payload of every `dispatch.*` event; the kind is `dispatch.<type>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DispatchEvent {
    /// A task asked for a worker.
    Requested { request: Request },
    /// The dispatch rules were resolved for it.
    Resolved { resolution: Resolution },
    /// The coordinator must pick the agent (or fix the pool).
    Escalated { reason: String },
    /// The agent and account were chosen; the task waits for a host.
    Chosen { choice: Choice },
    /// No host can take it now. Recorded when the reason changes.
    Held {
        reason: String,
        #[serde(default)]
        refusals: Vec<Refusal>,
    },
    /// Admission placed it on a host.
    Placed {
        host: HostId,
        #[serde(default)]
        notes: Vec<String>,
    },
    /// The host's supervisor took the task.
    Spawned,
    /// The host's supervisor refused or failed to start it.
    SpawnFailed { reason: String },
    /// Taken back before it was placed.
    Withdrawn,
    /// Idle pool worktrees were pruned on a host short of disk.
    Pruned { host: HostId, outcome: String },
    /// A host has been unhealthy too long: a decision for a person.
    HostUnhealthy {
        host: HostId,
        health: Health,
        unhealthy_secs: u64,
    },
    /// A host reported unhealthy is healthy again.
    HostRecovered { host: HostId },
}

impl DispatchEvent {
    pub fn kind(&self) -> EventKind {
        let name = match self {
            DispatchEvent::Requested { .. } => "requested",
            DispatchEvent::Resolved { .. } => "resolved",
            DispatchEvent::Escalated { .. } => "escalated",
            DispatchEvent::Chosen { .. } => "chosen",
            DispatchEvent::Held { .. } => "held",
            DispatchEvent::Placed { .. } => "placed",
            DispatchEvent::Spawned => "spawned",
            DispatchEvent::SpawnFailed { .. } => "spawn_failed",
            DispatchEvent::Withdrawn => "withdrawn",
            DispatchEvent::Pruned { .. } => "pruned",
            DispatchEvent::HostUnhealthy { .. } => "host_unhealthy",
            DispatchEvent::HostRecovered { .. } => "host_recovered",
        };
        EventKind::new(format!("{PREFIX}.{name}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_matches_tag() {
        let e = DispatchEvent::SpawnFailed { reason: "x".into() };
        assert_eq!(e.kind().as_str(), "dispatch.spawn_failed");
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["type"], "spawn_failed");
        assert_eq!(serde_json::from_value::<DispatchEvent>(v).unwrap(), e);
    }
}
