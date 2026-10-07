//! The typed event stream.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Every event type the stream can carry. The daemon emits the project, task
/// and decision events; the rest are reserved so clients can be generated now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
pub enum EventType {
    #[serde(rename = "project.updated")]
    ProjectUpdated,
    #[serde(rename = "task.created")]
    TaskCreated,
    #[serde(rename = "task.state_changed")]
    TaskStateChanged,
    /// A new entry in a task's activity log; payload is a [`TaskEvent`](crate::TaskEvent).
    #[serde(rename = "task.event")]
    TaskEvent,
    #[serde(rename = "coordinator.message")]
    CoordinatorMessage,
    #[serde(rename = "worker.transcript")]
    WorkerTranscript,
    #[serde(rename = "worker.output")]
    WorkerOutput,
    #[serde(rename = "decision.opened")]
    DecisionOpened,
    #[serde(rename = "decision.answered")]
    DecisionAnswered,
    /// A pull request appeared or changed; payload is a [`PullRequest`](crate::PullRequest)(crate::PullRequest).
    #[serde(rename = "pr.updated")]
    PrUpdated,
    /// A check appeared or changed; payload is a [`CheckUpdated`](crate::CheckUpdated).
    #[serde(rename = "check.updated")]
    CheckUpdated,
    /// A review appeared or changed; payload is a [`ReviewUpdated`](crate::ReviewUpdated).
    #[serde(rename = "review.updated")]
    ReviewUpdated,
    /// A worker was dispatched; payload is a [`DispatchRecord`](crate::DispatchRecord)(crate::DispatchRecord).
    #[serde(rename = "dispatch.recorded")]
    DispatchRecorded,
    /// An account's quota reading changed; payload is an
    /// [`AccountQuotaChanged`](crate::AccountQuotaChanged).
    #[serde(rename = "account.quota_changed")]
    AccountQuotaChanged,
    /// A finished task's learning awaits review; payload is a [`MemoryProposal`](crate::MemoryProposal)(crate::MemoryProposal).
    #[serde(rename = "memory.proposed")]
    MemoryProposed,
    /// A proposal was accepted and committed to the Project repo; payload is
    /// the accepted [`MemoryProposal`](crate::MemoryProposal)(crate::MemoryProposal) with its `entry`.
    #[serde(rename = "memory.accepted")]
    MemoryAccepted,
    /// A proposal was rejected; payload is the [`MemoryProposal`](crate::MemoryProposal)(crate::MemoryProposal).
    #[serde(rename = "memory.rejected")]
    MemoryRejected,
}

impl EventType {
    pub const ALL: [EventType; 17] = [
        EventType::ProjectUpdated,
        EventType::TaskCreated,
        EventType::TaskStateChanged,
        EventType::TaskEvent,
        EventType::CoordinatorMessage,
        EventType::WorkerTranscript,
        EventType::WorkerOutput,
        EventType::DecisionOpened,
        EventType::DecisionAnswered,
        EventType::PrUpdated,
        EventType::CheckUpdated,
        EventType::ReviewUpdated,
        EventType::DispatchRecorded,
        EventType::AccountQuotaChanged,
        EventType::MemoryProposed,
        EventType::MemoryAccepted,
        EventType::MemoryRejected,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            EventType::ProjectUpdated => "project.updated",
            EventType::TaskCreated => "task.created",
            EventType::TaskStateChanged => "task.state_changed",
            EventType::TaskEvent => "task.event",
            EventType::CoordinatorMessage => "coordinator.message",
            EventType::WorkerTranscript => "worker.transcript",
            EventType::WorkerOutput => "worker.output",
            EventType::DecisionOpened => "decision.opened",
            EventType::DecisionAnswered => "decision.answered",
            EventType::PrUpdated => "pr.updated",
            EventType::CheckUpdated => "check.updated",
            EventType::ReviewUpdated => "review.updated",
            EventType::DispatchRecorded => "dispatch.recorded",
            EventType::AccountQuotaChanged => "account.quota_changed",
            EventType::MemoryProposed => "memory.proposed",
            EventType::MemoryAccepted => "memory.accepted",
            EventType::MemoryRejected => "memory.rejected",
        }
    }

    pub fn parse(s: &str) -> Option<EventType> {
        EventType::ALL.into_iter().find(|t| t.as_str() == s)
    }
}

/// One entry of the event stream. `seq` is monotonic across the whole daemon;
/// a client that reconnects with `/v1/events?cursor=<last seq>` receives every
/// event with a greater `seq`, replayed from the store, then live events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Event {
    pub seq: i64,
    /// Absent for daemon-wide events.
    pub project_id: Option<String>,
    #[serde(rename = "type")]
    pub event_type: EventType,
    /// RFC 3339 UTC timestamp.
    pub ts: String,
    #[schema(value_type = Object)]
    pub payload: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_type_names_round_trip() {
        for t in EventType::ALL {
            let json = serde_json::to_string(&t).unwrap();
            assert_eq!(json, format!("\"{}\"", t.as_str()));
            assert_eq!(EventType::parse(t.as_str()), Some(t));
        }
    }
}
