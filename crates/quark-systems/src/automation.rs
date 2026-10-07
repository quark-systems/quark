//! A Project's automation: its inbox, its trigger rules and its away policy.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Everything the Automation screen shows, in one read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ProjectAutomation {
    pub project_id: String,
    /// Whether the native engine acts on rules and routes, or only records
    /// what it would do beside the current engine.
    pub acting: bool,
    /// Messages waiting for the coordinator, oldest first.
    pub inbox: Vec<InboxMessage>,
    pub rules: Vec<TriggerRule>,
    pub away: AwaySettings,
}

/// A message from any channel, waiting until the coordinator handles it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct InboxMessage {
    pub id: String,
    /// `inbox` for notes; email, voice and mentions later.
    pub channel: String,
    pub from: String,
    pub body: String,
    /// RFC 3339.
    pub at: String,
    pub task_id: Option<String>,
}

/// A note for the coordinator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct NewInboxNote {
    pub body: String,
}

/// A condition-to-action rule as defined.
///
/// `when` is one of `{"source": "event", "kind", "task"?, "fields"?}`,
/// `{"source": "every", "secs"}`, `{"source": "at", "at"}` or
/// `{"source": "command", "argv", "interval_secs"?, "stable"?,
/// "timeout_secs"?, "expect"?, "error_budget"?}`. `then` is one of
/// `{"do": "wake", "note"}`, `{"do": "inbox", "body"}`,
/// `{"do": "steer", "task", "text"}` or
/// `{"do": "command", "argv", "timeout_secs"?}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TriggerRule {
    pub id: String,
    pub description: String,
    pub enabled: bool,
    /// Stops after its first fire. Time (`at`) and command conditions
    /// always fire once.
    pub once: bool,
    #[schema(value_type = Object)]
    pub when: serde_json::Value,
    #[schema(value_type = Object)]
    pub then: serde_json::Value,
    /// When this version of the rule was defined, RFC 3339.
    pub defined_at: String,
    /// Fires so far (or would-be fires, when the engine is not acting).
    pub fires: u32,
}

/// A rule to add or replace under the id in the path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PutTriggerRule {
    #[serde(default)]
    pub description: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub once: bool,
    #[schema(value_type = Object)]
    pub when: serde_json::Value,
    #[schema(value_type = Object)]
    pub then: serde_json::Value,
}

fn yes() -> bool {
    true
}

/// Where the user is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AwayPosture {
    Present,
    /// Gone; their decisions wait for their return.
    Away,
    /// Present, but only wants what matters.
    Quiet,
}

/// What happened, as far as routing cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AwayOccasion {
    Progress,
    Paused,
    Done,
    Decision,
    Blocked,
    Failed,
    Stale,
    Inbound,
    TriggerFired,
    TriggerFailed,
}

/// How the user hears about an occasion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AwayReach {
    Silent,
    Digest,
    Notify,
    /// Kept for their return.
    Hold,
}

/// One cell of the away policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AwayRoute {
    pub posture: AwayPosture,
    pub occasion: AwayOccasion,
    /// Wake the coordinator for judgment.
    pub wake: bool,
    pub user: AwayReach,
    /// Differs from the built-in default because the Project or the global
    /// policy set it.
    #[serde(default)]
    pub overridden: bool,
}

/// The away policy in force for a Project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AwaySettings {
    pub posture: AwayPosture,
    /// Seconds between digests.
    pub digest_secs: u64,
    /// Every posture and occasion.
    pub routes: Vec<AwayRoute>,
    /// Items waiting for the next digest.
    pub waiting: u32,
    /// Items held for the user's return.
    pub held: u32,
}

/// Replace a Project's away policy: the digest interval and every cell that
/// should differ from the default (`overridden` is ignored).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct UpdateAwayPolicy {
    pub digest_secs: u64,
    #[serde(default)]
    pub routes: Vec<AwayRoute>,
}
