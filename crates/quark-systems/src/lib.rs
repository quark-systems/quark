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

/// One entry of a task's activity log: something the worker or the engine
/// reported about the task. History, not current state; the task's `state`
/// is the reconciled truth.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TaskEvent {
    /// Monotonic per daemon; pass the last one seen as `after` to page.
    pub id: i64,
    pub task_id: String,
    pub project_id: String,
    /// What was reported, e.g. `working`, `needs-decision`, `done`.
    pub kind: String,
    /// The decision this entry opens or closes, when it names one.
    pub decision_key: Option<String>,
    pub note: String,
    /// When the daemon read the entry (RFC 3339 UTC).
    pub ts: String,
}

/// How a file differs between a task's base and its working tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FileChangeStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    /// New and not yet added to version control.
    Untracked,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ChangedFile {
    pub path: String,
    /// Previous path of a renamed or copied file.
    pub old_path: Option<String>,
    pub status: FileChangeStatus,
    /// Added lines; absent for binary files.
    pub additions: Option<u64>,
    /// Removed lines; absent for binary files.
    pub deletions: Option<u64>,
}

/// Files a task changed: its working tree, including uncommitted and
/// untracked work, compared with where it branched from the default branch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TaskChanges {
    pub task_id: String,
    /// The default-branch ref the task is compared against, e.g. `origin/main`.
    pub base_ref: String,
    /// Commit the task branched from (merge base with `base_ref`).
    pub base: String,
    /// Commit checked out in the task's working tree.
    pub head: String,
    pub files: Vec<ChangedFile>,
}

/// A unified diff for a whole task or one of its files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TaskDiff {
    pub task_id: String,
    pub base: String,
    /// The file this diff covers; absent for the whole task.
    pub path: Option<String>,
    pub patch: String,
    /// True when `patch` was cut at the size limit.
    pub truncated: bool,
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

/// Who a terminal belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TerminalRole {
    /// A Project's coordinator session. Its terminal id is the Project id.
    Coordinator,
    /// A task's worker session. Its terminal id is the task id.
    Worker,
}

/// A live terminal: one session pane the daemon streams as `worker.output`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Terminal {
    /// The task id for a worker, the Project id for a coordinator.
    pub id: String,
    pub project_id: String,
    pub role: TerminalRole,
    /// Set for worker terminals.
    pub task_id: Option<String>,
    pub title: String,
    pub cols: u16,
    pub rows: u16,
}

/// Raw bytes to type into a terminal.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TerminalInput {
    /// Base64 of the bytes, exactly as a terminal would send them
    /// (`\r` for Enter, escape sequences for keys).
    pub data_b64: String,
    /// Optional per-terminal sequence number. When given, it must be greater
    /// than the last one applied, so a retried request is not typed twice.
    pub seq: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
pub struct TerminalResize {
    pub cols: u16,
    pub rows: u16,
}

/// What a `worker.output` event carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TerminalChunkKind {
    /// Bytes the program wrote, in order. Not aligned to lines or UTF-8.
    Output,
    /// A full repaint: reset the emulator to `cols` x `rows`, then feed
    /// `data_b64`. Sent when the daemon (re)attaches to a pane or had to drop
    /// output, so a client never needs history from before it.
    Snapshot,
}

/// Payload of a `worker.output` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TerminalOutput {
    pub terminal_id: String,
    pub role: TerminalRole,
    pub task_id: Option<String>,
    pub kind: TerminalChunkKind,
    pub data_b64: String,
    /// Terminal size; set on snapshots.
    pub cols: Option<u16>,
    pub rows: Option<u16>,
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

/// The role an agent plays in a Project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    /// Runs a Project's chat and dispatches workers.
    Coordinator,
    /// Runs one task in its own worktree.
    Worker,
}

/// Reasoning effort, shared across harnesses. Each harness accepts a subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    pub const ALL: [Effort; 5] = [
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::Xhigh,
        Effort::Max,
    ];

    pub fn parse(s: &str) -> Option<Effort> {
        Effort::ALL.into_iter().find(|e| e.as_str() == s)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
        }
    }
}

/// Request body for `POST /v1/harnesses:validate`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ValidateAgentConfig {
    pub config: AgentConfig,
    /// The role the config is for; absent checks it as a worker.
    #[serde(default)]
    pub role: Option<AgentRole>,
}

/// One problem with an agent config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ConfigIssue {
    /// The config field the issue is about: `harness`, `model`, `effort` or `role`.
    pub field: String,
    /// Stable machine-readable code, e.g. `unknown_harness`.
    pub code: String,
    pub message: String,
}

/// The outcome of validating an agent config.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AgentConfigValidation {
    /// True when there are no errors; warnings do not block.
    pub valid: bool,
    pub errors: Vec<ConfigIssue>,
    /// Settings the harness will ignore, e.g. an effort it has no control for.
    pub warnings: Vec<ConfigIssue>,
}

/// Whether a harness is installed on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HarnessInstall {
    pub installed: bool,
    /// Version string the harness reports, when it could be read.
    pub version: Option<String>,
    /// Resolved executable path.
    pub path: Option<String>,
    /// How to install it, shown when it is missing.
    pub install_hint: String,
}

/// How a harness chooses its model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelSelection {
    /// Any model id the harness accepts.
    FreeForm,
    /// A `provider/model` id.
    ProviderQualified,
    /// The harness picks the model itself; a configured model is ignored.
    Automatic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HarnessModels {
    pub selection: ModelSelection,
    /// Where to find the current model list for this harness and account.
    pub discovery: Option<String>,
}

/// Login or key health for one account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuthState {
    /// A credential was found.
    Configured,
    /// The harness's documented credential sources are all empty.
    NotConfigured,
    /// Quark cannot tell from the outside.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HarnessAuth {
    pub state: AuthState,
    /// What was checked, e.g. the credential file or environment variable.
    pub detail: String,
}

/// How confidently the daemon can tell when an agent is busy or done.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SupervisionConfidence {
    /// Hooks, an extension or a session log report turns.
    High,
    /// Turn state is read from the terminal screen.
    Low,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HarnessSupervision {
    pub confidence: SupervisionConfidence,
    /// Where busy and turn-end state come from.
    pub source: String,
}

/// A harness the daemon can run agents with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct HarnessInfo {
    /// Stable id used in agent configs and dispatch rules, e.g. `claude-code`.
    pub id: String,
    pub name: String,
    pub roles: Vec<AgentRole>,
    pub install: HarnessInstall,
    pub models: HarnessModels,
    /// Accepted effort levels; empty when the harness has no effort control.
    pub efforts: Vec<Effort>,
    /// Credential health for the default account.
    pub auth: HarnessAuth,
    pub supervision: HarnessSupervision,
    /// True when chat output can be read from the harness session log.
    pub transcript: bool,
    /// Environment variable that selects an account's config directory, when
    /// the harness supports more than one account.
    pub account_env: Option<String>,
}

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
    /// A new entry in a task's activity log; payload is a [`TaskEvent`].
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
    pub const ALL: [EventType; 14] = [
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
