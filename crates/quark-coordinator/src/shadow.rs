//! Slice 6's comparison: did the native coordinator want to wake whenever
//! firstmate's coordinator acted on a wake?
//!
//! The native coordinator is woken only for judgment ([`crate::wake`]), so
//! it should have recorded a `would_wake` near every wake turn in which
//! firstmate's coordinator acted ([`BaselineTurn::acts`]). [`WakeCoverage`]
//! reads both from the event log and reports each acting wake turn with no
//! `would_wake` for its Project within [`WINDOW`] either side as a
//! [`Miss`].
//!
//! The opposite is not a miss: a `would_wake` where firstmate only
//! acknowledged status, or did not wake at all, costs tokens, and the
//! Metrics tab's Coordinator section counts those. Turns the user started
//! are not compared, because the user wakes both.
//!
//! It is a heuristic: whether a turn acted is read from firstmate's
//! transcript ([`crate::baseline`]), and the two sides are matched by time,
//! not by cause. A turn is judged once the log has moved [`WINDOW`] past
//! it, and only when the shadow was running for the whole [`WINDOW`] either
//! side of it, so the transcript's history from before the shadow started,
//! and turns firstmate took while the daemon was down, are not judged.
//!
//! The fold learns when the daemon ran from its starts: each start is
//! recorded before the daemon writes anything else
//! ([`WakeCoverage::daemon_started`]), so the run before it ended at the
//! last event the log holds ahead of it. That end is early when the log was
//! quiet before the daemon stopped, which only leaves more turns unjudged.

use std::collections::{BTreeMap, VecDeque};

use quark_core::{Event, ProjectId};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

use crate::baseline::{BaselineTurn, Cause};
use crate::events::CoordinatorEvent;

/// How far apart a firstmate wake turn and a native `would_wake` may be.
pub const WINDOW: Duration = Duration::minutes(10);

/// An acting firstmate wake turn the native coordinator would have slept
/// through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Miss {
    pub project: ProjectId,
    pub turn: BaselineTurn,
}

/// The fold that finds [`Miss`]es, fed every event in log order.
#[derive(Default)]
pub struct WakeCoverage {
    /// Daemon runs, oldest first.
    runs: Vec<Run>,
    /// The newest event time folded.
    last: Option<OffsetDateTime>,
    projects: BTreeMap<ProjectId, Project>,
}

/// One daemon run.
struct Run {
    start: OffsetDateTime,
    /// The last event before the next run started; `None` while running.
    end: Option<OffsetDateTime>,
    /// Whether it ran the coordinator's shadow; `None` until it says.
    shadow: Option<bool>,
}

#[derive(Default)]
struct Project {
    /// When each `would_wake` was recorded, oldest first, trimmed to what
    /// a waiting turn could still match.
    wakes: VecDeque<OffsetDateTime>,
    /// Acting wake turns not judged yet, oldest first.
    waiting: Vec<(OffsetDateTime, BaselineTurn)>,
}

impl WakeCoverage {
    pub fn new() -> Self {
        Self::default()
    }

    /// A daemon started at `at`, before writing anything else: the run
    /// before it ended with the last event folded.
    pub fn daemon_started(&mut self, at: OffsetDateTime) {
        self.open(at, None);
    }

    /// A daemon started at `at` running the coordinator's shadow or not.
    /// It says so after [`Self::daemon_started`]; a log from before those
    /// were recorded has only this, and its run starts here.
    pub fn started(&mut self, at: OffsetDateTime, coordinator: bool) {
        match self.runs.last_mut() {
            Some(run) if run.end.is_none() && run.shadow.is_none() => {
                run.shadow = Some(coordinator)
            }
            _ => self.open(at, Some(coordinator)),
        }
        self.seen(at);
    }

    fn open(&mut self, at: OffsetDateTime, shadow: Option<bool>) {
        let last = self.last;
        if let Some(run) = self.runs.last_mut() {
            run.end
                .get_or_insert(last.unwrap_or(run.start).max(run.start));
        }
        self.runs.push(Run {
            start: at,
            end: None,
            shadow,
        });
        self.seen(at);
    }

    fn seen(&mut self, at: OffsetDateTime) {
        self.last = Some(self.last.map_or(at, |l| l.max(at)));
    }

    /// Fold `e` and return the turns it proves were missed.
    pub fn apply(&mut self, e: &Event) -> Vec<Miss> {
        self.seen(e.ts);
        if e.kind
            .as_str()
            .strip_prefix(crate::events::PREFIX)
            .is_some_and(|k| k.starts_with('.'))
        {
            match e.decode::<CoordinatorEvent>() {
                Ok(CoordinatorEvent::WouldWake { .. }) => {
                    let p = self.projects.entry(e.project.clone()).or_default();
                    p.wakes.push_back(e.ts);
                }
                Ok(CoordinatorEvent::Baseline { turn })
                    if turn.cause == Cause::Wake && turn.acts =>
                {
                    if let Ok(at) = OffsetDateTime::parse(&turn.at, &Rfc3339) {
                        let p = self.projects.entry(e.project.clone()).or_default();
                        p.waiting.push((at, turn));
                    }
                }
                _ => {}
            }
        }
        self.judge(e.ts)
    }

    /// Judge every waiting turn the log has moved [`WINDOW`] past.
    fn judge(&mut self, now: OffsetDateTime) -> Vec<Miss> {
        let mut missed = Vec::new();
        let runs = &self.runs;
        for (project, p) in &mut self.projects {
            let wakes = &p.wakes;
            p.waiting.retain(|(at, turn)| {
                let covered = wakes
                    .iter()
                    .any(|w| *w >= *at - WINDOW && *w <= *at + WINDOW);
                if covered {
                    return false;
                }
                if now - *at <= WINDOW {
                    return true;
                }
                if !watched(runs, *at) {
                    return false;
                }
                missed.push(Miss {
                    project: project.clone(),
                    turn: turn.clone(),
                });
                false
            });
            // A turn read later can be older than the newest wake, so keep
            // wakes a while; the transcript is read on the same timer.
            let horizon = now - WINDOW * 6;
            while p.wakes.front().is_some_and(|w| *w < horizon) {
                p.wakes.pop_front();
            }
        }
        missed
    }
}

/// Whether a turn at `at` could have been seen: the run it fell in ran the
/// shadow from a [`WINDOW`] before it to a [`WINDOW`] after it.
fn watched(runs: &[Run], at: OffsetDateTime) -> bool {
    runs.iter()
        .rev()
        .find(|run| run.start <= at)
        .is_some_and(|run| {
            run.shadow == Some(true)
                && at - run.start >= WINDOW
                && run.end.is_none_or(|end| at + WINDOW <= end)
        })
}

#[cfg(test)]
mod tests {
    use quark_core::{HostId, NewEvent, Seq};

    use super::*;
    use crate::events::Usage;
    use crate::wake::{Need, WakeItem};

    const T0: i64 = 1_790_000_000;

    fn at(min: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(T0 + min * 60).unwrap()
    }

    fn event(min: i64, e: &CoordinatorEvent) -> Event {
        let mut ev = NewEvent::typed(HostId::from("h"), ProjectId::from("p"), None, e.kind(), e)
            .unwrap()
            .with_seq(Seq(1));
        ev.ts = at(min);
        ev
    }

    fn turn(min: i64, cause: Cause, acts: bool) -> CoordinatorEvent {
        CoordinatorEvent::Baseline {
            turn: BaselineTurn {
                id: format!("u{min}"),
                at: at(min).format(&Rfc3339).unwrap(),
                cause,
                usage: Usage::default(),
                tool_calls: 1,
                acts,
            },
        }
    }

    fn would_wake() -> CoordinatorEvent {
        CoordinatorEvent::WouldWake {
            items: vec![WakeItem {
                source: Seq(1),
                task: None,
                need: Need::Done,
                summary: "t: done".into(),
                key: "k".into(),
            }],
        }
    }

    fn tick(min: i64) -> Event {
        event(min, &CoordinatorEvent::Cursor { through: Seq(1) })
    }

    #[test]
    fn an_acting_wake_with_no_native_wake_is_a_miss() {
        let mut c = WakeCoverage::new();
        c.started(at(0), true);
        // Read from the transcript a little after it happened.
        assert!(c.apply(&event(22, &turn(20, Cause::Wake, true))).is_empty());
        assert!(c.apply(&tick(29)).is_empty(), "still inside the window");
        let missed = c.apply(&tick(31));
        assert_eq!(missed.len(), 1);
        assert_eq!(missed[0].turn.id, "u20");
        assert!(c.apply(&tick(60)).is_empty(), "judged once");
    }

    #[test]
    fn a_native_wake_either_side_covers_it() {
        let mut c = WakeCoverage::new();
        c.started(at(0), true);
        c.apply(&event(18, &would_wake()));
        c.apply(&event(22, &turn(20, Cause::Wake, true)));
        // And one that is covered by a wake recorded after it was read.
        c.apply(&event(41, &turn(40, Cause::Wake, true)));
        c.apply(&event(45, &would_wake()));
        assert!(c.apply(&tick(90)).is_empty());
    }

    #[test]
    fn user_turns_acks_and_unwatched_turns_are_not_judged() {
        let mut c = WakeCoverage::new();
        c.started(at(0), true);
        c.apply(&event(30, &turn(5, Cause::Wake, true)));
        c.apply(&event(30, &turn(20, Cause::User, true)));
        c.apply(&event(30, &turn(21, Cause::Wake, false)));
        c.started(at(40), false);
        c.apply(&event(60, &turn(50, Cause::Wake, true)));
        assert!(c.apply(&tick(120)).is_empty());
    }

    #[test]
    fn turns_while_the_daemon_was_down_are_not_judged() {
        let mut c = WakeCoverage::new();
        c.daemon_started(at(0));
        c.started(at(0), true);
        assert!(c.apply(&tick(30)).is_empty());
        // The daemon stops after 30; one turn was read just before.
        assert!(c.apply(&event(31, &turn(28, Cause::Wake, true))).is_empty());
        // Back at 90: the new run's first events come before its shadows.
        c.daemon_started(at(90));
        assert!(c.apply(&tick(90)).is_empty());
        c.started(at(90), true);
        // Turns read from the transcript after the restart, one while down.
        assert!(c.apply(&event(91, &turn(60, Cause::Wake, true))).is_empty());
        assert!(c.apply(&tick(200)).is_empty());
        // A turn well inside the new run is still judged.
        c.apply(&event(121, &turn(120, Cause::Wake, true)));
        let missed = c.apply(&tick(140));
        assert_eq!(missed.len(), 1);
        assert_eq!(missed[0].turn.id, "u120");
    }

    #[test]
    fn a_log_without_daemon_starts_ends_runs_at_the_next_start() {
        let mut c = WakeCoverage::new();
        c.started(at(0), true);
        c.apply(&tick(30));
        c.started(at(90), true);
        c.apply(&event(91, &turn(60, Cause::Wake, true)));
        assert!(c.apply(&tick(200)).is_empty());
    }
}
