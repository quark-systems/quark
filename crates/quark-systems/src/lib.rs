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
    /// Local path of the Project workspace, once it is provisioned or attached.
    pub workspace_path: Option<String>,
    pub status: ProjectStatus,
    /// What is happening now while provisioning, or why provisioning failed.
    pub status_detail: Option<String>,
    /// Code repos in the Project workspace.
    pub repos: Vec<RepoSource>,
    /// Harness, model and effort the coordinator and, by default, workers use.
    pub agent_config: Option<AgentConfig>,
    /// Dispatch preset the Project's `dispatch.yaml` was created from.
    pub dispatch_preset: Option<DispatchPreset>,
    pub delivery: Option<DeliveryPolicy>,
    /// Local path of the Project repo (bare), which holds `project.yaml`,
    /// `dispatch.yaml`, `instructions.md` and `memory/`.
    pub project_repo_path: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Create a Project.
///
/// With `repos`, the daemon provisions it: clones the repos into a new
/// Project workspace, writes the Project repo and starts the coordinator. The
/// Project is returned with status `provisioning` and moves to `ready` or
/// `failed` (see `project.updated` events). `agent_config` is required then.
///
/// Without `repos`, the Project is only recorded, optionally attached to an
/// existing workspace at `workspace_path`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct CreateProject {
    pub name: String,
    pub goal: Option<String>,
    /// Attach an existing workspace instead of provisioning one.
    pub workspace_path: Option<String>,
    #[serde(default)]
    pub repos: Vec<RepoSource>,
    pub agent_config: Option<AgentConfig>,
    /// One of the presets listed by `DispatchPreset`; default `single`.
    pub dispatch_preset: Option<DispatchPreset>,
    /// Default `gated`.
    pub delivery: Option<DeliveryPolicy>,
}

/// Partial update; absent fields are left unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UpdateProject {
    pub name: Option<String>,
    pub goal: Option<String>,
    pub workspace_path: Option<String>,
}

/// Where a Project is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectStatus {
    /// Repos are being cloned, the workspace seeded or the coordinator started.
    Provisioning,
    Ready,
    /// Provisioning stopped; `status_detail` says at which step and why.
    Failed,
}

impl ProjectStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectStatus::Provisioning => "provisioning",
            ProjectStatus::Ready => "ready",
            ProjectStatus::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> ProjectStatus {
        match s {
            "provisioning" => ProjectStatus::Provisioning,
            "failed" => ProjectStatus::Failed,
            _ => ProjectStatus::Ready,
        }
    }
}

/// A forge repo in a Project workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RepoSource {
    /// Clone URL (https, ssh, scp-like or an absolute local path).
    pub url: String,
    /// Short name, unique in the Project; derived from the URL when absent.
    pub name: Option<String>,
}

/// A harness with its model and effort.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AgentConfig {
    /// Harness id, e.g. `claude-code`, `codex`, `pi`.
    pub harness: String,
    pub model: Option<String>,
    /// `low`, `medium`, `high`, `xhigh` or `max`.
    pub effort: Option<String>,
}

/// How a Project's changes reach a pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryPolicy {
    /// Every change passes the verification gate before its PR is opened.
    Gated,
    /// Workers open the PR directly; CI is the only check.
    Direct,
}

/// Starting dispatch rules for a new Project. Both route to the Project's
/// agent config; rules can be edited afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DispatchPreset {
    /// Every task uses the agent config.
    Single,
    /// Trivial mechanical edits run at low effort; everything else uses the
    /// agent config.
    LightTrivial,
}

impl DispatchPreset {
    pub fn as_str(self) -> &'static str {
        match self {
            DispatchPreset::Single => "single",
            DispatchPreset::LightTrivial => "light_trivial",
        }
    }
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

/// A steering message for a task's worker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SendTaskMessage {
    /// Plain text for the worker to read; may span several lines.
    pub text: String,
}

/// Replace a task's worker in the same worktree. Absent fields keep the
/// worker's current harness, model and effort.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RelaunchTask {
    pub harness: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Where things stand, for the new worker, which keeps the worktree but
    /// none of the conversation. A default note is used when absent.
    pub note: Option<String>,
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

/// Who or what produced a transcript entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptRole {
    /// Input from a person, or from the engine on a person's behalf.
    User,
    /// Text the agent wrote for the reader.
    Assistant,
    /// The agent's visible reasoning, when the harness records it.
    Thinking,
    /// A tool the agent invoked; `text` holds its input.
    ToolCall,
    /// What a tool returned; `text` holds its output.
    ToolResult,
}

/// One entry of a coordinator or worker transcript, parsed from the harness's
/// own session log. Payload of `coordinator.message` and `worker.transcript`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TranscriptEntry {
    pub role: TranscriptRole,
    /// Markdown for `user`, `assistant` and `thinking`; tool input or output
    /// otherwise.
    pub text: String,
    /// Tool name, for `tool_call` and `tool_result` when the harness records it.
    pub tool_name: Option<String>,
    /// Pairs a `tool_result` with its `tool_call`.
    pub tool_call_id: Option<String>,
    /// `true` when a tool reported failure.
    pub is_error: bool,
    /// `true` when `text` was cut to the daemon's size limit.
    pub truncated: bool,
    /// RFC 3339 timestamp recorded by the harness, when present.
    pub ts: Option<String>,
}

/// A transcript entry as listed by the history endpoints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TranscriptItem {
    /// The `seq` of the event that carried this entry; pass it as `after` to
    /// page, and use it to merge history with live events.
    pub id: i64,
    #[serde(flatten)]
    pub entry: TranscriptEntry,
}

/// A message for a coordinator, typed into its session as if at the keyboard.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CoordinatorMessage {
    pub text: String,
}

/// The coordinator's session took the message. Its reply arrives as
/// `coordinator.message` events, read from the session log.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CoordinatorMessageAccepted {
    pub coordinator_id: String,
    /// `false` when the text was typed and submitted but the session did not
    /// confirm the submit; check the transcript before sending again.
    pub confirmed: bool,
    pub accepted_at: String,
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
