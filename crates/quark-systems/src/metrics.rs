//! The Project dashboard's Metrics tab: how the Project's work has gone,
//! computed from the native event log.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{AccountQuota, FailoverOutcome};

/// How a Project's work went over a window of days.
///
/// Task outcomes, lead time, interventions and relaunches come from the
/// event log (`events.db`), which mirrors firstmate's status lines and
/// spawns. A status line's time is when the daemon read it, so lines written
/// before the log started all carry the log's first time: `log_started_at`
/// says how far back the numbers reach.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ProjectMetrics {
    pub project_id: String,
    /// Days covered, ending now.
    pub days: u32,
    /// Start of the window (RFC 3339 UTC).
    pub from: String,
    /// End of the window, when the metrics were computed (RFC 3339 UTC).
    pub to: String,
    /// The Project's first event in the log; absent when it has none.
    pub log_started_at: Option<String>,
    pub throughput: Throughput,
    pub lead_time: LeadTime,
    pub gates: GateMetrics,
    pub interventions: Interventions,
    pub failovers: Failovers,
    /// Accounts this Project's tasks ran under, with their quota now.
    pub accounts: Vec<AccountUse>,
    /// The coordinator's turns and tokens: firstmate's, read from its
    /// transcript, beside the native coordinator's.
    #[serde(default)]
    pub coordinator: CoordinatorMetrics,
    /// Metrics the daemon cannot compute yet, and why.
    pub unavailable: Vec<UnavailableMetric>,
}

/// Tasks that finished in the window.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Throughput {
    /// Tasks whose last word was `done`.
    pub done: u32,
    /// Tasks whose last word was `failed`.
    pub failed: u32,
    /// One entry per UTC day of the window, oldest first.
    pub per_day: Vec<DayCount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DayCount {
    /// `YYYY-MM-DD` (UTC).
    pub date: String,
    pub done: u32,
    pub failed: u32,
}

/// From a task's first spawn to its `done`, over tasks done in the window.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LeadTime {
    /// Tasks measured.
    pub tasks: u32,
    pub median_s: Option<u64>,
    pub p90_s: Option<u64>,
}

/// How often finished work got through on its own.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct GateMetrics {
    /// Share of finished tasks that ended `done` rather than `failed`, 0 to
    /// 1; absent when none finished.
    pub pass_rate: Option<f64>,
    /// Done tasks that never reported `failed` or `blocked` and were never
    /// relaunched.
    pub first_time_green: u32,
    /// `first_time_green` over done tasks, 0 to 1; absent when none were done.
    pub first_time_green_rate: Option<f64>,
}

/// Times work stopped for someone: a worker asking for a decision or
/// reporting it is stuck.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Interventions {
    /// `needs-decision` lines in the window.
    pub decisions: u32,
    /// `blocked` lines in the window.
    pub blockers: u32,
    /// Interventions per finished task; absent when none finished.
    pub per_finished_task: Option<f64>,
    /// Workers started again after their first spawn, for any reason.
    pub relaunches: u32,
}

/// Rate-limit failovers (ADR-11) handled in the window.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Failovers {
    pub relaunched: u32,
    pub no_healthy_account: u32,
    pub relaunch_failed: u32,
}

impl Failovers {
    pub fn add(&mut self, outcome: FailoverOutcome) {
        match outcome {
            FailoverOutcome::Relaunched => self.relaunched += 1,
            FailoverOutcome::NoHealthyAccount => self.no_healthy_account += 1,
            FailoverOutcome::RelaunchFailed => self.relaunch_failed += 1,
        }
    }

    pub fn total(&self) -> u32 {
        self.relaunched + self.no_healthy_account + self.relaunch_failed
    }
}

/// One account the Project's tasks ran under.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AccountUse {
    pub account_id: String,
    pub harness: String,
    pub label: String,
    /// The Project's tasks started under it.
    pub tasks: u32,
    pub quota: AccountQuota,
}

/// Coordinator token efficiency over the window.
///
/// A turn that changed nothing (no task started, steered, answered or
/// cancelled, nothing told or asked of the user) only acknowledged status;
/// the native coordinator's target share of those is near zero.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CoordinatorMetrics {
    /// Tasks seen in the window, the divisor of the per-task figures.
    pub tasks: u32,
    /// firstmate's coordinator (Claude Code transcripts only).
    pub baseline: CoordinatorTurns,
    /// The native coordinator; no turns until slice 6 switches on.
    pub native: CoordinatorTurns,
    /// While slice 6 runs in shadow: turns the native coordinator would
    /// have taken.
    pub would_wake_turns: u32,
}

/// One coordinator's turns and tokens.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CoordinatorTurns {
    pub turns: u32,
    /// Turns that only acknowledged status.
    pub ack_turns: u32,
    /// Input tokens, including prompt cache reads and writes.
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Of the input tokens, how many were cache reads.
    pub cache_read_tokens: u64,
    pub turns_per_task: Option<f64>,
    pub tokens_per_task: Option<f64>,
    /// 0 to 1.
    pub ack_share: Option<f64>,
}

/// A metric the dashboard shows as not measured yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UnavailableMetric {
    /// e.g. `spend` or `coordinator_tokens`.
    pub metric: String,
    pub reason: String,
}
