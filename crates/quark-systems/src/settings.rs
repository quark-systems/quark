//! The Project dashboard's Settings tab: every per-Project switch in one read.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{AgentConfig, DeliveryPolicy};

/// Every per-Project switch, read in one call.
///
/// Standing approval and holdout tests change through
/// `PATCH /v1/projects/{id}/settings`; dispatch rules and memory have their
/// own endpoints, summarized here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ProjectSettings {
    pub project_id: String,
    /// Merge this Project's pull requests as soon as they are green, without
    /// asking.
    pub standing_approval: bool,
    /// How changes reach a pull request. Chosen when the Project is created;
    /// the engine cannot change it afterwards yet, so it is read-only here.
    pub delivery: DeliveryPolicy,
    /// Harness, model, effort and account pool the coordinator and, by
    /// default, workers use.
    pub agent_config: Option<AgentConfig>,
    pub verification: VerificationSettings,
    pub dispatch: DispatchSummary,
    pub memory: MemorySummary,
}

/// The verification gates `project.yaml` on the Project repo's `main`
/// declares, per workspace source.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct VerificationSettings {
    /// Blob id of `project.yaml` on `main`; send it back with a holdout
    /// change. Absent without a Project repo or the file.
    pub revision: Option<String>,
    /// One per workspace source, in `project.yaml` order.
    pub sources: Vec<SourceVerification>,
    /// Why the gates could not be read, when `project.yaml` does not compile.
    pub error: Option<String>,
}

/// One workspace source's gates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SourceVerification {
    pub source: String,
    /// Repo checks, in order.
    pub checks: Vec<GateCheck>,
    /// Playwright journeys, when declared.
    pub journeys: Option<GateJourneys>,
    pub holdout: HoldoutSettings,
}

/// A repo check: a command that must exit 0.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GateCheck {
    pub name: String,
    pub run: String,
    pub timeout_s: Option<u64>,
}

/// Where the journeys run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GateJourneys {
    /// Command that starts the app.
    pub start: String,
    /// URL the journeys open.
    pub url: String,
    /// Directory, relative to the source, the journeys run from.
    pub dir: Option<String>,
}

/// Holdout tests for one source: tests workers never see, kept in the
/// Project repo under `holdout/<source>/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HoldoutSettings {
    /// Not turned off with `holdout: false`. The tests run when this is on
    /// and there is at least one category.
    pub enabled: bool,
    /// Category directories under `holdout/<source>/` on `main`.
    pub categories: Vec<String>,
    pub timeout_s: Option<u64>,
}

/// The Project's dispatch rules, in brief; the Dispatch screen edits them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DispatchSummary {
    /// Rules besides the default.
    pub rules: u32,
    /// Candidates of the default.
    pub default_candidates: u32,
    /// Classifier provider in effect; `none` when the coordinator picks.
    pub classifier: Option<String>,
    /// Why the rules could not be read, when they could not.
    pub error: Option<String>,
}

/// The Project's memory, in brief; the Memory screen reviews it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MemorySummary {
    /// Entries under `memory/` on the Project repo's `main`.
    pub entries: u32,
    /// Proposals waiting for review.
    pub proposals_to_review: u32,
}

/// Change Project settings. Absent fields are left unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UpdateProjectSettings {
    /// Turn standing approval on or off.
    pub standing_approval: Option<bool>,
    /// Turn holdout tests on or off per source. Written to `project.yaml`
    /// in one commit on the Project repo's `main`; the engine takes the new
    /// gates on the daemon's next refresh.
    #[serde(default)]
    pub holdout: Vec<HoldoutChange>,
    /// `verification.revision` the change was made against; a holdout change
    /// is refused when `main` has another.
    pub revision: Option<String>,
}

/// Turn one source's holdout tests on or off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HoldoutChange {
    pub source: String,
    pub enabled: bool,
}
