//! The guardrail's rules as pure functions: no reads, no clock, no state.
//!
//! [`observe`] advances a red-main episode from one health read, and
//! [`merge_decision`] and [`dispatch_decision`] decide from the facts a
//! caller gathered. [`crate::NativeGuard`] feeds them live reads; the shadow
//! feeds them the inputs firstmate recorded, so a disagreement is a rule
//! difference, never a timing one.
//!
//! The rules (the requirements' "rebase and re-verify" and "red-main
//! guardrail"):
//!
//! 1. A merge's head must contain the base branch's current tip.
//! 2. When the project declares gates, they must have passed on exactly that
//!    head.
//! 3. While main is red, only a change whose own run of every failing check
//!    is green may merge (with rule 1 its head contains the red tip, so
//!    merging it turns main green), and new dispatch is paused except one
//!    fix-main task. Work already running continues.
//! 4. Only a person overrides rule 3, by answering the episode's one
//!    decision; the override lasts until main is green again.
//! 5. A merge refuses when main's health can't be read; a dispatch goes ahead,
//!    so a network blip never stops work.

use std::collections::BTreeSet;
use std::fmt;

use quark_core::Permit;
use serde::{Deserialize, Serialize};

use crate::health::HealthRead;

/// Main's status for the guardrail, firstmate's `FM_RM_STATUS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MainStatus {
    /// Green or no checks; any episode is closed.
    Clear,
    /// An episode is open and nobody has overridden it.
    Red,
    /// An episode is open and its decision was answered.
    Overridden,
    /// Unreadable with no episode open.
    Unknown,
}

impl MainStatus {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "clear" => Some(MainStatus::Clear),
            "red" => Some(MainStatus::Red),
            "overridden" => Some(MainStatus::Overridden),
            "unknown" => Some(MainStatus::Unknown),
            _ => None,
        }
    }
}

/// An open red-main episode: one per repository and branch, from the first
/// red read until a green one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Episode {
    /// RFC 3339 time of the first red read.
    pub since: String,
    /// The red tip.
    pub commit: String,
    /// The checks failing on it; a fixing change must pass all of them.
    pub checks: Vec<String>,
    /// The person's decision filed for this episode, once filed.
    #[serde(default)]
    pub decision: Option<String>,
    /// The one fix-main task, once dispatched.
    #[serde(default)]
    pub fix_task: Option<String>,
}

/// What one health read did to the episode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub status: MainStatus,
    /// The episode after the read; `None` when none is open.
    pub episode: Option<Episode>,
    /// This read opened the episode (file its decision, dispatch a fix).
    pub opened: bool,
    /// This read closed an episode (its decision is moot).
    pub closed: bool,
}

/// Advance `prev` by one read. `read` is `None` when main's health could not
/// be read; `overridden` says whether a decision id has been answered.
///
/// A pending tip (a fix just landed, its checks still running) and an
/// unreadable one keep an open episode with its recorded failing checks.
pub fn observe(
    prev: Option<Episode>,
    read: Option<&HealthRead>,
    now: &str,
    overridden: impl Fn(&str) -> bool,
) -> Observation {
    let had = prev.is_some();
    let (status, episode, opened, closed) = match read {
        Some(r) if r.state.is_clear() => (MainStatus::Clear, None, false, had),
        Some(r) if r.state == crate::health::HealthState::Red => {
            let mut e = prev.unwrap_or_else(|| Episode {
                since: now.to_string(),
                commit: String::new(),
                checks: Vec::new(),
                decision: None,
                fix_task: None,
            });
            e.commit = r.tip.clone();
            e.checks = r.red.clone();
            (MainStatus::Red, Some(e), !had, false)
        }
        // Pending.
        Some(_) => match prev {
            Some(e) => (MainStatus::Red, Some(e), false, false),
            None => (MainStatus::Clear, None, false, false),
        },
        None => match prev {
            Some(e) => (MainStatus::Red, Some(e), false, false),
            None => (MainStatus::Unknown, None, false, false),
        },
    };
    let status = match (&status, &episode) {
        (
            MainStatus::Red,
            Some(Episode {
                decision: Some(d), ..
            }),
        ) if overridden(d) => MainStatus::Overridden,
        _ => status,
    };
    Observation {
        status,
        episode,
        opened,
        closed,
    }
}

/// Why a guard allowed or refused, spelled as firstmate records it in
/// `state/guard-decisions.jsonl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    StaleHead,
    HeadUnreadable,
    GatesNotPassed,
    /// firstmate only: its episode file could not be written.
    RedMainRecordFailed,
    MainClear,
    Overridden,
    MainUnknown,
    FixesMain,
    MainRed,
    FixMainClaimed,
    FixMainTaken,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::StaleHead => "stale_head",
            Reason::HeadUnreadable => "head_unreadable",
            Reason::GatesNotPassed => "gates_not_passed",
            Reason::RedMainRecordFailed => "red_main_record_failed",
            Reason::MainClear => "main_clear",
            Reason::Overridden => "overridden",
            Reason::MainUnknown => "main_unknown",
            Reason::FixesMain => "fixes_main",
            Reason::MainRed => "main_red",
            Reason::FixMainClaimed => "fix_main_claimed",
            Reason::FixMainTaken => "fix_main_taken",
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A guard's answer with its reason code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub allow: bool,
    pub reason: Reason,
    /// What a person reads.
    pub message: String,
}

impl Decision {
    fn allow(reason: Reason, message: impl Into<String>) -> Self {
        Self {
            allow: true,
            reason,
            message: message.into(),
        }
    }

    fn deny(reason: Reason, message: impl Into<String>) -> Self {
        Self {
            allow: false,
            reason,
            message: message.into(),
        }
    }

    pub fn permit(&self) -> Permit {
        if self.allow {
            Permit::Allow
        } else {
            Permit::Deny {
                reason: self.message.clone(),
            }
        }
    }
}

/// Whether a head contains the base branch's current tip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    Fresh,
    Stale,
    Unreadable,
}

impl Freshness {
    /// From a `behind_by` count; `None` is unreadable.
    pub fn from_behind(behind: Option<u64>) -> Self {
        match behind {
            Some(0) => Freshness::Fresh,
            Some(_) => Freshness::Stale,
            None => Freshness::Unreadable,
        }
    }
}

/// Gate evidence for a head, when the project declares gates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateEvidence {
    /// The head the gates last ran on.
    pub head: String,
    /// `passed`, `failed`, `running`, ...
    pub state: String,
}

/// Everything a merge decision needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeFacts {
    pub head: String,
    pub freshness: Freshness,
    /// `None` when the project declares no gates; `Some(None)` when it does
    /// and there is no evidence.
    pub gates: Option<Option<GateEvidence>>,
    pub main: MainStatus,
    /// The open episode's failing checks.
    pub failing: Vec<String>,
    /// Checks the change passed on its own head.
    pub green: BTreeSet<String>,
}

pub fn merge_decision(f: &MergeFacts) -> Decision {
    match f.freshness {
        Freshness::Fresh => {}
        Freshness::Stale => {
            return Decision::deny(
                Reason::StaleHead,
                format!(
                    "stale head {}: rebase onto main, verify the rebased head, then merge",
                    short(&f.head)
                ),
            )
        }
        Freshness::Unreadable => {
            return Decision::deny(
                Reason::HeadUnreadable,
                format!(
                    "could not read whether {} contains main's tip",
                    short(&f.head)
                ),
            )
        }
    }
    if let Some(evidence) = &f.gates {
        let passed = evidence
            .as_ref()
            .is_some_and(|e| e.head == f.head && e.state == "passed");
        if !passed {
            return Decision::deny(
                Reason::GatesNotPassed,
                format!("gates have not passed on {}", short(&f.head)),
            );
        }
    }
    match f.main {
        MainStatus::Clear => Decision::allow(Reason::MainClear, "main is green"),
        MainStatus::Overridden => Decision::allow(
            Reason::Overridden,
            "main is red, but a person overrode the guardrail",
        ),
        MainStatus::Unknown => Decision::deny(Reason::MainUnknown, "could not read main's health"),
        MainStatus::Red => {
            let missing: Vec<&str> = f
                .failing
                .iter()
                .filter(|c| !f.green.contains(*c))
                .map(String::as_str)
                .collect();
            if missing.is_empty() && !f.failing.is_empty() {
                Decision::allow(
                    Reason::FixesMain,
                    "main is red and this change passes every failing check, so it may merge",
                )
            } else {
                let missing = if missing.is_empty() {
                    "the failing checks".to_string()
                } else {
                    missing.join(", ")
                };
                Decision::deny(
                    Reason::MainRed,
                    format!("main is red; only a change that turns it green may merge, and this one has no green run of: {missing}"),
                )
            }
        }
    }
}

/// Everything a dispatch decision needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchFacts {
    pub task: String,
    pub main: MainStatus,
    /// The task was dispatched as the fix-main task.
    pub fix_main: bool,
    /// The episode's recorded fix-main task, if any.
    pub prior_fix: Option<String>,
    /// Whether that task is still live.
    pub prior_fix_live: bool,
}

pub fn dispatch_decision(f: &DispatchFacts) -> Decision {
    match f.main {
        MainStatus::Clear => Decision::allow(Reason::MainClear, "main is green"),
        MainStatus::Unknown => Decision::allow(
            Reason::MainUnknown,
            "could not read main's health; dispatching without the red-main check",
        ),
        MainStatus::Overridden => Decision::allow(
            Reason::Overridden,
            "main is red, but a person overrode the guardrail",
        ),
        MainStatus::Red if f.fix_main => match &f.prior_fix {
            Some(other) if *other != f.task && f.prior_fix_live => Decision::deny(
                Reason::FixMainTaken,
                format!("main already has a live fix-main task, {other}"),
            ),
            _ => Decision::allow(Reason::FixMainClaimed, "dispatching the one fix-main task"),
        },
        MainStatus::Red => Decision::deny(
            Reason::MainRed,
            "main is red, so new work is paused except one fix-main task",
        ),
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::HealthState;

    fn read(state: HealthState, red: &[&str]) -> HealthRead {
        HealthRead {
            tip: "tip".into(),
            state,
            red: red.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn episode_lifecycle() {
        let none = |_: &str| false;
        let o = observe(None, Some(&read(HealthState::None, &[])), "t0", none);
        assert_eq!(
            (o.status, o.opened, o.closed),
            (MainStatus::Clear, false, false)
        );

        let o = observe(None, Some(&read(HealthState::Red, &["ci"])), "t1", none);
        assert_eq!(o.status, MainStatus::Red);
        assert!(o.opened);
        let mut e = o.episode.unwrap();
        assert_eq!(
            (e.since.as_str(), e.checks.clone()),
            ("t1", vec!["ci".to_string()])
        );
        e.decision = Some("d1".into());

        // A second red read keeps the episode and its start.
        let o = observe(
            Some(e.clone()),
            Some(&read(HealthState::Red, &["ci"])),
            "t2",
            none,
        );
        assert!(!o.opened);
        assert_eq!(o.episode.as_ref().unwrap().since, "t1");
        // Pending and unreadable keep it.
        let o = observe(
            Some(e.clone()),
            Some(&read(HealthState::Pending, &[])),
            "t3",
            none,
        );
        assert_eq!(o.status, MainStatus::Red);
        assert_eq!(o.episode.as_ref().unwrap().checks, vec!["ci"]);
        let o = observe(Some(e.clone()), None, "t3", none);
        assert_eq!(o.status, MainStatus::Red);
        // An answered decision overrides.
        let o = observe(Some(e.clone()), None, "t3", |d| d == "d1");
        assert_eq!(o.status, MainStatus::Overridden);
        // Green closes it.
        let o = observe(Some(e), Some(&read(HealthState::Green, &[])), "t4", none);
        assert_eq!(
            (o.status, o.closed, o.episode),
            (MainStatus::Clear, true, None)
        );
        // Unreadable with nothing open is unknown; pending with nothing open is clear.
        assert_eq!(observe(None, None, "t5", none).status, MainStatus::Unknown);
        assert_eq!(
            observe(None, Some(&read(HealthState::Pending, &[])), "t5", none).status,
            MainStatus::Clear
        );
    }

    fn facts() -> MergeFacts {
        MergeFacts {
            head: "h".into(),
            freshness: Freshness::Fresh,
            gates: None,
            main: MainStatus::Clear,
            failing: vec![],
            green: BTreeSet::new(),
        }
    }

    #[test]
    fn merge_rules_in_order() {
        assert_eq!(merge_decision(&facts()).reason, Reason::MainClear);
        let f = MergeFacts {
            freshness: Freshness::Stale,
            main: MainStatus::Red,
            ..facts()
        };
        assert_eq!(merge_decision(&f).reason, Reason::StaleHead);
        let f = MergeFacts {
            freshness: Freshness::Unreadable,
            ..facts()
        };
        assert_eq!(merge_decision(&f).reason, Reason::HeadUnreadable);

        let gated = |head: &str, state: &str| MergeFacts {
            gates: Some(Some(GateEvidence {
                head: head.into(),
                state: state.into(),
            })),
            ..facts()
        };
        assert_eq!(
            merge_decision(&gated("old", "passed")).reason,
            Reason::GatesNotPassed
        );
        assert_eq!(
            merge_decision(&gated("h", "failed")).reason,
            Reason::GatesNotPassed
        );
        assert!(merge_decision(&gated("h", "passed")).allow);
        let f = MergeFacts {
            gates: Some(None),
            ..facts()
        };
        assert_eq!(merge_decision(&f).reason, Reason::GatesNotPassed);

        let red = |green: &[&str]| MergeFacts {
            main: MainStatus::Red,
            failing: vec!["build".into(), "ci".into()],
            green: green.iter().map(|s| s.to_string()).collect(),
            ..facts()
        };
        let d = merge_decision(&red(&["ci"]));
        assert_eq!(d.reason, Reason::MainRed);
        assert!(d.message.ends_with("no green run of: build"));
        assert_eq!(
            merge_decision(&red(&["ci", "build", "x"])).reason,
            Reason::FixesMain
        );
        // An episode with no recorded failing check proves nothing.
        let f = MergeFacts {
            main: MainStatus::Red,
            ..facts()
        };
        assert_eq!(merge_decision(&f).reason, Reason::MainRed);
        let f = MergeFacts {
            main: MainStatus::Overridden,
            ..facts()
        };
        assert_eq!(merge_decision(&f).reason, Reason::Overridden);
        let f = MergeFacts {
            main: MainStatus::Unknown,
            ..facts()
        };
        assert!(!merge_decision(&f).allow);
    }

    #[test]
    fn dispatch_rules() {
        let f = |main, fix_main, prior: Option<&str>, live| DispatchFacts {
            task: "t".into(),
            main,
            fix_main,
            prior_fix: prior.map(str::to_string),
            prior_fix_live: live,
        };
        let r = |x: DispatchFacts| dispatch_decision(&x).reason;
        assert_eq!(
            r(f(MainStatus::Clear, true, None, false)),
            Reason::MainClear
        );
        assert_eq!(
            r(f(MainStatus::Unknown, false, None, false)),
            Reason::MainUnknown
        );
        assert_eq!(r(f(MainStatus::Red, false, None, false)), Reason::MainRed);
        assert_eq!(
            r(f(MainStatus::Red, true, None, false)),
            Reason::FixMainClaimed
        );
        assert_eq!(
            r(f(MainStatus::Red, true, Some("t"), true)),
            Reason::FixMainClaimed
        );
        assert_eq!(
            r(f(MainStatus::Red, true, Some("o"), false)),
            Reason::FixMainClaimed
        );
        assert_eq!(
            r(f(MainStatus::Red, true, Some("o"), true)),
            Reason::FixMainTaken
        );
        assert_eq!(
            r(f(MainStatus::Overridden, false, None, false)),
            Reason::Overridden
        );
    }
}
