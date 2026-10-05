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
    /// Merge this Project's pull requests as soon as they are green, without
    /// asking. Maps to the engine's merge posture for the Project's repos.
    #[serde(default)]
    pub standing_approval: bool,
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
    /// Turn standing approval on or off. Needs the Project's workspace when
    /// the Project has repos.
    pub standing_approval: Option<bool>,
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
    /// Account pool to run under: one of the harness's accounts in this pool
    /// is chosen per task (see `Account.pools`). Absent runs under the
    /// harness's default account.
    #[serde(default)]
    pub pool: Option<String>,
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
    /// The account the task's worker was started under (an `Account.id`),
    /// when the harness has accounts.
    #[serde(default)]
    pub account_id: Option<String>,
    /// Each time the worker hit a rate limit, oldest first: the account it
    /// moved to, or why it could not move.
    #[serde(default)]
    pub failovers: Vec<AccountFailover>,
    pub created_at: String,
    pub updated_at: String,
}

/// How a rate limit on a task's account was handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FailoverOutcome {
    /// The worker was relaunched from its branch, in the same worktree,
    /// under `to_account_id`.
    Relaunched,
    /// No other account in the pool was healthy; a decision was opened.
    NoHealthyAccount,
    /// The engine could not relaunch the worker; a decision was opened.
    RelaunchFailed,
}

/// One rate limit a task's worker hit, and where the worker went (ADR-11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AccountFailover {
    /// The account that reported the rate limit (an `Account.id`).
    pub from_account_id: String,
    /// The account the worker was relaunched under; absent unless `outcome`
    /// is `relaunched`.
    pub to_account_id: Option<String>,
    /// The pool the next account was chosen from, when one was named.
    pub pool: Option<String>,
    pub outcome: FailoverOutcome,
    /// The harness log line that reported the limit, e.g.
    /// `claude: assistant isApiErrorMessage error=rate_limit`.
    pub signal: String,
    /// What the harness said about the limit, or why the relaunch failed.
    pub detail: Option<String>,
    /// When the daemon handled it (RFC 3339 UTC).
    pub at: String,
}

/// A steering message for a task's worker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SendTaskMessage {
    /// Plain text for the worker to read; may span several lines.
    pub text: String,
}

/// An answer to an open decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AnswerDecision {
    /// The answer, as the worker or coordinator will read it.
    pub answer: String,
    /// Who is answering: one line of at most 128 bytes. Stored with the
    /// decision and recorded with the engine's own record of the answer.
    /// Absent or null means the daemon's own user (`$USER`).
    #[serde(default)]
    pub answered_by: Option<String>,
}

/// Replace a task's worker in the same worktree. Absent fields keep the
/// worker's current harness, model and effort.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RelaunchTask {
    pub harness: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Account pool to choose from when the task has no usable account of
    /// the harness yet; absent uses the Project agent config's pool when the
    /// harness matches. A task keeps its account across relaunches.
    #[serde(default)]
    pub pool: Option<String>,
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

/// Why a task got its agent (ADR-11): one record per spawn of the task's
/// worker. Records are history: they are kept after the task ends, for later
/// scoring, and never change once recorded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DispatchRecord {
    pub id: String,
    pub task_id: String,
    pub project_id: String,
    /// What started this worker.
    pub trigger: DispatchTrigger,
    /// Who chose the agent.
    pub decided_by: DispatchDecider,
    /// One sentence on why this agent, for people.
    pub summary: String,
    /// The dispatch rule the classifier matched; absent when none matched or
    /// no classifier was consulted.
    pub rule: Option<DispatchRule>,
    /// What the engine's dispatch resolution reported.
    pub resolution: DispatchResolution,
    /// Every profile the resolution weighed, in the rule's order, each with
    /// whether it passed and why.
    pub candidates: Vec<DispatchCandidate>,
    /// The agent the worker was actually started with.
    pub chosen: DispatchChoice,
    pub classifier: DispatchClassifier,
    /// The rate limit this relaunch answered, when the daemon relaunched the
    /// worker to move it to another account; also in `Task.failovers`.
    #[serde(default)]
    pub failover: Option<AccountFailover>,
    /// When the daemon recorded the dispatch (RFC 3339 UTC).
    pub recorded_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DispatchTrigger {
    /// The task's first worker.
    Spawn,
    /// A replacement worker in the same worktree.
    Relaunch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DispatchDecider {
    /// The classifier matched a rule confidently and the worker was started
    /// with the profile its resolution selected.
    Classifier,
    /// The coordinator picked: no classifier, a resolution that was not
    /// clear, or a selected profile the coordinator overrode.
    Coordinator,
    /// The worker was relaunched in its worktree; dispatch rules were not
    /// consulted again.
    Relaunch,
}

/// A dispatch rule, as the Project's dispatch rules name it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DispatchRule {
    /// The rule's id in the resolution, e.g. `rule_2`, or `default`.
    pub id: String,
    /// The rule's `when` condition.
    pub when: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DispatchStatus {
    /// A profile was selected.
    Clear,
    /// The classifier's confidence was below its floor.
    Ambiguous,
    /// The rule needs approval, no candidate qualified, there was a tie, or
    /// there were no rules to match.
    Escalate,
    /// The classifier or the quota read failed.
    Error,
    /// No classifier is configured (`provider: none`).
    Off,
    /// The resolution was not run for this worker: a relaunch, or a worker
    /// that started before the daemon saw it.
    NotConsulted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DispatchResolution {
    pub status: DispatchStatus,
    /// Why the status is not `clear`.
    pub reason: Option<String>,
    /// Further notes from the resolution, such as an unranked candidate.
    pub notes: Vec<String>,
    /// The resolution's output as the engine printed it, kept for scoring.
    pub output: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DispatchCandidate {
    pub harness: String,
    pub model: Option<String>,
    /// Whether the candidate was eligible.
    pub passed: bool,
    /// Why it passed or failed, e.g. `eligible` or `profile floor
    /// all_models below 15%`.
    pub reason: String,
    /// The quota evidence it was judged on, e.g. `provider=claude
    /// scope=all_models remaining=79%`.
    pub evidence: Option<String>,
}

/// The agent a worker was started with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DispatchChoice {
    pub harness: String,
    /// Absent for the harness's default model.
    pub model: Option<String>,
    /// Absent for the harness's default effort.
    pub effort: Option<String>,
    /// The account the worker was started under (an `Account.id`, as on the
    /// task when it was recorded); absent when the harness has no accounts or
    /// the daemon does not know it.
    pub account: Option<String>,
}

/// The classifier behind the System-1 API, as consulted for this dispatch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DispatchClassifier {
    /// `none` when no classifier was consulted and the coordinator picked.
    pub provider: String,
    /// The model that answered, e.g. `jev-1.13.0`.
    pub model: Option<String>,
    /// The classifier's confidence in the matched rule, from 0 to 1.
    pub confidence: Option<f64>,
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

/// Where a pull request is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestState {
    Open,
    /// Open but marked as a draft.
    Draft,
    Merged,
    /// Closed without merging.
    Closed,
}

impl PullRequestState {
    pub fn as_str(self) -> &'static str {
        match self {
            PullRequestState::Open => "open",
            PullRequestState::Draft => "draft",
            PullRequestState::Merged => "merged",
            PullRequestState::Closed => "closed",
        }
    }

    pub fn parse(s: &str) -> Option<PullRequestState> {
        match s {
            "open" => Some(PullRequestState::Open),
            "draft" => Some(PullRequestState::Draft),
            "merged" => Some(PullRequestState::Merged),
            "closed" => Some(PullRequestState::Closed),
            _ => None,
        }
    }

    /// Merged and closed pull requests no longer change.
    pub fn is_final(self) -> bool {
        matches!(self, PullRequestState::Merged | PullRequestState::Closed)
    }
}

/// Whether the forge can merge the head into the base without conflicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Mergeability {
    Mergeable,
    Conflicting,
    /// Not computed yet, or not reported.
    Unknown,
}

/// One state for all of a pull request's checks on its current head.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChecksState {
    /// Every check finished and none failed.
    Passing,
    /// At least one check failed.
    Failing,
    /// None failed and at least one has not finished.
    Pending,
    /// No check has reported on the head.
    None,
}

/// What reviewers decided, as the forge sums it up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    /// An approving review is required and has not been given.
    ReviewRequired,
    /// The repo requires no review and none decided.
    None,
}

/// The state of one check on a pull request's head.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    /// Queued or running.
    Pending,
    Success,
    Failure,
    /// Finished without passing or failing (neutral, skipped).
    Neutral,
    Cancelled,
}

/// One CI check or commit status on a pull request's current head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Check {
    /// Unique per pull request; for GitHub Actions, `workflow / job`.
    pub name: String,
    pub status: CheckStatus,
    /// The forge's own conclusion, e.g. `timed_out`, when it says more than `status`.
    pub conclusion: Option<String>,
    /// Link to the run or its log.
    pub details_url: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

/// What a review said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
    /// Started and not submitted yet.
    Pending,
}

/// One submitted review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Review {
    /// The forge's review id.
    pub id: String,
    /// Forge login of the reviewer.
    pub author: Option<String>,
    pub state: ReviewState,
    /// Markdown; may be empty.
    pub body: String,
    pub submitted_at: Option<String>,
    /// Head commit the review was left on.
    pub commit: Option<String>,
}

/// Where a verification gate, or a case in it, stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GateState {
    Pending,
    Running,
    Passed,
    Failed,
    Skipped,
}

/// The verification gates of ADR-15, run in this order before a PR opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GateKind {
    /// The repo's own checks (lint, tests) through the engine's validation.
    Checks,
    /// Playwright journeys declared for the repo in `project.yaml`.
    Journeys,
    /// Holdout tests from the Project repo, never shown to workers. Cases
    /// carry only a category and pass or fail.
    Holdout,
}

/// What a gate artifact is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// A Playwright `trace.zip`; opens in the Playwright trace viewer.
    Trace,
    Screenshot,
    Video,
    Log,
    /// A rendered report, e.g. Playwright's HTML report.
    Report,
}

/// A file a gate produced, served by the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EvidenceArtifact {
    /// Stable for the same file of the same pull request.
    pub id: String,
    pub kind: ArtifactKind,
    /// File name, e.g. `trace.zip`.
    pub name: String,
    pub content_type: String,
    pub size_bytes: Option<u64>,
    /// `/v1/pull-requests/{id}/evidence/artifacts/{artifact_id}`.
    pub url: String,
}

/// One case of a gate: a journey, a repo check, or a holdout category.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GateCase {
    pub name: String,
    pub state: GateState,
    pub duration_ms: Option<u64>,
    /// Why it failed; absent for holdout cases.
    pub message: Option<String>,
    pub artifacts: Vec<EvidenceArtifact>,
}

/// One verification gate's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Gate {
    pub kind: GateKind,
    pub state: GateState,
    pub summary: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub cases: Vec<GateCase>,
}

/// Verification gate results for a pull request (ADR-15): repo checks,
/// Playwright journeys and holdout tests, with traces and screenshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Evidence {
    /// The commit the gates ran on.
    pub head_sha: Option<String>,
    pub state: GateState,
    /// True when the pull request's head has moved past `head_sha`.
    pub stale: bool,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub gates: Vec<Gate>,
}

/// A pull request opened by one of a Project's tasks, with its checks and
/// reviews as last read from the forge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PullRequest {
    pub id: String,
    pub project_id: String,
    /// The task that opened it.
    pub task_id: Option<String>,
    pub url: String,
    /// `github` or `gitlab`.
    pub provider: String,
    /// Repository path, e.g. `quark-systems/quark`.
    pub repo: String,
    pub number: u64,
    /// Absent until the forge has been read.
    pub title: Option<String>,
    /// Forge login of the author.
    pub author: Option<String>,
    pub state: PullRequestState,
    pub head_ref: Option<String>,
    pub base_ref: Option<String>,
    /// Head commit the checks and mergeability refer to.
    pub head_sha: Option<String>,
    pub mergeable: Mergeability,
    pub checks_state: ChecksState,
    pub review_decision: ReviewDecision,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
    pub changed_files: Option<u64>,
    pub checks: Vec<Check>,
    pub reviews: Vec<Review>,
    /// Verification gate results (ADR-15); absent until the gates report.
    pub evidence: Option<Evidence>,
    /// When the forge reports the PR was opened, updated, merged and closed.
    pub opened_at: Option<String>,
    pub updated_at: Option<String>,
    pub merged_at: Option<String>,
    pub closed_at: Option<String>,
    /// When the daemon last read the forge successfully.
    pub synced_at: Option<String>,
    /// Why the last forge read failed, until one succeeds.
    pub sync_error: Option<String>,
}

/// Payload of `check.updated`: one check that appeared or changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CheckUpdated {
    pub pull_request_id: String,
    pub task_id: Option<String>,
    pub head_sha: Option<String>,
    pub check: Check,
}

/// Payload of `review.updated`: one review that appeared or changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ReviewUpdated {
    pub pull_request_id: String,
    pub task_id: Option<String>,
    pub review: Review,
}

/// A unified diff of a pull request, or of one of its files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PullRequestDiff {
    pub pull_request_id: String,
    /// The file this diff covers; absent for the whole pull request.
    pub path: Option<String>,
    pub patch: String,
    /// True when `patch` was cut at the size limit.
    pub truncated: bool,
}

/// Which side of a diff a line comment is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiffSide {
    /// The changed version (added or context lines).
    New,
    /// The base version (removed lines).
    Old,
}

/// A review comment for the worker that owns the pull request. It is
/// delivered to the worker as a steering message, not posted on the forge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PullRequestComment {
    pub text: String,
    /// File the comment is about, as it appears in the diff.
    pub path: Option<String>,
    /// Line in `path` on `side`; requires `path`.
    pub line: Option<u64>,
    /// Default `new`.
    pub side: Option<DiffSide>,
}

/// How to merge a pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MergeMethod {
    Squash,
    Merge,
    Rebase,
}

impl MergeMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            MergeMethod::Squash => "squash",
            MergeMethod::Merge => "merge",
            MergeMethod::Rebase => "rebase",
        }
    }
}

/// Request body for `POST /v1/pull-requests/{id}:merge`; may be empty.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MergePullRequest {
    /// Default `squash` on GitHub; GitLab uses the project's own setting.
    pub method: Option<MergeMethod>,
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

/// Where a memory proposal came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemorySource {
    /// The task's worker reported it.
    Worker,
    /// The Project coordinator added it for a finished task.
    Coordinator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryProposalState {
    Proposed,
    Accepted,
    Rejected,
}

/// What a learning rests on.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MemoryEvidence {
    /// The task it was learned in.
    pub task_id: Option<String>,
    pub task_title: Option<String>,
    /// The task's pull request when the learning was proposed.
    pub pull_request_url: Option<String>,
    /// Files the learning is about: the ones the report names, else the
    /// files the task changed.
    #[serde(default)]
    pub files: Vec<String>,
}

/// One accepted memory entry: a file under the Project repo's `memory/`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MemoryEntry {
    /// The file name without its extension.
    pub id: String,
    pub project_id: String,
    /// Path in the Project repo, e.g. `memory/2026-10-02-run-the-gates.md`.
    pub path: String,
    pub text: String,
    #[serde(default)]
    pub evidence: MemoryEvidence,
    pub source: Option<MemorySource>,
    /// When it was learned (RFC 3339 UTC); absent for a file written by hand
    /// without one.
    pub date: Option<String>,
    pub accepted_at: Option<String>,
    pub accepted_by: Option<String>,
    /// The proposal it was accepted from.
    pub proposal_id: Option<String>,
    /// The Project repo commit that added it, when known.
    pub commit: Option<String>,
}

/// A learning from a finished task, waiting for review before it becomes
/// Project memory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MemoryProposal {
    pub id: String,
    pub project_id: String,
    /// The text as proposed, or as accepted when it was edited.
    pub text: String,
    pub evidence: MemoryEvidence,
    pub source: MemorySource,
    pub state: MemoryProposalState,
    /// When it was proposed (RFC 3339 UTC).
    pub proposed_at: String,
    pub decided_at: Option<String>,
    pub decided_by: Option<String>,
    /// The entry it became, once accepted.
    pub entry: Option<MemoryEntry>,
}

/// Accept a memory proposal, optionally edited.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AcceptMemoryProposal {
    /// The text to keep instead of the proposed one.
    #[serde(default)]
    pub text: Option<String>,
    /// Who is accepting: one line of at most 128 bytes. Absent or null means
    /// the daemon's own user (`$USER`).
    #[serde(default)]
    pub decided_by: Option<String>,
}

/// Reject a memory proposal.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RejectMemoryProposal {
    /// Who is rejecting, as for [`AcceptMemoryProposal::decided_by`].
    #[serde(default)]
    pub decided_by: Option<String>,
}

/// One entry of user-level memory: a file under `~/.quark/memory/`, which
/// every Project's coordinator reads. A file written by hand carries only
/// `id`, `path` and `text`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct UserMemoryEntry {
    /// The file name without its extension.
    pub id: String,
    /// Absolute path of the file on this machine.
    pub path: String,
    pub text: String,
    #[serde(default)]
    pub evidence: MemoryEvidence,
    pub source: Option<MemorySource>,
    /// When it was learned (RFC 3339 UTC).
    pub date: Option<String>,
    /// The Project it was promoted from.
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    /// The Project memory entry it was promoted from.
    pub entry_id: Option<String>,
    /// The Project repo commit that added that entry, when known.
    pub commit: Option<String>,
    pub promoted_at: Option<String>,
    pub promoted_by: Option<String>,
}

/// Promote a Project memory entry to user-level memory.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PromoteMemoryEntry {
    /// Who is promoting, as for [`AcceptMemoryProposal::decided_by`].
    #[serde(default)]
    pub promoted_by: Option<String>,
}

/// A Project repo commit that touched `memory/`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MemoryCommit {
    pub commit: String,
    pub subject: String,
    pub author: Option<String>,
    /// When it was committed (RFC 3339).
    pub date: Option<String>,
    /// Unified diff of what it changed under `memory/`.
    pub patch: String,
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

/// An account a harness runs under: its own config directory, holding what
/// the harness writes after its own login. Every harness with accounts also
/// has a default account (`default: true`), its usual config directory, which
/// is listed but cannot be removed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Account {
    /// `acc_...`, or `default-<harness>` for a harness's default account.
    pub id: String,
    /// Harness id, e.g. `claude-code`.
    pub harness: String,
    pub label: String,
    /// The config directory the harness is pointed at (`CLAUDE_CONFIG_DIR`,
    /// `CODEX_HOME` and equivalents).
    pub config_dir: Option<String>,
    pub default: bool,
    /// Pools the account belongs to. A dispatch profile naming a pool runs
    /// each task under one of the pool's accounts for its harness.
    pub pools: Vec<String>,
    /// Credential health from the harness adapter.
    pub health: HarnessAuth,
    pub quota: AccountQuota,
    /// Tasks and coordinators currently running under the account.
    pub active_tasks: u32,
    /// False when the engine cannot start this harness under another account
    /// yet, so only its default account can be chosen from a pool.
    pub launchable: bool,
    pub created_at: Option<String>,
}

/// Add an account for a harness.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CreateAccount {
    pub harness: String,
    /// Shown in the app; defaults to the directory name.
    pub label: Option<String>,
    /// Absolute path of the account's config directory. Log in once with the
    /// harness pointed at it, e.g. `CLAUDE_CONFIG_DIR=<dir> claude`.
    pub config_dir: String,
    #[serde(default)]
    pub pools: Vec<String>,
}

/// Partial update; absent fields are left unchanged.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct UpdateAccount {
    /// Not accepted for a default account.
    pub label: Option<String>,
    /// Replaces the account's pools.
    pub pools: Option<Vec<String>>,
}

/// Whether an account's quota could be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuotaState {
    /// Not read yet.
    Pending,
    /// `remaining_percent` and `windows` hold the latest reading.
    Known,
    /// The read ran but had no numbers, e.g. the account needs a login.
    Unavailable,
    /// The quota reader failed or is not installed.
    Error,
    /// Quota is read per account for Claude Code and Codex only.
    Unsupported,
}

/// One rate-limit window of an account's plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct QuotaWindow {
    /// e.g. `five_hour`, `weekly`.
    pub id: String,
    /// e.g. `session`, `week`.
    pub label: String,
    pub percent_remaining: Option<f64>,
    /// RFC 3339 UTC.
    pub resets_at: Option<String>,
}

/// An account's latest quota reading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AccountQuota {
    pub state: QuotaState,
    /// What limits the account now, 0 to 100.
    pub remaining_percent: Option<f64>,
    /// The subscription plan, when reported.
    pub plan: Option<String>,
    pub windows: Vec<QuotaWindow>,
    /// Why there are no numbers.
    pub detail: Option<String>,
    /// When the reading was taken (RFC 3339 UTC).
    pub checked_at: Option<String>,
}

impl AccountQuota {
    /// A quota with no numbers in `state`.
    pub fn empty(state: QuotaState, detail: Option<String>) -> Self {
        Self {
            state,
            remaining_percent: None,
            plan: None,
            windows: Vec::new(),
            detail,
            checked_at: None,
        }
    }

    /// True when a reading shows nothing left.
    pub fn exhausted(&self) -> bool {
        self.state == QuotaState::Known && self.remaining_percent.is_some_and(|p| p <= 0.0)
    }
}

/// Payload of `account.quota_changed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AccountQuotaChanged {
    pub account_id: String,
    pub harness: String,
    pub quota: AccountQuota,
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
    /// A pull request appeared or changed; payload is a [`PullRequest`].
    #[serde(rename = "pr.updated")]
    PrUpdated,
    /// A check appeared or changed; payload is a [`CheckUpdated`].
    #[serde(rename = "check.updated")]
    CheckUpdated,
    /// A review appeared or changed; payload is a [`ReviewUpdated`].
    #[serde(rename = "review.updated")]
    ReviewUpdated,
    /// A worker was dispatched; payload is a [`DispatchRecord`].
    #[serde(rename = "dispatch.recorded")]
    DispatchRecorded,
    /// An account's quota reading changed; payload is an
    /// [`AccountQuotaChanged`].
    #[serde(rename = "account.quota_changed")]
    AccountQuotaChanged,
    /// A finished task's learning awaits review; payload is a [`MemoryProposal`].
    #[serde(rename = "memory.proposed")]
    MemoryProposed,
    /// A proposal was accepted and committed to the Project repo; payload is
    /// the accepted [`MemoryProposal`] with its `entry`.
    #[serde(rename = "memory.accepted")]
    MemoryAccepted,
    /// A proposal was rejected; payload is the [`MemoryProposal`].
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
