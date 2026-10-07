//! Projects, their repo source and agent configuration.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

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
