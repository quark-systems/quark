//! The append-only event log every state change goes through first.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{EventId, HostId, ProjectId, Result, TaskId};

/// Position of an event in one log, starting at 1. `Seq(0)` means "before
/// the first event", so `read(Seq(0), ..)` reads from the start.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Seq(pub u64);

impl Seq {
    pub const ZERO: Seq = Seq(0);

    pub fn next(self) -> Seq {
        Seq(self.0 + 1)
    }
}

/// Dotted, lower-case event kind such as `task.transition` or
/// `worker.report`. Subsystems own their prefixes; see
/// `docs/engine/contracts.md`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventKind(pub String);

impl EventKind {
    pub fn new(kind: impl Into<String>) -> Self {
        Self(kind.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The subsystem prefix, the part before the first dot.
    pub fn prefix(&self) -> &str {
        self.0.split('.').next().unwrap_or("")
    }
}

impl From<&str> for EventKind {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Well-known kinds. Payload shapes are documented next to each constant.
pub mod kinds {
    /// A task changed state. Payload: [`crate::task::TaskEvent`].
    pub const TASK: &str = "task.transition";
    /// A worker message. Payload: [`crate::worker::WorkerEnvelope`].
    pub const WORKER: &str = "worker.message";
    /// A worktree handout, return or lease. Payload: [`crate::worktree::WorktreeEvent`].
    pub const WORKTREE: &str = "worktree.change";
    /// The shadow engine saw bash and native disagree. Payload:
    /// [`crate::slice::Divergence`].
    pub const SHADOW_DIVERGENCE: &str = "shadow.divergence";
    /// A slice changed mode. Payload: [`crate::slice::SliceChange`].
    pub const SLICE_MODE: &str = "slice.mode";
}

/// An event before the log assigns it a sequence number.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewEvent {
    pub id: EventId,
    #[serde(with = "time::serde::rfc3339")]
    pub ts: OffsetDateTime,
    pub host: HostId,
    pub project: ProjectId,
    pub task: Option<TaskId>,
    pub kind: EventKind,
    pub payload: serde_json::Value,
}

impl NewEvent {
    /// A new event stamped now with a fresh id.
    pub fn new(
        host: HostId,
        project: ProjectId,
        task: Option<TaskId>,
        kind: impl Into<EventKind>,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            id: EventId::new(),
            ts: OffsetDateTime::now_utc(),
            host,
            project,
            task,
            kind: kind.into(),
            payload,
        }
    }

    /// A new event whose payload is `value` serialized.
    pub fn typed<T: Serialize>(
        host: HostId,
        project: ProjectId,
        task: Option<TaskId>,
        kind: impl Into<EventKind>,
        value: &T,
    ) -> Result<Self> {
        let payload = serde_json::to_value(value)
            .map_err(|e| crate::CoreError::Invalid(format!("payload: {e}")))?;
        Ok(Self::new(host, project, task, kind, payload))
    }

    pub fn with_seq(self, seq: Seq) -> Event {
        Event {
            id: self.id,
            seq,
            ts: self.ts,
            host: self.host,
            project: self.project,
            task: self.task,
            kind: self.kind,
            payload: self.payload,
        }
    }
}

/// An event as stored in the log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    pub seq: Seq,
    #[serde(with = "time::serde::rfc3339")]
    pub ts: OffsetDateTime,
    pub host: HostId,
    pub project: ProjectId,
    pub task: Option<TaskId>,
    pub kind: EventKind,
    pub payload: serde_json::Value,
}

impl Event {
    /// The payload deserialized as `T`.
    pub fn decode<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_value(self.payload.clone())
            .map_err(|e| crate::CoreError::Invalid(format!("{} payload: {e}", self.kind.0)))
    }
}

/// Crash-safe, append-only, ordered log. The single source of truth: read
/// models are rebuilt from it by replay.
///
/// Implementations must make `append` durable before returning, keep `seq`
/// strictly increasing with no gaps, and treat an id already in the log as
/// success returning the original `seq` (idempotent append).
#[async_trait]
pub trait EventLog: Send + Sync {
    /// Append one event and return its sequence number.
    async fn append(&self, event: NewEvent) -> Result<Seq>;

    /// Up to `limit` events with `seq > after`, in order.
    async fn read(&self, after: Seq, limit: usize) -> Result<Vec<Event>>;

    /// The last assigned sequence number, `Seq::ZERO` when empty.
    async fn head(&self) -> Result<Seq>;

    /// Every event with `seq > after`, then each new one as it is appended.
    async fn subscribe(&self, after: Seq) -> Result<Box<dyn Subscription>>;
}

/// A live tail of an [`EventLog`].
#[async_trait]
pub trait Subscription: Send {
    /// The next event, waiting for one if needed. `None` once the log is
    /// closed.
    async fn next(&mut self) -> Option<Result<Event>>;
}
