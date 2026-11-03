//! The fleet check: a timed sweep for workers that are stuck.
//!
//! Firstmate's watcher had a heartbeat that looked over the whole fleet
//! every ten minutes, whatever else happened. In the native engine the
//! supervisor relaunches workers whose sessions end and [`crate::nudge`]
//! carries on after passing API errors, but a worker that just sits (at its
//! prompt after a turn it thought was final, on a question it put in prose
//! instead of asking, on a menu) was only reported stale, which the away
//! policy keeps quiet. Nobody looked.
//!
//! Every [`INTERVAL`], each running worker that has been quiet (no screen
//! change, no worker message) for the supervisor's `stale_after`:
//!
//! 1. is nudged: told it has been quiet and asked to report what it waits
//!    on or carry on. At most once in [`NUDGE_EVERY`], and never while its
//!    screen shows a dialog or menu, where typed text could pick an option;
//! 2. when it is quiet that long again after its nudge, or it shows a
//!    dialog, the coordinator is woken with what its screen shows, to
//!    steer, answer, relaunch or cancel it, or ask the user. At most once in
//!    [`WAKE_EVERY`] per worker, so one the coordinator chose to leave
//!    waiting does not wake it every pass.
//!
//! `QUARK_FLEET_CHECK_SECS` changes the interval; 0 turns the check off.
//! What it remembers is in memory: after a daemon restart quiet time counts
//! again from zero, so the worst case is one more nudge.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use quark_coordinator::Coordinator;
use quark_core::{ProjectId, TaskId};
use quark_supervisor::{Nudge, Quiet, Supervisor};

/// How often the fleet is checked.
pub const INTERVAL: Duration = Duration::from_secs(5 * 60);
/// Most one nudge per worker in this long.
pub const NUDGE_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// Most one coordinator wake per worker in this long.
pub const WAKE_EVERY: Duration = Duration::from_secs(2 * 60 * 60);
/// Who the coordinator is told asks.
pub const BY: &str = "the fleet check";
/// Screen lines a wake quotes.
const SCREEN_LINES: usize = 15;

/// [`INTERVAL`], or `QUARK_FLEET_CHECK_SECS` when set; `None` (off) when
/// that is 0.
pub fn interval_from_env() -> Option<Duration> {
    let secs = std::env::var("QUARK_FLEET_CHECK_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok());
    match secs {
        Some(0) => None,
        Some(s) => Some(Duration::from_secs(s)),
        None => Some(INTERVAL),
    }
}

/// What the fleet check does about one quiet worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    Nudge(Quiet),
    /// Wake the coordinator; `nudged` is how long ago the worker was.
    Wake {
        quiet: Quiet,
        nudged: Option<Duration>,
    },
}

#[derive(Debug, Clone, Default)]
struct Seen {
    generation: String,
    nudged: Option<Instant>,
    woken: Option<Instant>,
}

/// Which quiet workers to nudge and which to wake the coordinator about.
#[derive(Default)]
pub struct Plan {
    seen: Mutex<HashMap<(ProjectId, TaskId), Seen>>,
}

impl Plan {
    /// Decide for every running worker in `quiet`; workers no longer
    /// listed are forgotten. A worker is stuck once quiet for `after`.
    pub fn decide(&self, quiet: &[Quiet], after: Duration, now: Instant) -> Vec<Act> {
        let mut seen = self.seen.lock().unwrap();
        seen.retain(|k, _| quiet.iter().any(|q| (&q.project, &q.task) == (&k.0, &k.1)));
        let mut acts = Vec::new();
        for q in quiet {
            let s = seen.entry((q.project.clone(), q.task.clone())).or_default();
            if s.generation != q.generation {
                // A new worker for the task starts with a clean slate.
                *s = Seen {
                    generation: q.generation.clone(),
                    ..Seen::default()
                };
            }
            if q.idle < after {
                continue;
            }
            let since = |t: Option<Instant>| t.map(|t| now.saturating_duration_since(t));
            match since(s.nudged) {
                Some(ago) if ago < NUDGE_EVERY => {
                    // Quiet a whole stretch again after its nudge.
                    if ago < after || since(s.woken).is_some_and(|w| w < WAKE_EVERY) {
                        continue;
                    }
                    s.woken = Some(now);
                    acts.push(Act::Wake {
                        quiet: q.clone(),
                        nudged: Some(ago),
                    });
                }
                _ => {
                    s.nudged = Some(now);
                    acts.push(Act::Nudge(q.clone()));
                }
            }
        }
        acts
    }

    /// The nudge for `q` found a dialog: wake the coordinator instead,
    /// unless it was woken for this worker recently. Returns whether to.
    pub fn dialog(&self, q: &Quiet, now: Instant) -> bool {
        let mut seen = self.seen.lock().unwrap();
        let Some(s) = seen.get_mut(&(q.project.clone(), q.task.clone())) else {
            return false;
        };
        if s.woken
            .is_some_and(|w| now.saturating_duration_since(w) < WAKE_EVERY)
        {
            return false;
        }
        s.woken = Some(now);
        true
    }
}

/// What a worker is told when it is nudged.
pub fn nudge_message(idle: Duration) -> String {
    format!(
        "Quark's fleet check: nothing has happened in your session for {} minutes. If you \
         are waiting on something, report it (blocked, or ask your question). If your work \
         is finished, report done. Otherwise carry on.",
        idle.as_secs() / 60
    )
}

/// What the coordinator is told about a stuck worker.
pub fn wake_note(q: &Quiet, nudged: Option<Duration>, screen: Option<&str>) -> String {
    let mins = |d: Duration| d.as_secs() / 60;
    let mut note = match nudged {
        Some(ago) => format!(
            "this worker has been stuck: quiet again for {} minutes after it was nudged {} \
             minutes ago.",
            mins(q.idle),
            mins(ago)
        ),
        None => format!(
            "this worker has been quiet for {} minutes on a dialog or menu, so it was not \
             nudged.",
            mins(q.idle)
        ),
    };
    note.push_str(
        " Look at it and unblock it: steer it, answer what it waits on, relaunch or cancel \
         it, or ask the user.",
    );
    if let Some(screen) = screen {
        let lines: Vec<&str> = screen.lines().filter(|l| !l.trim().is_empty()).collect();
        let tail = &lines[lines.len().saturating_sub(SCREEN_LINES)..];
        if !tail.is_empty() {
            note.push_str("\nIts screen ends:\n```\n");
            note.push_str(&tail.join("\n"));
            note.push_str("\n```");
        }
    }
    note
}

pub struct FleetCheck {
    supervisor: Arc<Supervisor>,
    coordinator: Option<Arc<Coordinator>>,
    plan: Plan,
}

impl FleetCheck {
    pub fn new(supervisor: Arc<Supervisor>, coordinator: Option<Arc<Coordinator>>) -> Self {
        Self {
            supervisor,
            coordinator,
            plan: Plan::default(),
        }
    }

    /// One sweep. Returns what it did.
    pub async fn check(&self) -> Vec<Act> {
        let after = self.supervisor.config().stale_after;
        let now = Instant::now();
        let acts = self
            .plan
            .decide(&self.supervisor.quiet_workers(), after, now);
        let mut done = Vec::new();
        for act in acts {
            match act {
                Act::Nudge(q) => {
                    let message = nudge_message(q.idle);
                    match self.supervisor.nudge(&q.project, &q.task, &message).await {
                        Ok(Nudge::Sent(_)) => {
                            tracing::info!(project = %q.project, task = %q.task, idle_secs = q.idle.as_secs(), "nudged a quiet worker");
                            done.push(Act::Nudge(q));
                        }
                        Ok(Nudge::Dialog(screen)) => {
                            if self.plan.dialog(&q, now) {
                                self.wake(&q, None, Some(&screen)).await;
                                done.push(Act::Wake {
                                    quiet: q,
                                    nudged: None,
                                });
                            }
                        }
                        Err(e) => {
                            tracing::warn!(project = %q.project, task = %q.task, error = %e, "could not nudge a quiet worker")
                        }
                    }
                }
                Act::Wake { quiet, nudged } => {
                    let screen = self.supervisor.screen(&quiet.project, &quiet.task).await;
                    self.wake(&quiet, nudged, screen.ok().as_deref()).await;
                    done.push(Act::Wake { quiet, nudged });
                }
            }
        }
        done
    }

    async fn wake(&self, q: &Quiet, nudged: Option<Duration>, screen: Option<&str>) {
        let Some(c) = &self.coordinator else {
            tracing::warn!(project = %q.project, task = %q.task, "a worker is stuck and no coordinator runs to tell");
            return;
        };
        let note = wake_note(q, nudged, screen);
        match c.request(&q.project, Some(q.task.clone()), &note, BY).await {
            Ok(_) => {
                tracing::info!(project = %q.project, task = %q.task, "woke the coordinator about a stuck worker")
            }
            Err(e) => {
                tracing::warn!(project = %q.project, task = %q.task, error = %e, "could not wake the coordinator about a stuck worker")
            }
        }
    }

    /// Check every `every` until the task is aborted.
    pub async fn run(self, every: Duration) {
        let mut timer = tokio::time::interval(every);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The first tick is immediate; the supervisor has watched nothing yet.
        timer.tick().await;
        loop {
            timer.tick().await;
            self.check().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AFTER: Duration = Duration::from_secs(15 * 60);

    fn quiet(task: &str, generation: &str, mins: u64) -> Quiet {
        Quiet {
            project: ProjectId::from("p"),
            task: TaskId::from(task),
            generation: generation.into(),
            idle: Duration::from_secs(mins * 60),
        }
    }

    fn mins(n: u64) -> Duration {
        Duration::from_secs(n * 60)
    }

    fn kinds(acts: &[Act]) -> Vec<&'static str> {
        acts.iter()
            .map(|a| match a {
                Act::Nudge(_) => "nudge",
                Act::Wake { .. } => "wake",
            })
            .collect()
    }

    #[test]
    fn a_quiet_worker_is_nudged_then_the_coordinator_is_woken() {
        let plan = Plan::default();
        let t0 = Instant::now();
        assert!(plan.decide(&[quiet("t", "g", 10)], AFTER, t0).is_empty());
        assert_eq!(
            kinds(&plan.decide(&[quiet("t", "g", 16)], AFTER, t0 + mins(5))),
            ["nudge"]
        );
        // The nudge was typed, so the screen moved; not quiet long enough.
        assert!(plan
            .decide(&[quiet("t", "g", 5)], AFTER, t0 + mins(10))
            .is_empty());
        // A screen the nudge did not move is not taken as a new stretch.
        assert!(plan
            .decide(&[quiet("t", "g", 26)], AFTER, t0 + mins(15))
            .is_empty());
        let acts = plan.decide(&[quiet("t", "g", 16)], AFTER, t0 + mins(25));
        assert_eq!(kinds(&acts), ["wake"]);
        assert!(matches!(&acts[0], Act::Wake { nudged: Some(d), .. } if *d == mins(20)));
        // Woken once; still stuck later does not wake it every pass.
        assert!(plan
            .decide(&[quiet("t", "g", 40)], AFTER, t0 + mins(50))
            .is_empty());
        assert_eq!(
            kinds(&plan.decide(&[quiet("t", "g", 150)], AFTER, t0 + mins(150))),
            ["wake"]
        );
    }

    #[test]
    fn a_worker_that_carries_on_is_left_alone() {
        let plan = Plan::default();
        let t0 = Instant::now();
        assert_eq!(
            kinds(&plan.decide(&[quiet("t", "g", 15)], AFTER, t0)),
            ["nudge"]
        );
        for i in 1..10 {
            assert!(plan
                .decide(&[quiet("t", "g", 2)], AFTER, t0 + mins(5 * i))
                .is_empty());
        }
    }

    #[test]
    fn a_new_worker_or_a_finished_task_starts_over() {
        let plan = Plan::default();
        let t0 = Instant::now();
        assert_eq!(
            kinds(&plan.decide(&[quiet("t", "g1", 15)], AFTER, t0)),
            ["nudge"]
        );
        // Relaunched: the new generation gets its own nudge.
        assert_eq!(
            kinds(&plan.decide(&[quiet("t", "g2", 15)], AFTER, t0 + mins(20))),
            ["nudge"]
        );
        // Gone from the running list, then back: forgotten in between.
        assert!(plan.decide(&[], AFTER, t0 + mins(25)).is_empty());
        assert_eq!(
            kinds(&plan.decide(&[quiet("t", "g2", 15)], AFTER, t0 + mins(40))),
            ["nudge"]
        );
    }

    #[test]
    fn nudges_are_rationed() {
        let plan = Plan::default();
        let t0 = Instant::now();
        assert_eq!(
            kinds(&plan.decide(&[quiet("t", "g", 15)], AFTER, t0)),
            ["nudge"]
        );
        // It answered, then went quiet again: the coordinator hears of it.
        assert_eq!(
            kinds(&plan.decide(&[quiet("t", "g", 15)], AFTER, t0 + mins(60))),
            ["wake"]
        );
        // Past the nudge window it is nudged again first.
        assert_eq!(
            kinds(&plan.decide(&[quiet("t", "g", 15)], AFTER, t0 + NUDGE_EVERY)),
            ["nudge"]
        );
    }

    #[test]
    fn a_dialog_wakes_the_coordinator_once_in_a_while() {
        let plan = Plan::default();
        let t0 = Instant::now();
        let q = quiet("t", "g", 15);
        plan.decide(std::slice::from_ref(&q), AFTER, t0);
        assert!(plan.dialog(&q, t0));
        assert!(!plan.dialog(&q, t0 + mins(30)));
        assert!(plan.dialog(&q, t0 + WAKE_EVERY));
    }

    #[test]
    fn the_wake_note_quotes_the_end_of_the_screen() {
        let screen = (1..=30)
            .map(|i| format!("line {i}\n\n"))
            .collect::<String>();
        let note = wake_note(&quiet("t", "g", 16), Some(mins(20)), Some(&screen));
        assert!(note.starts_with("this worker has been stuck: quiet again for 16 minutes"));
        assert!(note.contains("```\nline 16\n"));
        assert!(!note.contains("line 15\n"));
        assert!(note.ends_with("line 30\n```"));
        let note = wake_note(&quiet("t", "g", 16), None, None);
        assert!(note.contains("on a dialog or menu"));
        assert!(!note.contains("```"));
    }
}
