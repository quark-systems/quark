//! Carrying on after a passing API error.
//!
//! A worker whose turn dies on an error that passes by itself (the Mac
//! slept mid-response, the network dropped; see
//! [`quark_transcript::ApiError`]) sits at its prompt until someone types.
//! Nothing else notices: the supervisor reports it stale, which the away
//! policy keeps quiet. On Matt's Mac two Selah workers sat like that for
//! nine days.
//!
//! When the transcript tap reads such an error at the end of a worker's
//! log, the worker is asked to carry on, after [`SETTLE`] so the network
//! that just came back has settled. A worker that moves on by itself in the
//! meantime is left alone. A worker is nudged at most [`MAX_NUDGES`] times
//! in [`WINDOW`], so one whose error keeps coming back stops being nudged
//! and waits for a person.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long after the error the worker is nudged.
pub const SETTLE: Duration = Duration::from_secs(2 * 60);
/// Most nudges a worker gets in [`WINDOW`].
pub const MAX_NUDGES: usize = 3;
pub const WINDOW: Duration = Duration::from_secs(6 * 60 * 60);
/// What the worker is told.
pub const MESSAGE: &str = "Your last turn ended on an API error (network or sleep), not on \
anything you did. Carry on from where you stopped.";

/// A worker waiting to be nudged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Due {
    pub project_id: String,
    pub task_id: String,
    /// The engine's id for the task, which steering takes.
    pub engine_id: String,
    pub error: String,
}

#[derive(Default)]
pub struct Nudges {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// By task id: the worker and when its error was read.
    pending: HashMap<String, (Due, Instant)>,
    /// By task id: when it was nudged, within [`WINDOW`].
    sent: HashMap<String, Vec<Instant>>,
}

impl Nudges {
    /// The tap read a passing API error at the end of the worker's log.
    pub fn stopped(&self, due: Due, now: Instant) {
        let mut s = self.state.lock().unwrap();
        s.pending.insert(due.task_id.clone(), (due, now));
    }

    /// The worker's log moved on without stopping at an error.
    pub fn progressed(&self, task_id: &str) {
        self.state.lock().unwrap().pending.remove(task_id);
    }

    /// `project_id`'s workers to nudge now; each is taken off the pending
    /// list. Workers past their allowance are dropped and logged.
    pub fn take_due(&self, project_id: &str, now: Instant) -> Vec<Due> {
        let mut s = self.state.lock().unwrap();
        let ready: Vec<String> = s
            .pending
            .iter()
            .filter(|(_, (due, at))| {
                due.project_id == project_id && now.duration_since(*at) >= SETTLE
            })
            .map(|(id, _)| id.clone())
            .collect();
        let mut out = Vec::new();
        for id in ready {
            let (due, _) = s.pending.remove(&id).expect("listed above");
            let sent = s.sent.entry(id).or_default();
            sent.retain(|t| now.duration_since(*t) < WINDOW);
            if sent.len() >= MAX_NUDGES {
                tracing::warn!(
                    project = %due.project_id,
                    task = %due.task_id,
                    error = %due.error,
                    "worker keeps stopping on API errors; leaving it for a person"
                );
                continue;
            }
            sent.push(now);
            out.push(due);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn due(task: &str) -> Due {
        Due {
            project_id: "prj_1".into(),
            task_id: task.into(),
            engine_id: format!("e-{task}"),
            error: "API Error: slept".into(),
        }
    }

    #[test]
    fn nudges_after_settling_unless_the_worker_moved_on() {
        let n = Nudges::default();
        let t0 = Instant::now();
        n.stopped(due("a"), t0);
        n.stopped(due("b"), t0);
        assert!(n.take_due("prj_1", t0 + Duration::from_secs(10)).is_empty());
        n.progressed("b");
        assert!(n.take_due("prj_2", t0 + SETTLE).is_empty());
        assert_eq!(n.take_due("prj_1", t0 + SETTLE), [due("a")]);
        // Taken once.
        assert!(n.take_due("prj_1", t0 + SETTLE * 2).is_empty());
    }

    #[test]
    fn stops_nudging_a_worker_that_keeps_failing() {
        let n = Nudges::default();
        let mut t = Instant::now();
        for _ in 0..MAX_NUDGES {
            n.stopped(due("a"), t);
            t += SETTLE;
            assert_eq!(n.take_due("prj_1", t).len(), 1);
        }
        n.stopped(due("a"), t);
        assert!(n.take_due("prj_1", t + SETTLE).is_empty());
        // A fresh allowance once the window has passed.
        n.stopped(due("a"), t + WINDOW);
        assert_eq!(n.take_due("prj_1", t + WINDOW + SETTLE).len(), 1);
    }
}
