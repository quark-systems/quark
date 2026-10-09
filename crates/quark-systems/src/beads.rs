//! Beads: the Project's issues, decision beads and memories, kept in one
//! Beads database per Project (<https://beads.gascity.com>), plus the New
//! issue drafts the coordinator writes before any bead exists.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{MemoryEvidence, MemorySource};

/// Where a Project's Beads database stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BeadsState {
    /// No database yet; `:setup` creates one.
    Missing,
    SettingUp,
    Ready,
    Failed,
    /// `bd` or `dolt` is not installed on this machine.
    Unavailable,
}

/// The outcome of one sync run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TrackerSync {
    /// When it finished (RFC 3339 UTC).
    pub at: String,
    pub ok: bool,
    /// What happened, or why it failed.
    pub message: String,
}

/// An issue tracker a sync rule can mirror. Beads also speaks Jira, Linear
/// and GitLab; Quark syncs GitHub Issues so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SyncTracker {
    Github,
}

/// Which way a sync rule carries issues.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SyncDirection {
    /// Bring the tracker's issues into Beads.
    Pull,
    /// Send this rule's issues to the tracker.
    Push,
    Both,
}

/// One explicit link between the Project's Beads database and an issue
/// tracker. A Project has none until someone adds one, so nothing leaves
/// Beads by default.
///
/// A rule only touches its own issues: a pull brings in the tracker's
/// issues, and a push sends the issues that came from that tracker plus
/// the new ones carrying the rule's `label`. Decisions never sync.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SyncRule {
    pub id: String,
    pub tracker: SyncTracker,
    /// The tracker's project, as `owner/repo` for GitHub.
    pub repository: String,
    pub direction: SyncDirection,
    /// Issues made in Beads with this label are pushed by this rule.
    pub label: String,
    /// A paused rule is kept but does not run.
    pub enabled: bool,
    #[serde(default)]
    pub last_sync: Option<TrackerSync>,
}

/// A sync rule as written by `PUT .../beads/sync-rules`; an omitted id makes
/// a new rule, and an omitted label is `repo:<repo name>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct WriteSyncRule {
    pub id: Option<String>,
    #[serde(default = "github")]
    pub tracker: SyncTracker,
    pub repository: String,
    pub direction: SyncDirection,
    pub label: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn github() -> SyncTracker {
    SyncTracker::Github
}

fn yes() -> bool {
    true
}

/// Body of `PUT /v1/projects/{id}/beads/sync-rules`: the whole rule list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct WriteSyncRules {
    pub rules: Vec<WriteSyncRule>,
}

/// A Project's Beads database. Also the payload of `beads.status` events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct BeadsStatus {
    pub project_id: String,
    pub state: BeadsState,
    /// Why it is unavailable or failed, or what setup is doing now.
    pub detail: Option<String>,
    /// The directory holding the database's `.beads`; agents get it as
    /// `BEADS_DIR`.
    pub dir: Option<String>,
    /// Issue id prefix, as in `qk-44`.
    pub prefix: Option<String>,
    /// Trackers the database syncs with; none by default.
    #[serde(default)]
    pub sync_rules: Vec<SyncRule>,
    /// The latest sync run, over every rule.
    pub last_sync: Option<TrackerSync>,
}

/// An issue linked to another, with enough to label the link.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct IssueRef {
    pub id: String,
    pub title: String,
    /// Beads' status: `open`, `in_progress`, `blocked`, `deferred`, `closed`...
    pub status: String,
    pub issue_type: String,
}

/// One bead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Issue {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub status: String,
    /// 0 (highest) to 4.
    pub priority: u8,
    /// bug, feature, task, epic, chore, decision... or a custom type.
    pub issue_type: String,
    #[serde(default)]
    pub labels: Vec<String>,
    pub assignee: Option<String>,
    pub owner: Option<String>,
    pub created_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    /// The GitHub issue it mirrors, when synced.
    pub external_ref: Option<String>,
    /// Issues this one waits for (`blocks` dependencies), closed ones included.
    #[serde(default)]
    pub blocked_by: Vec<String>,
    /// Open and nothing it waits for is still open.
    pub ready: bool,
    /// Something it waits for is still open.
    pub blocked: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct IssueComment {
    pub author: String,
    pub text: String,
    pub created_at: String,
}

/// A non-blocking link: `related`, `discovered-from`, `supersedes`,
/// `parent-child`...
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RelatedIssue {
    #[serde(flatten)]
    pub issue: IssueRef,
    pub kind: String,
}

/// One bead with everything linked to it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct IssueDetail {
    #[serde(flatten)]
    pub issue: Issue,
    pub notes: Option<String>,
    pub design: Option<String>,
    pub acceptance_criteria: Option<String>,
    pub blocked_by_issues: Vec<IssueRef>,
    /// Issues waiting for this one.
    pub blocks_issues: Vec<IssueRef>,
    pub related_issues: Vec<RelatedIssue>,
    pub comments: Vec<IssueComment>,
}

/// A `beads.changed` event: something in the database changed, from Quark,
/// an agent or a sync. Clients read the issues again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct BeadsChanged {
    pub project_id: String,
    /// The bead it touched, when the change names one.
    pub issue_id: Option<String>,
    /// Beads' journal operation (`create`, `update`, `close`, `dep_add`...),
    /// or `sync`, `memory` and `setup` for changes Quark made itself.
    pub op: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IssueDraftState {
    Open,
    Accepted,
    Discarded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DraftRole {
    User,
    Coordinator,
}

/// One turn of the New issue side chat.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DraftMessage {
    pub role: DraftRole,
    pub text: String,
    pub at: String,
}

/// A bead the coordinator drafted; nothing exists in Beads until the draft
/// is accepted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DraftIssue {
    /// Draft-local key (`1`, `2`).
    pub key: String,
    pub title: String,
    #[serde(default = "default_type")]
    pub issue_type: String,
    #[serde(default = "default_priority")]
    pub priority: u8,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub description: String,
    /// Other drafts' keys or existing issue ids this one waits for.
    #[serde(default)]
    pub blocked_by: Vec<String>,
}

fn default_type() -> String {
    "task".into()
}

fn default_priority() -> u8 {
    2
}

/// The New issue side chat: what was said and the beads drafted so far.
/// Also the payload of `issue_draft.updated` events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct IssueDraft {
    pub id: String,
    pub project_id: String,
    pub state: IssueDraftState,
    pub messages: Vec<DraftMessage>,
    pub issues: Vec<DraftIssue>,
    /// Existing issues judged related but separate.
    #[serde(default)]
    pub related: Vec<String>,
    /// A message went to the coordinator and its drafts have not come back.
    pub waiting: bool,
    /// Issue ids created on accept, by draft key.
    #[serde(default)]
    pub created: BTreeMap<String, String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Start a draft, or add a message to one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DraftText {
    pub text: String,
}

/// The coordinator's drafts, replacing the previous ones.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct WriteIssueDraft {
    /// What to say in the side chat.
    #[serde(default)]
    pub reply: Option<String>,
    pub issues: Vec<DraftIssue>,
    #[serde(default)]
    pub related: Vec<String>,
}

/// Create the drafted beads.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AcceptIssueDraft {
    /// The drafts as edited; the stored ones when absent.
    #[serde(default)]
    pub issues: Option<Vec<DraftIssue>>,
    /// Ask the coordinator to start a worker on the first created issue.
    #[serde(default)]
    pub start_worker: bool,
}

/// One of the Project's Beads memories (`bd remember`), injected into every
/// agent session by `bd prime`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct BeadsMemory {
    pub key: String,
    pub value: String,
    /// Where it came from, when it was accepted from a proposal in Quark.
    pub evidence: Option<MemoryEvidence>,
    pub source: Option<MemorySource>,
    pub accepted_at: Option<String>,
    pub accepted_by: Option<String>,
}
