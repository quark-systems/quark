//! The slice switch: which engine runs each slice of the port.
//!
//! Slices switch from `bash` to `shadow` to `native` strictly in order, each
//! only after shadow mode shows native agrees with bash and the
//! verify-quark journeys pass. Every change is recorded so a rollback is one
//! call.

use std::collections::BTreeMap;
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

/// Which engine serves a slice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SliceMode {
    /// firstmate only.
    #[default]
    Bash,
    /// firstmate decides and acts; native runs beside it on reads and its
    /// disagreements are recorded.
    Shadow,
    /// Native decides and acts.
    Native,
}

impl SliceMode {
    pub fn as_str(self) -> &'static str {
        match self {
            SliceMode::Bash => "bash",
            SliceMode::Shadow => "shadow",
            SliceMode::Native => "native",
        }
    }
}

impl FromStr for SliceMode {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "bash" => Ok(SliceMode::Bash),
            "shadow" => Ok(SliceMode::Shadow),
            "native" => Ok(SliceMode::Native),
            _ => Err(CoreError::Invalid(format!("slice mode {s:?}"))),
        }
    }
}

/// One recorded mode change. Payload of a
/// [`crate::event::kinds::SLICE_MODE`] event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SliceChange {
    pub slice: Slice,
    pub from: SliceMode,
    pub to: SliceMode,
    /// Who changed it, or `rollback`.
    pub by: String,
}

/// What the shadow engine records when bash and native disagree. Payload of
/// a [`crate::event::kinds::SHADOW_DIVERGENCE`] event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Divergence {
    pub slice: Slice,
    /// The adapter operation, such as `snapshot`.
    pub operation: String,
    /// What bash returned, which is what callers got.
    pub bash: serde_json::Value,
    /// What native returned, or its error.
    pub native: serde_json::Value,
}

/// Per-slice modes with their change history.
///
/// Invariant: slices switch on in order. A slice may be `shadow` only when
/// every earlier slice is at least `shadow`, and `native` only when every
/// earlier slice is `native`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SliceSwitch {
    /// Slices not listed are `bash`.
    #[serde(default)]
    modes: BTreeMap<Slice, SliceMode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    history: Vec<SliceChange>,
}

impl SliceSwitch {
    /// Every slice on bash.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mode(&self, slice: Slice) -> SliceMode {
        self.modes.get(&slice).copied().unwrap_or_default()
    }

    /// Every slice and its mode, in order.
    pub fn modes(&self) -> Vec<(Slice, SliceMode)> {
        Slice::ALL.into_iter().map(|s| (s, self.mode(s))).collect()
    }

    pub fn history(&self) -> &[SliceChange] {
        &self.history
    }

    /// Set `slice` to `to`, enforcing switch-on order. Returns the change,
    /// or `None` when the slice was already in that mode.
    pub fn set(&mut self, slice: Slice, to: SliceMode, by: &str) -> Result<Option<SliceChange>> {
        let from = self.mode(slice);
        if from == to {
            return Ok(None);
        }
        let mut next = self.modes.clone();
        next.insert(slice, to);
        Self::check_order(&next)?;
        self.modes = next;
        self.modes.retain(|_, m| *m != SliceMode::Bash);
        let change = SliceChange {
            slice,
            from,
            to,
            by: by.to_string(),
        };
        self.history.push(change.clone());
        Ok(Some(change))
    }

    /// Undo the last change to `slice`, putting it back in its previous
    /// mode. Rolling back also rolls back later slices that the order rule
    /// would otherwise leave ahead of it.
    pub fn rollback(&mut self, slice: Slice) -> Result<Vec<SliceChange>> {
        let Some(last) = self.history.iter().rev().find(|c| c.slice == slice) else {
            return Err(CoreError::Invalid(format!(
                "slice {slice} has no change to roll back"
            )));
        };
        let target = last.from;
        let mut changes = Vec::new();
        // Later slices first, so every intermediate state keeps the order rule.
        for later in Slice::ALL.into_iter().rev().filter(|s| *s > slice) {
            if rank(self.mode(later)) > rank(target) {
                changes.extend(self.set(later, target, "rollback")?);
            }
        }
        changes.extend(self.set(slice, target, "rollback")?);
        Ok(changes)
    }

    /// Parse `1=shadow,verification=bash`, the form a CLI flag or env var
    /// takes. Every slice not named is `bash`.
    pub fn parse(spec: &str) -> Result<Self> {
        let mut modes = BTreeMap::new();
        for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (slice, mode) = part
                .split_once('=')
                .ok_or_else(|| CoreError::Invalid(format!("slice setting {part:?}")))?;
            let mode: SliceMode = mode.trim().parse()?;
            if mode != SliceMode::Bash {
                modes.insert(slice.trim().parse()?, mode);
            }
        }
        Self::check_order(&modes)?;
        Ok(Self {
            modes,
            history: Vec::new(),
        })
    }

    fn check_order(modes: &BTreeMap<Slice, SliceMode>) -> Result<()> {
        let mut floor = SliceMode::Native;
        for slice in Slice::ALL {
            let m = modes.get(&slice).copied().unwrap_or_default();
            if rank(m) > rank(floor) {
                return Err(CoreError::Refused(format!(
                    "slice {} can't be {} before every earlier slice is",
                    slice,
                    m.as_str()
                )));
            }
            floor = m;
        }
        Ok(())
    }
}

fn rank(m: SliceMode) -> u8 {
    match m {
        SliceMode::Bash => 0,
        SliceMode::Shadow => 1,
        SliceMode::Native => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_bash() {
        let s = SliceSwitch::new();
        assert!(s.modes().iter().all(|(_, m)| *m == SliceMode::Bash));
    }

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

    #[test]
    fn enforces_switch_on_order() {
        let mut s = SliceSwitch::new();
        assert!(s.set(Slice::Verification, SliceMode::Shadow, "t").is_err());
        s.set(Slice::EventLog, SliceMode::Shadow, "t").unwrap();
        s.set(Slice::Verification, SliceMode::Shadow, "t").unwrap();
        // Native needs every earlier slice native.
        assert!(s.set(Slice::Verification, SliceMode::Native, "t").is_err());
        s.set(Slice::EventLog, SliceMode::Native, "t").unwrap();
        s.set(Slice::Verification, SliceMode::Native, "t").unwrap();
        assert_eq!(s.mode(Slice::Verification), SliceMode::Native);
    }

    #[test]
    fn rollback_restores_previous_mode_and_later_slices() {
        let mut s = SliceSwitch::parse("1=shadow").unwrap();
        s.set(Slice::EventLog, SliceMode::Native, "matt").unwrap();
        s.set(Slice::Verification, SliceMode::Shadow, "matt")
            .unwrap();
        let changes = s.rollback(Slice::EventLog).unwrap();
        assert_eq!(s.mode(Slice::EventLog), SliceMode::Shadow);
        assert_eq!(s.mode(Slice::Verification), SliceMode::Shadow);
        assert_eq!(changes.len(), 1);

        let mut s = SliceSwitch::new();
        s.set(Slice::EventLog, SliceMode::Shadow, "matt").unwrap();
        s.set(Slice::Verification, SliceMode::Shadow, "matt")
            .unwrap();
        let changes = s.rollback(Slice::EventLog).unwrap();
        assert_eq!(s.mode(Slice::EventLog), SliceMode::Bash);
        assert_eq!(s.mode(Slice::Verification), SliceMode::Bash);
        assert_eq!(changes.len(), 2);
        assert!(changes.iter().all(|c| c.by == "rollback"));
    }

    #[test]
    fn parses_flag_form() {
        let s = SliceSwitch::parse("event_log=native, 2=shadow ,3=bash").unwrap();
        assert_eq!(s.mode(Slice::EventLog), SliceMode::Native);
        assert_eq!(s.mode(Slice::Verification), SliceMode::Shadow);
        assert_eq!(s.mode(Slice::WorkerProtocol), SliceMode::Bash);
        assert!(SliceSwitch::parse("2=shadow").is_err());
        assert!(SliceSwitch::parse("1=fast").is_err());
        assert!(SliceSwitch::parse("").unwrap() == SliceSwitch::new());
    }
}
