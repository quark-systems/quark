//! Neutral types for the Quark `/v1` API and its typed event stream.
//!
//! These types are the contract between `quarkd` and every client (the desktop
//! app today, generated web and mobile clients later). They use neutral names
//! only: no engine vocabulary leaks through here.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// API version prefix every route is served under.
pub const API_VERSION: &str = "v1";

/// A Project: a goal-driven workspace that owns tasks, decisions and PRs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub goal: Option<String>,
    /// Local path of the Project workspace, when one is attached.
    pub workspace_path: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateProject {
    pub name: String,
    pub goal: Option<String>,
    pub workspace_path: Option<String>,
}

/// Partial update; absent fields are left unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UpdateProject {
    pub name: Option<String>,
    pub goal: Option<String>,
    pub workspace_path: Option<String>,
}

/// What a task delivers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// Produces a code change and usually a pull request.
    Ship,
    /// Produces a report, never a pull request.
    Scout,
}

/// Neutral task lifecycle state. Engine adapters map their own states onto
/// these; anything they cannot classify is `unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Queued,
    Running,
    NeedsDecision,
    Blocked,
    Paused,
    InReview,
    Done,
    Failed,
    Unknown,
}

impl TaskState {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskState::Queued => "queued",
            TaskState::Running => "running",
            TaskState::NeedsDecision => "needs_decision",
            TaskState::Blocked => "blocked",
            TaskState::Paused => "paused",
            TaskState::InReview => "in_review",
            TaskState::Done => "done",
            TaskState::Failed => "failed",
            TaskState::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> TaskState {
        match s {
            "queued" => TaskState::Queued,
            "running" => TaskState::Running,
            "needs_decision" => TaskState::NeedsDecision,
            "blocked" => TaskState::Blocked,
            "paused" => TaskState::Paused,
            "in_review" => TaskState::InReview,
            "done" => TaskState::Done,
            "failed" => TaskState::Failed,
            _ => TaskState::Unknown,
        }
    }
}

impl TaskKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskKind::Ship => "ship",
            TaskKind::Scout => "scout",
        }
    }

    pub fn parse(s: &str) -> Option<TaskKind> {
        match s {
            "ship" => Some(TaskKind::Ship),
            "scout" => Some(TaskKind::Scout),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Task {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub kind: Option<TaskKind>,
    pub state: TaskState,
    /// Short human-readable note about the latest state change.
    pub state_note: Option<String>,
    pub harness: Option<String>,
    pub pull_request_url: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionState {
    Open,
    Answered,
}

/// A question held for a person.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Decision {
    pub id: String,
    pub project_id: String,
    pub task_id: Option<String>,
    pub question: String,
    pub state: DecisionState,
    pub answer: Option<String>,
    pub answered_by: Option<String>,
    pub opened_at: String,
    pub answered_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Health {
    pub status: String,
    pub version: String,
    /// Name of the engine adapter in use.
    pub engine: String,
    /// Highest event `seq` in the store; 0 when empty.
    pub last_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ErrorDetail {
    /// Stable machine-readable code, e.g. `not_found`.
    pub code: String,
    pub message: String,
}

/// Every event type the stream can carry. Phase 0 emits the project, task and
/// decision events; the rest are reserved so clients can be generated now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
pub enum EventType {
    #[serde(rename = "project.updated")]
    ProjectUpdated,
    #[serde(rename = "task.created")]
    TaskCreated,
    #[serde(rename = "task.state_changed")]
    TaskStateChanged,
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
    #[serde(rename = "pr.updated")]
    PrUpdated,
    #[serde(rename = "check.updated")]
    CheckUpdated,
    #[serde(rename = "review.updated")]
    ReviewUpdated,
    #[serde(rename = "dispatch.recorded")]
    DispatchRecorded,
    #[serde(rename = "account.quota_changed")]
    AccountQuotaChanged,
}

impl EventType {
    pub const ALL: [EventType; 13] = [
        EventType::ProjectUpdated,
        EventType::TaskCreated,
        EventType::TaskStateChanged,
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
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            EventType::ProjectUpdated => "project.updated",
            EventType::TaskCreated => "task.created",
            EventType::TaskStateChanged => "task.state_changed",
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

    #[test]
    fn task_state_round_trip() {
        for s in [
            TaskState::Queued,
            TaskState::Running,
            TaskState::NeedsDecision,
            TaskState::Blocked,
            TaskState::Paused,
            TaskState::InReview,
            TaskState::Done,
            TaskState::Failed,
            TaskState::Unknown,
        ] {
            assert_eq!(TaskState::parse(s.as_str()), s);
            assert_eq!(
                serde_json::to_string(&s).unwrap(),
                format!("\"{}\"", s.as_str())
            );
        }
    }
}
