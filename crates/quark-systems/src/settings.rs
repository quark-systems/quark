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
    /// When the coordinator's conversation is compacted.
    pub compaction: CompactionSettings,
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
    /// Change when the coordinator's conversation is compacted.
    #[serde(default)]
    pub compaction: Option<CompactionSettings>,
}

/// When Quark compacts a Project coordinator's conversation. With `auto`
/// on, a turn that ends with the context at `at_percent` of the model's
/// window or more (of 200k when the window is unknown) is followed
/// by a wrap up, then the harness's `/compact`. With it off, Quark leaves
/// compaction to the harness, which compacts on its own only when the
/// context window is nearly full.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CompactionSettings {
    pub auto: bool,
    /// How full the context window is, in percent, when a compaction starts.
    pub at_percent: u8,
}

/// A Project coordinator's agent and how full its context is, for the
/// coordinator's header.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CoordinatorContext {
    pub coordinator_id: String,
    /// The harness, model, effort and pool it was started with.
    pub agent_config: Option<AgentConfig>,
    /// The model its session log last named, when it differs from or fills
    /// in the configured one.
    pub model_seen: Option<String>,
    /// Tokens in context when its last turn ended. Absent before its first
    /// turn, and after a compaction until the next turn ends.
    pub context_tokens: Option<u64>,
    /// The model's context window, when Quark knows it.
    pub window_tokens: Option<u64>,
    /// When the turn that `context_tokens` was read from ended.
    pub measured_at: Option<String>,
    pub compaction: CompactionSettings,
    /// The context size, in tokens, at which it compacts: `at_percent` of
    /// the window. Absent when automatic compaction is off.
    pub compact_at_tokens: Option<u64>,
    /// What Quark is doing about compaction right now.
    pub compacting: CompactionPhase,
    /// The last time the conversation was compacted, by anyone.
    pub last_compaction: Option<CompactionRecord>,
    /// The latest change of its agent, under way or finished.
    #[serde(default)]
    pub switch: Option<CoordinatorSwitch>,
}

/// A change of a coordinator's harness, model, effort or pool, and how far
/// it got. See `docs/engine/coordinator-switch.md`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CoordinatorSwitch {
    pub id: String,
    pub project_id: String,
    pub from: Option<AgentConfig>,
    pub to: AgentConfig,
    pub kind: SwitchKind,
    pub state: SwitchState,
    /// What the old coordinator wrote for the new one (handoff only).
    pub brief: Option<String>,
    /// Why it failed.
    pub error: Option<String>,
    pub requested_at: String,
    pub updated_at: String,
}

/// How the conversation carries over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SwitchKind {
    /// Same harness: the conversation continues under the new model.
    Resume,
    /// Another harness: a handoff brief carries over.
    Handoff,
}

/// A switch's step. Each is recorded before its effect happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SwitchState {
    Requested,
    /// The old coordinator was asked to save what it knows and write a brief.
    WrappingUp,
    /// The new agent is committed; the old coordinator is being closed.
    Switching,
    /// The new coordinator is being started.
    Starting,
    /// The new coordinator is being told what changed.
    Briefing,
    Done,
    /// The new agent could not start; the old one was put back.
    Failed,
}

impl SwitchState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::WrappingUp => "wrapping_up",
            Self::Switching => "switching",
            Self::Starting => "starting",
            Self::Briefing => "briefing",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "requested" => Self::Requested,
            "wrapping_up" => Self::WrappingUp,
            "switching" => Self::Switching,
            "starting" => Self::Starting,
            "briefing" => Self::Briefing,
            "done" => Self::Done,
            "failed" => Self::Failed,
            _ => return None,
        })
    }

    /// Whether the switch still has steps to take.
    pub fn open(self) -> bool {
        !matches!(self, Self::Done | Self::Failed)
    }
}

/// Where a Quark-run compaction is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompactionPhase {
    /// Nothing under way.
    Idle,
    /// The coordinator was asked to wrap up; `/compact` follows its turn.
    WrappingUp,
    /// `/compact` was sent and has not been seen to finish.
    Compacting,
}

/// A compaction the coordinator's session log recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CompactionRecord {
    /// RFC 3339, as the harness recorded it.
    pub at: Option<String>,
    /// Who compacted it and at what size, as the conversation shows it.
    pub text: String,
}

/// Turn one source's holdout tests on or off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HoldoutChange {
    pub source: String,
    pub enabled: bool,
}
