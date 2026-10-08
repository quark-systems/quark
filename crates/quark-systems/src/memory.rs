//! Project and user memory: entries, proposals and commits.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

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
    /// Its key in the Project's Beads memories (`bd remember`), when it was
    /// accepted into Beads rather than committed to `memory/`.
    #[serde(default)]
    pub beads_key: Option<String>,
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
    /// Who should know; `project` when absent.
    #[serde(default)]
    pub scope: Option<MemoryScope>,
}

/// Who an accepted learning is for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    /// This Project: its Beads memories, or `memory/` in the Project repo
    /// while it has no Beads database.
    #[default]
    Project,
    /// Every Project of this user: user-level memory.
    User,
    /// Anyone working in the repo: kept as Project memory, and the
    /// coordinator is asked for a pull request adding it to `AGENTS.md`.
    Repo,
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
