//! Captain holds and open decisions, derived from a fleet snapshot.
//!
//! The engine has no separate decision type: a decision is a backlog task held
//! for the captain, and a worker's open `needs-decision`/`blocked` events are
//! folded per key into `hints.open_decisions`. The engine classifies both;
//! this module only reshapes them for the daemon's `decisions` projection.

use serde::{Deserialize, Serialize};

use crate::snapshot::{BacklogState, FleetSnapshot, HoldBucket};

/// A backlog task held for the captain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptainHold {
    pub task_id: String,
    pub title: Option<String>,
    pub repo: Option<String>,
    pub reason: Option<String>,
    pub hold_kind: Option<String>,
    /// Deferred until this date (`hold-until`).
    pub until: Option<String>,
    /// Machine-written timestamp of when the hold was set.
    pub set_at: Option<String>,
    pub bucket: HoldBucket,
    /// Waiting on the captain now (exactly `bucket == Live`).
    pub actionable: bool,
    pub age_days: Option<i64>,
    pub unresolved_blocker_ids: Vec<String>,
}

/// A keyed decision a worker opened in its status log and has not resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenDecision {
    pub task_id: String,
    pub key: String,
    /// `needs-decision` or `blocked`.
    pub verb: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decisions {
    pub holds: Vec<CaptainHold>,
    pub open: Vec<OpenDecision>,
}

impl Decisions {
    /// Holds waiting on the captain now.
    pub fn actionable_holds(&self) -> impl Iterator<Item = &CaptainHold> {
        self.holds.iter().filter(|h| h.actionable)
    }
}

/// Every captain hold (all buckets, backlog order) and every open decision
/// (task id order, then the engine's key order).
pub fn decisions(snapshot: &FleetSnapshot) -> Decisions {
    let holds = snapshot
        .backlog_items()
        .filter(|r| r.state != BacklogState::Done)
        .filter_map(|r| {
            let bucket = r.hold_bucket?;
            Some(CaptainHold {
                task_id: r.id.clone()?,
                title: r.title.clone(),
                repo: r.repo.clone(),
                reason: r.hold_reason.clone(),
                hold_kind: r.hold_kind.clone(),
                until: r.hold_until.clone(),
                set_at: r.hold_set.clone(),
                bucket,
                actionable: r.captain_actionable,
                age_days: r.hold_age_days,
                unresolved_blocker_ids: r.unresolved_blocker_ids.clone(),
            })
        })
        .collect();

    let open = snapshot
        .tasks
        .iter()
        .flat_map(|t| {
            t.hints.open_decisions.iter().map(|d| OpenDecision {
                task_id: t.id.clone(),
                key: d.key.clone().unwrap_or_else(|| "default".into()),
                verb: d.verb.clone(),
                summary: d.summary.clone(),
            })
        })
        .collect();

    Decisions { holds, open }
}
