//! Coordinator token efficiency, folded from the log.
//!
//! For one Project over a window: turns, tokens and acknowledgement-only
//! turns, per task, for the native coordinator (`coordinator.turn_ended`,
//! with its acting tool calls from `coordinator.tool`) and for firstmate's
//! (`coordinator.baseline`). In shadow mode the native side has no turns of
//! its own yet; `would_wake` counts the turns it would have taken.
//!
//! A task counts once it appears in the window under a task event, a
//! firstmate spawn or a dispatch request.

use std::collections::{BTreeMap, BTreeSet};

use quark_core::{Event, TaskId};
use serde::{Deserialize, Serialize};

use crate::events::{CoordinatorEvent, Usage, PREFIX};

/// One coordinator's figures.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Side {
    pub turns: u32,
    /// Turns that changed nothing: status acknowledgements.
    pub acks: u32,
    pub usage: Usage,
}

impl Side {
    /// Share of turns that only acknowledged status.
    pub fn ack_share(&self) -> Option<f64> {
        (self.turns > 0).then(|| f64::from(self.acks) / f64::from(self.turns))
    }

    pub fn per_task(&self, tasks: u32) -> Option<(f64, f64)> {
        (tasks > 0).then(|| {
            (
                f64::from(self.turns) / f64::from(tasks),
                self.usage.total() as f64 / f64::from(tasks),
            )
        })
    }
}

/// Native and baseline figures for one Project.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Efficiency {
    pub tasks: u32,
    pub native: Side,
    pub baseline: Side,
    /// Shadow: turns the native coordinator would have taken.
    pub would_wake: u32,
}

impl Efficiency {
    /// Fold `events` (one Project's, in log order) whose time passes
    /// `in_window`.
    pub fn fold(events: &[Event], in_window: impl Fn(&Event) -> bool) -> Efficiency {
        let mut out = Efficiency::default();
        let mut tasks: BTreeSet<&TaskId> = BTreeSet::new();
        // Native turns being built: turn id (None for unwoken) -> acted.
        let mut acted: BTreeMap<Option<String>, bool> = BTreeMap::new();
        for e in events {
            let kind = e.kind.as_str();
            if let Some(task) = &e.task {
                if in_window(e)
                    && (kind == quark_core::event::kinds::TASK
                        || kind == "firstmate.spawn"
                        || kind == "dispatch.requested")
                {
                    tasks.insert(task);
                }
            }
            if e.kind.prefix() != PREFIX {
                continue;
            }
            let Ok(ev) = e.decode::<CoordinatorEvent>() else {
                continue;
            };
            match ev {
                CoordinatorEvent::Tool { turn, call, .. } => {
                    *acted.entry(turn).or_default() |= call.acts();
                }
                CoordinatorEvent::TurnEnded { turn, usage, .. } => {
                    let acts = acted.remove(&turn).unwrap_or(false);
                    if in_window(e) {
                        out.native.turns += 1;
                        // A turn nobody woke was the user talking: never an ack.
                        if !acts && turn.is_some() {
                            out.native.acks += 1;
                        }
                        out.native.usage.add(&usage);
                    }
                }
                CoordinatorEvent::Baseline { turn } if in_window(e) => {
                    out.baseline.turns += 1;
                    if turn.is_ack() {
                        out.baseline.acks += 1;
                    }
                    out.baseline.usage.add(&turn.usage);
                }
                CoordinatorEvent::WouldWake { .. } if in_window(e) => out.would_wake += 1,
                _ => {}
            }
        }
        out.tasks = tasks.len() as u32;
        out
    }
}

#[cfg(test)]
mod tests {
    use quark_core::{HostId, NewEvent, ProjectId, Seq};
    use serde_json::json;

    use super::*;
    use crate::baseline::{BaselineTurn, Cause};
    use crate::tools::ToolCall;

    fn ev(seq: u64, task: Option<&str>, e: serde_json::Value, kind: &str) -> Event {
        NewEvent::new(
            HostId::from("h"),
            ProjectId::from("p"),
            task.map(TaskId::from),
            kind,
            e,
        )
        .with_seq(Seq(seq))
    }

    fn c(seq: u64, e: CoordinatorEvent) -> Event {
        ev(
            seq,
            None,
            serde_json::to_value(&e).unwrap(),
            e.kind().as_str(),
        )
    }

    #[test]
    fn counts_turns_acks_and_tasks() {
        let usage = Usage {
            input: 100,
            output: 10,
            cache_read: 50,
            calls: 1,
        };
        let base = |id: &str, cause, acts| CoordinatorEvent::Baseline {
            turn: BaselineTurn {
                id: id.into(),
                at: String::new(),
                cause,
                usage,
                tool_calls: 1,
                acts,
            },
        };
        let events = vec![
            ev(
                1,
                Some("t1"),
                json!({"type": "queued", "title": "x"}),
                "task.transition",
            ),
            ev(2, Some("t2"), json!({}), "firstmate.spawn"),
            ev(3, Some("t2"), json!({}), "firstmate.status"),
            c(4, base("a", Cause::Wake, false)),
            c(5, base("b", Cause::Wake, true)),
            c(6, base("c", Cause::User, true)),
            c(
                7,
                CoordinatorEvent::Tool {
                    id: "1".into(),
                    turn: Some("w1".into()),
                    call: ToolCall::Fleet {},
                },
            ),
            c(
                8,
                CoordinatorEvent::TurnEnded {
                    turn: Some("w1".into()),
                    end: Default::default(),
                    usage,
                },
            ),
            c(
                9,
                CoordinatorEvent::Tool {
                    id: "2".into(),
                    turn: Some("w2".into()),
                    call: ToolCall::TellUser { text: "x".into() },
                },
            ),
            c(
                10,
                CoordinatorEvent::TurnEnded {
                    turn: Some("w2".into()),
                    end: Default::default(),
                    usage,
                },
            ),
            c(11, CoordinatorEvent::WouldWake { items: vec![] }),
        ];
        let e = Efficiency::fold(&events, |_| true);
        assert_eq!(e.tasks, 2);
        assert_eq!(e.baseline.turns, 3);
        assert_eq!(e.baseline.acks, 1);
        assert_eq!(e.baseline.usage.total(), 330);
        assert_eq!(e.native.turns, 2);
        assert_eq!(e.native.acks, 1);
        assert_eq!(e.would_wake, 1);
        assert_eq!(e.baseline.ack_share(), Some(1.0 / 3.0));
        assert_eq!(e.baseline.per_task(e.tasks), Some((1.5, 165.0)));
        let none = Efficiency::fold(&events, |e| e.seq > Seq(11));
        assert_eq!(none, Efficiency::default());
    }
}
