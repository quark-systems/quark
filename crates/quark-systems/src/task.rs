//! Tasks, their activity log, changes and decisions.

use crate::AccountFailover;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

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
    /// The model the worker runs: what its session log reports, else what
    /// it was started with. Absent for the harness default before the log
    /// says.
    #[serde(default)]
    pub model: Option<String>,
    /// The git branch checked out in the task's working copy, while it
    /// exists.
    #[serde(default)]
    pub branch: Option<String>,
    /// When the finished task was cleared: its terminal closed, its
    /// worktree given back and its card taken off the board. Its history is
    /// kept. Absent while the task is shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleared_at: Option<String>,
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
    /// Account pool to choose from when the task has no usable account of
    /// the harness yet; absent uses the Project agent config's pool when the
    /// harness matches. A task keeps its account across relaunches.
    #[serde(default)]
    pub pool: Option<String>,
    /// Where things stand, for the new worker, which keeps the worktree but
    /// none of the conversation. A default note is used when absent.
    pub note: Option<String>,
}

/// Clear a Project's finished workers at once. Only done and failed tasks
/// are ever cleared.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ClearTasks {
    /// Which finished states to clear; absent clears done and failed tasks.
    #[serde(default)]
    pub states: Option<Vec<TaskState>>,
    /// Throw away work that has not landed instead of keeping those
    /// workers, as [`ClearTask::discard`] does.
    #[serde(default)]
    pub discard: bool,
}

/// How to clear one finished worker.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ClearTask {
    /// Throw away the worktree's uncommitted changes and untracked files
    /// instead of keeping the worker. Its commits stay on its branch.
    #[serde(default)]
    pub discard: bool,
}

/// What clearing a Project's finished workers did.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ClearedTasks {
    /// Task ids taken off the board, their sessions stopped and worktrees
    /// given back.
    pub cleared: Vec<String>,
    /// Tasks left on the board because their worktree holds work that has
    /// not landed, or because the engine could not clean them up.
    pub kept: Vec<KeptTask>,
}

/// A task clearing left on the board, and why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct KeptTask {
    pub task_id: String,
    pub title: String,
    /// `unlanded_work` or `engine_failed`.
    pub code: String,
    pub reason: String,
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

#[cfg(test)]
mod tests {
    use super::*;

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

/// How and why a task's worker was dispatched: the issue it was started
/// for, the brief it received, and the delivery contract it was given. Parts
/// the daemon has no record of are absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TaskBrief {
    pub task_id: String,
    /// The issue the task was dispatched for: the first issue id in the
    /// task's title, then in its brief, that the Project's Beads database
    /// has. `external_ref` links its tracker issue.
    pub issue: Option<crate::Issue>,
    /// The brief as dispatched.
    pub brief: Option<DispatchBrief>,
    /// When it was dispatched (RFC 3339 UTC).
    pub dispatched_at: Option<String>,
    /// Who dispatched it: `coordinator`, the only one that starts tasks.
    pub dispatched_by: Option<String>,
    /// How its changes reach a pull request, as recorded at spawn.
    pub delivery: Option<TaskDelivery>,
    /// Whether its pull request merges without asking once green, as
    /// recorded at spawn.
    pub merges_without_asking: Option<bool>,
    /// The pull request the task opened, if any.
    pub pull_request_url: Option<String>,
}

/// A task's brief as the coordinator wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DispatchBrief {
    /// The whole brief, Markdown.
    pub markdown: String,
    /// The section restating what the person asked for (`Captain's intent`
    /// in the engine's briefs), Markdown without its heading.
    pub intent: Option<String>,
    /// The section where the coordinator specifies the work (`Firstmate
    /// spec`), Markdown without its heading.
    pub spec: Option<String>,
    /// The brief as the worker was started with it, when the engine wraps
    /// the coordinator's in one of its own.
    pub launch: Option<String>,
}

/// How a task's changes reach a pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskDelivery {
    /// The change passes the verification gate before its PR is opened.
    Gated,
    /// The worker opens the PR directly; CI is the only check.
    Direct,
    /// The change stays local; no PR.
    LocalOnly,
}
