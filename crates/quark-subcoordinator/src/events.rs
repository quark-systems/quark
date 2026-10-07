//! The sub-coordinators' events, under the `subcoordinator.` prefix.
//!
//! Each event's project is the sub-coordinator's id. Together they are
//! everything the engine remembers about a sub-coordinator, folded back by
//! [`crate::Registry`].

use quark_core::host::Health;
use quark_core::session::SessionId;
use quark_core::worker::WorkerMessage;
use quark_core::EventKind;
use serde::{Deserialize, Serialize};

use crate::model::{Profile, Registration};

/// The prefix every sub-coordinator event kind starts with.
pub const PREFIX: &str = "subcoordinator";

/// Why an agent generation started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    /// The first launch, or a launch after the agent was stopped.
    Launch,
    /// A person or the coordinator asked for a fresh agent.
    Relaunch,
    /// The engine replaced an agent whose session ended unexpectedly.
    Recover,
}

/// What a message to the sub-coordinator carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    /// Text for the agent to read and act on.
    Steer,
    /// Work items moved into its queue; the body is JSON
    /// ([`crate::HandoffItem`] list).
    Handoff,
}

impl MessageKind {
    /// The inbox file suffix.
    pub fn suffix(self) -> &'static str {
        match self {
            MessageKind::Steer => "md",
            MessageKind::Handoff => "handoff.json",
        }
    }
}

/// A message from the parent, delivered as a file in the inbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub kind: MessageKind,
    pub body: String,
    /// The decision key this message answers, if it is an answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<String>,
}

/// Payload of every `subcoordinator.*` event; the kind is
/// `subcoordinator.<type>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SubEvent {
    /// The sub-coordinator exists, with its charter, placement, harness and
    /// projects.
    Registered { registration: Registration },
    /// Its home was created on its host and its projects cloned.
    Seeded,
    /// Inherited configuration was written into its home.
    Inherited { digest: String, files: Vec<String> },
    /// An agent generation started in `session`.
    Launched {
        generation: String,
        session: SessionId,
        cause: Cause,
        profile: Profile,
    },
    /// The session of `generation` ended or vanished.
    Exited {
        generation: String,
        code: Option<i32>,
        /// Whether the engine ended it on request (stop, relaunch, retire)
        /// rather than finding it gone.
        #[serde(default)]
        stopped: bool,
    },
    /// Its host's health changed.
    Health { health: Health },
    /// A message for the sub-coordinator, kept until it is delivered.
    Sent { message: Message },
    /// The message's file is in the inbox.
    Delivered { id: String },
    /// The agent moved the message to `inbox/handled`.
    Acknowledged { id: String },
    /// A line the sub-coordinator appended to its parent channel, from byte
    /// `offset` to `end` (past its newline), and what it says when it is a
    /// status line.
    Report {
        offset: u64,
        end: u64,
        line: String,
        message: Option<WorkerMessage>,
    },
    /// Stopped for good. The home stays on its host.
    Retired { reason: String, forced: bool },
}

impl SubEvent {
    /// The event kind, `subcoordinator.<type>`.
    pub fn kind(&self) -> EventKind {
        let name = match self {
            SubEvent::Registered { .. } => "registered",
            SubEvent::Seeded => "seeded",
            SubEvent::Inherited { .. } => "inherited",
            SubEvent::Launched { .. } => "launched",
            SubEvent::Exited { .. } => "exited",
            SubEvent::Health { .. } => "health",
            SubEvent::Sent { .. } => "sent",
            SubEvent::Delivered { .. } => "delivered",
            SubEvent::Acknowledged { .. } => "acknowledged",
            SubEvent::Report { .. } => "report",
            SubEvent::Retired { .. } => "retired",
        };
        EventKind::new(format!("{PREFIX}.{name}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_matches_tag() {
        let e = SubEvent::Delivered { id: "m".into() };
        assert_eq!(e.kind().as_str(), "subcoordinator.delivered");
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["type"], "delivered");
        assert_eq!(serde_json::from_value::<SubEvent>(v).unwrap(), e);
        let v = serde_json::to_value(SubEvent::Seeded).unwrap();
        assert_eq!(v["type"], "seeded");
    }
}
