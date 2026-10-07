//! Reading a commit's checks: is the default branch green, and which checks
//! has a pull request's head passed.
//!
//! The rules match firstmate's `fm_main_health_read` (bin/fm-main-health-lib.sh)
//! so shadow mode compares like with like:
//!
//! - Only the newest run of each check name counts, so a green re-run
//!   supersedes a failure.
//! - A completed run concluding `failure`, `timed_out`, `startup_failure` or
//!   `action_required`, or a status in `failure` or `error`, makes the commit
//!   red. `cancelled`, `neutral`, `skipped` and `stale` are not evidence that
//!   anything is broken.
//! - Otherwise anything still running makes it pending, any checks at all make
//!   it green, and a commit with no checks is `none`, which counts as green.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// One check run as GitHub's `commits/{sha}/check-runs` lists it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRun {
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub name: Option<String>,
    /// `queued`, `in_progress`, `completed`, ...
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub conclusion: Option<String>,
}

impl CheckRun {
    pub fn new(id: u64, name: &str, status: &str, conclusion: Option<&str>) -> Self {
        Self {
            id,
            name: Some(name.into()),
            status: Some(status.into()),
            conclusion: conclusion.map(str::to_string),
        }
    }
}

/// One commit status as GitHub's combined `commits/{sha}/status` lists it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitStatus {
    #[serde(default)]
    pub context: Option<String>,
    /// `success`, `pending`, `failure` or `error`.
    #[serde(default)]
    pub state: Option<String>,
}

impl CommitStatus {
    pub fn new(context: &str, state: &str) -> Self {
        Self {
            context: Some(context.into()),
            state: Some(state.into()),
        }
    }
}

/// A commit's overall check state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Green,
    Red,
    Pending,
    /// No checks at all; counts as green.
    None,
}

impl HealthState {
    pub fn as_str(self) -> &'static str {
        match self {
            HealthState::Green => "green",
            HealthState::Red => "red",
            HealthState::Pending => "pending",
            HealthState::None => "none",
        }
    }

    /// Parse firstmate's spelling; `None` for anything else, including the
    /// empty string firstmate records for an unreadable tip.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "green" => Some(HealthState::Green),
            "red" => Some(HealthState::Red),
            "pending" => Some(HealthState::Pending),
            "none" => Some(HealthState::None),
            _ => None,
        }
    }

    /// Green or no checks: nothing reports the commit broken.
    pub fn is_clear(self) -> bool {
        matches!(self, HealthState::Green | HealthState::None)
    }
}

/// One read of a branch tip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthRead {
    pub tip: String,
    pub state: HealthState,
    /// The failing checks, sorted and unique; empty unless `state` is red.
    pub red: Vec<String>,
}

const RED_CONCLUSIONS: [&str; 4] = ["failure", "timed_out", "startup_failure", "action_required"];
const GREEN_CONCLUSIONS: [&str; 3] = ["success", "neutral", "skipped"];

/// The newest run of each check name.
fn newest_runs(runs: &[CheckRun]) -> Vec<&CheckRun> {
    let mut by_name: BTreeMap<&str, &CheckRun> = BTreeMap::new();
    for run in runs {
        let key = run.name.as_deref().unwrap_or("");
        match by_name.get(key) {
            Some(seen) if seen.id >= run.id => {}
            _ => {
                by_name.insert(key, run);
            }
        }
    }
    by_name.into_values().collect()
}

/// Classify the checks on `tip`.
pub fn classify(tip: &str, runs: &[CheckRun], statuses: &[CommitStatus]) -> HealthRead {
    let mut red = BTreeSet::new();
    let mut pending = false;
    let mut any = false;
    for run in newest_runs(runs) {
        any = true;
        let completed = run.status.as_deref() == Some("completed");
        let conclusion = run.conclusion.as_deref().unwrap_or("");
        if completed && RED_CONCLUSIONS.contains(&conclusion) {
            red.insert(clean(run.name.as_deref().unwrap_or("(unnamed check)")));
        }
        if !completed {
            pending = true;
        }
    }
    for status in statuses {
        any = true;
        let state = status.state.as_deref().unwrap_or("");
        if matches!(state, "failure" | "error") {
            red.insert(clean(
                status.context.as_deref().unwrap_or("(unnamed status)"),
            ));
        }
        if state == "pending" {
            pending = true;
        }
    }
    let state = if !red.is_empty() {
        HealthState::Red
    } else if pending {
        HealthState::Pending
    } else if any {
        HealthState::Green
    } else {
        HealthState::None
    };
    HealthRead {
        tip: tip.to_string(),
        state,
        red: red.into_iter().collect(),
    }
}

/// The check names that passed on a commit: the newest run of each name
/// completed as success, neutral or skipped, and every successful status.
/// A pull request turns a red main green only when this covers every check
/// failing on main.
pub fn green_checks(runs: &[CheckRun], statuses: &[CommitStatus]) -> BTreeSet<String> {
    let mut green = BTreeSet::new();
    for run in newest_runs(runs) {
        let completed = run.status.as_deref() == Some("completed");
        let conclusion = run.conclusion.as_deref().unwrap_or("");
        if completed && GREEN_CONCLUSIONS.contains(&conclusion) {
            if let Some(name) = &run.name {
                green.insert(name.clone());
            }
        }
    }
    for status in statuses {
        if status.state.as_deref() == Some("success") {
            if let Some(context) = &status.context {
                green.insert(context.clone());
            }
        }
    }
    green
}

fn clean(name: &str) -> String {
    name.replace(['\n', '\r'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: u64, name: &str, status: &str, conclusion: Option<&str>) -> CheckRun {
        CheckRun::new(id, name, status, conclusion)
    }

    #[test]
    fn classifies_like_firstmate() {
        assert_eq!(classify("t", &[], &[]).state, HealthState::None);
        let r = classify(
            "t",
            &[
                run(1, "ci", "completed", Some("success")),
                run(2, "lint", "completed", Some("skipped")),
            ],
            &[],
        );
        assert_eq!(r.state, HealthState::Green);
        let r = classify(
            "t",
            &[
                run(1, "ci", "completed", Some("success")),
                run(2, "lint", "in_progress", None),
            ],
            &[],
        );
        assert_eq!(r.state, HealthState::Pending);
        let r = classify(
            "t",
            &[
                run(1, "ci", "completed", Some("failure")),
                run(2, "lint", "completed", Some("timed_out")),
                run(3, "deploy", "completed", Some("cancelled")),
            ],
            &[],
        );
        assert_eq!(r.state, HealthState::Red);
        assert_eq!(r.red, vec!["ci", "lint"]);
        // The newest run of a name decides it.
        let r = classify(
            "t",
            &[
                run(1, "ci", "completed", Some("failure")),
                run(2, "ci", "completed", Some("success")),
            ],
            &[],
        );
        assert_eq!(r.state, HealthState::Green);
        let r = classify("t", &[], &[CommitStatus::new("ext/ci", "error")]);
        assert_eq!(r.state, HealthState::Red);
        assert_eq!(r.red, vec!["ext/ci"]);
        let r = classify("t", &[], &[CommitStatus::new("ext/ci", "pending")]);
        assert_eq!(r.state, HealthState::Pending);
    }

    #[test]
    fn green_checks_take_the_newest_run() {
        let g = green_checks(
            &[
                run(1, "build", "completed", Some("success")),
                run(2, "build", "completed", Some("failure")),
                run(3, "ci", "completed", Some("neutral")),
                run(4, "e2e", "in_progress", None),
            ],
            &[
                CommitStatus::new("ext", "success"),
                CommitStatus::new("other", "failure"),
            ],
        );
        assert_eq!(
            g.into_iter().collect::<Vec<_>>(),
            vec!["ci".to_string(), "ext".to_string()]
        );
    }
}
