//! Typed view of `fm-fleet-snapshot.sh --json`.
//!
//! The script header in the engine owns the contract. These types keep the
//! fields the daemon projects and ignore the rest, so additive engine changes
//! do not break parsing. Unrecognized enum values map to `Unknown`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Error, Result};

pub const SCHEMA: &str = "fm-fleet-snapshot.v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetSnapshot {
    pub schema: String,
    /// UTC observation time of this run.
    pub generated: String,
    pub fm_home: String,
    #[serde(default)]
    pub backlog: Backlog,
    /// One row per task metadata record, sorted by id.
    #[serde(default)]
    pub tasks: Vec<Task>,
    #[serde(default)]
    pub scout_reports: Vec<ScoutReport>,
    #[serde(default)]
    pub main_inventory: Option<MainInventory>,
    /// Per-secondmate summaries; kept untyped until the daemon projects them.
    #[serde(default)]
    pub secondmate_current: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Backlog {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub present: bool,
    #[serde(default)]
    pub records: Vec<BacklogRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BacklogState {
    InFlight,
    Queued,
    Done,
    #[serde(other)]
    Unknown,
}

/// The engine's single classification of a captain hold. `None` on rows that
/// are not captain holds or are done.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldBucket {
    /// Waiting on the captain now.
    Live,
    /// Deferred with `hold-until` still in the future.
    Dated,
    /// Undated hold older than the engine's aging threshold.
    Aged,
    /// A blocker is still unresolved.
    Blocked,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacklogRecord {
    #[serde(default)]
    pub order: Option<u64>,
    pub state: BacklogState,
    /// False for free-form lines the engine preserved without parsing.
    #[serde(default)]
    pub structured: bool,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub hold_reason: Option<String>,
    #[serde(default)]
    pub hold_kind: Option<String>,
    #[serde(default)]
    pub hold_until: Option<String>,
    #[serde(default)]
    pub hold_set: Option<String>,
    #[serde(default)]
    pub hold_bucket: Option<HoldBucket>,
    #[serde(default)]
    pub hold_age_days: Option<i64>,
    #[serde(default)]
    pub captain_actionable: bool,
    #[serde(default)]
    pub blocked_by_ids: Vec<String>,
    #[serde(default)]
    pub unresolved_blocker_ids: Vec<String>,
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub merged: Option<String>,
    #[serde(default)]
    pub pr_url: Option<String>,
    #[serde(default)]
    pub report_path: Option<String>,
    #[serde(default)]
    pub current_role: Option<String>,
    #[serde(default)]
    pub raw: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub harness: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub yolo: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub paths: TaskPaths,
    /// Engine-reconciled current state. Absent when the task's generation
    /// changed mid-snapshot.
    #[serde(default)]
    pub current_state: Option<CurrentState>,
    #[serde(default)]
    pub endpoint: Option<Endpoint>,
    #[serde(default)]
    pub pr: Option<TaskPr>,
    #[serde(default)]
    pub hints: TaskHints,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskPaths {
    #[serde(default)]
    pub status_log: Option<StatusLogRef>,
    #[serde(default)]
    pub worktree: Option<PathRef>,
    #[serde(default)]
    pub report: Option<PathRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathRef {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub present: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusLogRef {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub present: bool,
    /// Historical wake event only, never current state.
    #[serde(default)]
    pub last_event: Option<LastEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastEvent {
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub raw: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurrentState {
    /// Engine state word, e.g. working, paused, parked, done, failed, unknown.
    pub state: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub raw: Option<String>,
    #[serde(default)]
    pub observed_at: Option<String>,
    #[serde(default)]
    pub freshness: Option<String>,
}

/// A yes/no fact the engine may report as a word instead ("unknown", "not_checked").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Probe {
    Known(bool),
    Word(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Endpoint {
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub exists: Option<Probe>,
    #[serde(default)]
    pub agent_alive: Option<Probe>,
    #[serde(default)]
    pub observed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPr {
    #[serde(default)]
    pub url: Option<String>,
    /// Where the URL came from, e.g. "meta" or "absent".
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub head: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskHints {
    #[serde(default)]
    pub pending_decision: bool,
    #[serde(default)]
    pub blocked_event: bool,
    /// The engine's authoritative keyed open-decision fold for this task.
    #[serde(default)]
    pub open_decisions: Vec<OpenDecisionHint>,
    #[serde(default)]
    pub scout_report_present: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenDecisionHint {
    #[serde(default)]
    pub key: Option<String>,
    pub verb: String,
    #[serde(default)]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoutReport {
    pub id: String,
    pub path: String,
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MainInventory {
    #[serde(default)]
    pub valid: bool,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub orphan_in_flight: Vec<String>,
    #[serde(default)]
    pub unstructured_current_count: u64,
}

/// Parse snapshot JSON, refusing any schema other than [`SCHEMA`].
pub fn parse(bytes: &[u8]) -> Result<FleetSnapshot> {
    let value: Value = serde_json::from_slice(bytes).map_err(|source| Error::Json {
        what: "fleet snapshot",
        source,
    })?;
    let found = value.get("schema").and_then(Value::as_str);
    if found != Some(SCHEMA) {
        return Err(Error::Schema {
            what: "fleet snapshot",
            found: found.map(str::to_string),
            expected: SCHEMA,
        });
    }
    serde_json::from_value(value).map_err(|source| Error::Json {
        what: "fleet snapshot",
        source,
    })
}

impl FleetSnapshot {
    pub fn task(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    /// Structured backlog rows only.
    pub fn backlog_items(&self) -> impl Iterator<Item = &BacklogRecord> {
        self.backlog
            .records
            .iter()
            .filter(|r| r.structured && r.id.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_other_schema() {
        let err = parse(br#"{"schema":"fm-fleet-snapshot.v2","generated":"x","fm_home":"/h"}"#)
            .unwrap_err();
        assert!(
            matches!(err, Error::Schema { found: Some(ref s), .. } if s == "fm-fleet-snapshot.v2")
        );
        assert!(matches!(
            parse(b"{}"),
            Err(Error::Schema { found: None, .. })
        ));
        assert!(matches!(parse(b"not json"), Err(Error::Json { .. })));
    }

    #[test]
    fn minimal_snapshot_parses() {
        let s =
            parse(br#"{"schema":"fm-fleet-snapshot.v1","generated":"x","fm_home":"/h"}"#).unwrap();
        assert!(s.tasks.is_empty());
        assert!(!s.backlog.present);
    }

    #[test]
    fn unknown_enum_words_do_not_fail() {
        let s = parse(
            br#"{"schema":"fm-fleet-snapshot.v1","generated":"x","fm_home":"/h",
            "backlog":{"present":true,"records":[{"state":"archived","structured":true,"id":"a","hold_bucket":"frozen"}]}}"#,
        )
        .unwrap();
        let r = &s.backlog.records[0];
        assert_eq!(r.state, BacklogState::Unknown);
        assert_eq!(r.hold_bucket, Some(HoldBucket::Unknown));
    }
}
