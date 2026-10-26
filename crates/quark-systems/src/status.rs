//! What needs you and how every Project and its agents are doing, in one
//! read: the summary the desktop app's attention queue, the menubar and
//! desktop widgets show.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{ProjectStatus, TaskState};

/// Every Project's status and everything that waits on you.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct StatusView {
    /// Everything that waits on you, oldest first.
    pub attention: Vec<AttentionItem>,
    /// Listed workers across all Projects, per tone.
    pub counts: ToneCounts,
    /// Projects by name; archived ones are left out.
    pub projects: Vec<ProjectSummary>,
}

/// What an attention item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttentionKind {
    /// An open decision.
    Decision,
    /// An open pull request with a failing check.
    PullRequest,
    /// A failed or blocked worker that is not waiting on a decision.
    Worker,
}

/// One thing that waits on you.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AttentionItem {
    /// `decision:<id>`, `pr:<id>` or `worker:<id>`; stable while it waits.
    pub key: String,
    pub kind: AttentionKind,
    /// The decision's id, the pull request's id or the worker's task id.
    pub target_id: String,
    /// The question, the pull request's title or the worker's title.
    pub title: String,
    pub project_id: String,
    pub project_name: String,
    /// When it started waiting (RFC 3339).
    pub since: String,
}

/// How a worker's state reads: the glossary's state words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Busy,
    NeedsYou,
    Ready,
    Failed,
    Parked,
    Idle,
}

impl Tone {
    /// The tone a worker in `state` shows.
    pub fn of(state: TaskState) -> Tone {
        match state {
            TaskState::Running => Tone::Busy,
            TaskState::NeedsDecision | TaskState::Blocked => Tone::NeedsYou,
            TaskState::InReview => Tone::Ready,
            TaskState::Failed => Tone::Failed,
            TaskState::Paused | TaskState::Queued => Tone::Parked,
            TaskState::Done | TaskState::Unknown => Tone::Idle,
        }
    }
}

/// Workers per tone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ToneCounts {
    pub busy: u32,
    pub needs_you: u32,
    pub ready: u32,
    pub failed: u32,
    pub parked: u32,
    pub idle: u32,
}

impl ToneCounts {
    pub fn add(&mut self, tone: Tone) {
        match tone {
            Tone::Busy => self.busy += 1,
            Tone::NeedsYou => self.needs_you += 1,
            Tone::Ready => self.ready += 1,
            Tone::Failed => self.failed += 1,
            Tone::Parked => self.parked += 1,
            Tone::Idle => self.idle += 1,
        }
    }

    pub fn merge(&mut self, other: ToneCounts) {
        self.busy += other.busy;
        self.needs_you += other.needs_you;
        self.ready += other.ready;
        self.failed += other.failed;
        self.parked += other.parked;
        self.idle += other.idle;
    }
}

/// One Project's status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ProjectSummary {
    pub id: String,
    pub name: String,
    pub status: ProjectStatus,
    pub status_detail: Option<String>,
    /// Items in `StatusView.attention` from this Project.
    pub attention: u32,
    pub open_decisions: u32,
    /// Open and draft pull requests.
    pub open_pull_requests: u32,
    /// Open pull requests with a failing check.
    pub failing_pull_requests: u32,
    /// Listed workers per tone.
    pub counts: ToneCounts,
    /// Listed workers: unfinished ones, and failed ones from the last three
    /// days, most recent activity first.
    pub workers: Vec<WorkerSummary>,
    /// The newest change to the Project, its workers, decisions or pull
    /// requests (RFC 3339).
    pub last_activity: String,
}

/// One listed worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WorkerSummary {
    pub id: String,
    pub title: String,
    pub state: TaskState,
    pub tone: Tone,
    /// The worker's latest status line, when it has one.
    pub note: Option<String>,
    pub harness: Option<String>,
    pub pull_request_url: Option<String>,
    pub updated_at: String,
}
