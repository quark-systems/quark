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
//! it, and only while the shadow was running from [`WINDOW`] before it, so
//! the transcript's history from before the shadow started is not judged.

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
    /// Shadow runs, oldest first: when each started and whether it ran the
    /// coordinator's shadow.
    runs: Vec<(OffsetDateTime, bool)>,
    projects: BTreeMap<ProjectId, Project>,
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

    /// A daemon started at `at`, running the coordinator's shadow or not.
    pub fn started(&mut self, at: OffsetDateTime, coordinator: bool) {
        self.runs.push((at, coordinator));
    }

    /// Whether a turn at `at` could have been seen: the run it fell in ran
    /// the shadow, and had been running for a [`WINDOW`].
    fn watched(&self, at: OffsetDateTime) -> bool {
        self.runs
            .iter()
            .rev()
            .find(|(start, _)| *start <= at)
            .is_some_and(|(start, on)| *on && at - *start >= WINDOW)
    }

    /// Fold `e` and return the turns it proves were missed.
    pub fn apply(&mut self, e: &Event) -> Vec<Miss> {
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
                        if self.watched(at) {
                            let p = self.projects.entry(e.project.clone()).or_default();
                            p.waiting.push((at, turn));
                        }
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
}
