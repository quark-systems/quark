//! The slices of the native port, and what a shadow comparison records.
//!
//! Every slice now runs native. The worktree pool can still be compared
//! with treehouse ([`Divergence`]); older event logs also carry the
//! firstmate comparisons recorded before firstmate was retired.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{CoreError, Result};

/// Slices of the native port, in switch-on order (slice 0 is tooling and has
/// no switch; slice 10 retires bash).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Slice {
    /// 1: event log, task state machine, recovery.
    EventLog,
    /// 2: gates, rebase and re-verify, red-main guardrail, merge.
    Verification,
    /// 3: MCP, hooks, file fallback.
    WorkerProtocol,
    /// 4: spawn, steer, supervise; PTY supervisor; harness manifests.
    Supervision,
    /// 5: dispatch, pools, hosts, telemetry, admission control.
    Dispatch,
    /// 6: judgment-only coordinator, layered prompt, personas.
    Coordinator,
    /// 7: sub-coordinators, triggers, channels, away policy.
    SubCoordinators,
    /// 8: sandbox, dashboard, metrics.
    Sandbox,
    /// 9: native worktree pool replacing treehouse.
    WorktreePool,
}

impl Slice {
    pub const ALL: [Slice; 9] = [
        Slice::EventLog,
        Slice::Verification,
        Slice::WorkerProtocol,
        Slice::Supervision,
        Slice::Dispatch,
        Slice::Coordinator,
        Slice::SubCoordinators,
        Slice::Sandbox,
        Slice::WorktreePool,
    ];

    /// Its number in the requirements' slice order.
    pub fn number(self) -> u8 {
        Self::ALL.iter().position(|s| *s == self).unwrap() as u8 + 1
    }

    pub fn from_number(n: u8) -> Option<Slice> {
        Self::ALL.get((n as usize).checked_sub(1)?).copied()
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Slice::EventLog => "event_log",
            Slice::Verification => "verification",
            Slice::WorkerProtocol => "worker_protocol",
            Slice::Supervision => "supervision",
            Slice::Dispatch => "dispatch",
            Slice::Coordinator => "coordinator",
            Slice::SubCoordinators => "sub_coordinators",
            Slice::Sandbox => "sandbox",
            Slice::WorktreePool => "worktree_pool",
        }
    }
}

impl fmt::Display for Slice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Slice {
    type Err = CoreError;

    /// A slice name or its number.
    fn from_str(s: &str) -> Result<Self> {
        if let Ok(n) = s.parse::<u8>() {
            return Slice::from_number(n)
                .ok_or_else(|| CoreError::Invalid(format!("no slice {n}")));
        }
        Slice::ALL
            .into_iter()
            .find(|x| x.as_str() == s)
            .ok_or_else(|| CoreError::Invalid(format!("no slice {s:?}")))
    }
}

/// What a shadow comparison records when the reference and native disagree. Payload of
/// a [`crate::event::kinds::SHADOW_DIVERGENCE`] event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Divergence {
    pub slice: Slice,
    /// The adapter operation, such as `snapshot`.
    pub operation: String,
    /// What the reference (treehouse, or firstmate in older logs) returned,
    /// which is what callers got.
    pub bash: serde_json::Value,
    /// What native returned, or its error.
    pub native: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_round_trip() {
        for s in Slice::ALL {
            assert_eq!(Slice::from_number(s.number()), Some(s));
            assert_eq!(s.as_str().parse::<Slice>().unwrap(), s);
        }
        assert_eq!(Slice::EventLog.number(), 1);
        assert_eq!(Slice::WorktreePool.number(), 9);
        assert!(Slice::from_number(0).is_none());
    }
}
