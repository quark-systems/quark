//! Agent harnesses: install, models, auth, supervision and config validation.

use crate::AgentConfig;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

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
