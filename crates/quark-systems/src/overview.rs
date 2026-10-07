//! The Project dashboard's Overview tab: what is happening now, and what
//! happened since you last looked, both read from the native event log.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// One Project's live status and, when asked for, the digest of what
/// changed since a given point in its event log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ProjectOverview {
    pub project_id: String,
    /// The event log's head when this was read. Send it back as `since` on
    /// the next visit to get a digest of everything after it.
    pub head: u64,
    pub live: LiveStatus,
    /// Present when the request named `since`.
    pub digest: Option<OverviewDigest>,
    /// Why the event log could not be read; `live` is then empty.
    pub error: Option<String>,
}

/// Every task's latest state, folded from its status lines.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LiveStatus {
    pub counts: StatusCounts,
    /// Open tasks first, then finished ones; most recent activity first
    /// within each group. Finished tasks older than a day are left out.
    pub tasks: Vec<TaskPulse>,
    /// When the Project's newest event was logged (RFC 3339).
    pub last_activity: Option<String>,
}

/// Tasks per [`PulseState`], each task once. Like the live list, done and
/// failed only count tasks that finished in the last day.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StatusCounts {
    pub working: u32,
    pub needs_decision: u32,
    pub blocked: u32,
    pub paused: u32,
    pub done: u32,
    pub failed: u32,
}

/// Coarse state of one task, from its last status line and open decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PulseState {
    Working,
    NeedsDecision,
    Blocked,
    Paused,
    Done,
    Failed,
}

impl PulseState {
    /// Done and failed tasks are finished; the rest still need watching.
    pub fn is_open(self) -> bool {
        !matches!(self, PulseState::Done | PulseState::Failed)
    }
}

/// One task's latest state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TaskPulse {
    /// The engine's id for the task, as its events carry it.
    pub engine_task: String,
    /// The Quark task, when the daemon has one for it.
    pub task_id: Option<String>,
    pub title: Option<String>,
    pub state: PulseState,
    /// The last status line's verb, as written.
    pub verb: String,
    /// The last status line's note.
    pub note: String,
    /// When the last line was logged (RFC 3339).
    pub at: String,
    /// Harness and model of the newest worker generation, when known.
    pub harness: Option<String>,
    pub model: Option<String>,
    /// Keys of decisions still open on this task.
    pub open_decisions: Vec<String>,
    /// The last pull request URL a status line named.
    pub pull_request: Option<String>,
}

/// What changed in a Project after `since`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct OverviewDigest {
    pub since: u64,
    /// When the first and last event after `since` were logged (RFC 3339).
    pub from: Option<String>,
    pub to: Option<String>,
    /// Every event of this Project after `since`.
    pub events: u32,
    /// Workers spawned or relaunched.
    pub spawned: u32,
    /// Tasks that reported done, and how many of those named a pull request.
    pub done: u32,
    pub pull_requests: u32,
    pub failed: u32,
    pub decisions_opened: u32,
    pub decisions_resolved: u32,
    /// The notable events (spawns, decisions, results), newest first.
    pub highlights: Vec<DigestItem>,
    /// More notable events happened than `highlights` holds.
    pub truncated: bool,
}

/// What a digest highlight was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DigestKind {
    Spawned,
    DecisionOpened,
    DecisionResolved,
    Done,
    Failed,
}

/// One notable event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DigestItem {
    pub seq: u64,
    pub at: String,
    /// The engine's id for the task, and the Quark task and its title when
    /// the daemon has one.
    pub engine_task: Option<String>,
    pub task_id: Option<String>,
    pub title: Option<String>,
    pub kind: DigestKind,
    /// The status line's note, or the harness and model of a spawn.
    pub text: String,
    /// A pull request URL the line named.
    pub url: Option<String>,
}
