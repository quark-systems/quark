//! Pull requests, checks, reviews, verification gates and evidence.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

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

/// A repository the forge account can reach, offered when adding
/// repositories to a Project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ForgeRepository {
    /// `owner/name`.
    pub full_name: String,
    pub private: bool,
    pub archived: bool,
    pub description: Option<String>,
    /// Last push, RFC 3339.
    pub pushed_at: Option<String>,
    /// SSH clone URL, e.g. `git@github.com:owner/name.git`.
    pub ssh_url: String,
    /// HTTPS clone URL.
    pub clone_url: String,
}
