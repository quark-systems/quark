//! The worker protocol: how a worker talks to the engine.
//!
//! Three transports carry the same four messages: quarkd's MCP tools
//! (`report`, `ask`, `learned`, `done`), harness hooks POSTing to quarkd,
//! and a file fallback for harnesses with neither. Every message lands in
//! the event log as a [`crate::event::kinds::WORKER`] event whose payload is
//! a [`WorkerEnvelope`].

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{EventId, Result, TaskId};

/// What a worker can say.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case")]
pub enum WorkerMessage {
    /// Progress or state the coordinator may need, such as `working`,
    /// `blocked` or `paused` with a note.
    Report { state: String, note: String },
    /// A question. `key` stays the same if the worker asks again.
    Ask { key: String, question: String },
    /// Something worth keeping in Project memory.
    Learned { fact: String },
    /// The deliverable is ready: a pull request, a branch or a report.
    Done {
        summary: String,
        pull_request: Option<String>,
    },
    /// A harness signal from hooks (`busy`, `turn_end`).
    Signal { signal: String },
}

/// Which transport a message came in on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Mcp,
    Hook,
    File,
}

/// Payload of a worker event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerEnvelope {
    pub task: TaskId,
    /// The worker generation (changes on relaunch), so a late message from a
    /// replaced worker can be told apart.
    pub generation: String,
    pub via: Transport,
    pub message: WorkerMessage,
}

/// Receives worker messages from any transport and records them.
///
/// `receive` must append the message to the event log before returning, so
/// a crash after `Ok` never loses it. A message from a replaced worker (a
/// generation other than the task's current one) is still recorded, then
/// answered with [`crate::CoreError::Refused`] so the old worker stops.
#[async_trait]
pub trait WorkerProtocol: Send + Sync {
    async fn receive(&self, envelope: WorkerEnvelope) -> Result<EventId>;
}
