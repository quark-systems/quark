//! Which events need the coordinator's judgment.
//!
//! The coordinator is woken only for judgment. Worker and channel events
//! are routed by the away policy (`quark_triggers::away`): an occasion wakes
//! the coordinator when the Project's route for it says so, which by default
//! is done, decision, blocked, failed, an inbound message and a failed rule.
//! On top of that come the decisions other native subsystems leave to the
//! coordinator: dispatch could not pick an agent, a spawn failed, a host has
//! been unhealthy too long, or someone asked for a wake explicitly.
//! Everything else (progress, signals, steering, launches, transitions the
//! engine made itself) never wakes it, which is where the token savings
//! come from.

use quark_core::{Event, Seq, TaskId};
use quark_triggers::away::AwayState;
use quark_triggers::Occasion;
use serde::{Deserialize, Serialize};

use crate::events::{CoordinatorEvent, PREFIX};

/// Why the coordinator is woken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Need {
    /// A worker asks a question.
    Decision,
    Blocked,
    Failed,
    /// A deliverable is ready.
    Done,
    /// A message arrived on a channel (the user's inbox first).
    Inbound,
    /// A trigger rule failed or its outcome is unknown.
    TriggerFailed,
    /// Dispatch could not pick an agent and asks the coordinator to.
    DispatchEscalated,
    /// A placed task could not be started.
    SpawnFailed,
    /// A host has been unhealthy too long.
    HostUnhealthy,
    /// Someone asked for a wake (a rule's `wake` action, the API).
    Requested,
    /// An occasion a Project's away policy routes to the coordinator
    /// although the default does not (progress, paused, stale, a rule that
    /// fired).
    Policy,
}

impl Need {
    fn from_occasion(o: Occasion) -> Need {
        match o {
            Occasion::Decision => Need::Decision,
            Occasion::Blocked => Need::Blocked,
            Occasion::Failed => Need::Failed,
            Occasion::Done => Need::Done,
            Occasion::Inbound => Need::Inbound,
            Occasion::TriggerFailed => Need::TriggerFailed,
            Occasion::Progress | Occasion::Paused | Occasion::Stale | Occasion::TriggerFired => {
                Need::Policy
            }
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Need::Decision => "decision",
            Need::Blocked => "blocked",
            Need::Failed => "failed",
            Need::Done => "done",
            Need::Inbound => "message",
            Need::TriggerFailed => "rule failed",
            Need::DispatchEscalated => "pick an agent",
            Need::SpawnFailed => "spawn failed",
            Need::HostUnhealthy => "host unhealthy",
            Need::Requested => "requested",
            Need::Policy => "policy",
        }
    }
}

/// One thing a turn asks the coordinator to judge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeItem {
    /// The event that caused it.
    pub source: Seq,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskId>,
    pub need: Need,
    pub summary: String,
    /// Items with the same key while still waiting collapse into the
    /// latest one (a worker asking the same question again).
    pub key: String,
}

/// The wake item `e` becomes, or `None` when it needs no judgment.
pub fn judgment(e: &Event, away: &AwayState) -> Option<WakeItem> {
    let task_prefix = e
        .task
        .as_ref()
        .map(|t| format!("{t}: "))
        .unwrap_or_default();
    let item = |need: Need, summary: String, key: String| WakeItem {
        source: e.seq,
        task: e.task.clone(),
        need,
        summary,
        key,
    };
    let by_event = || format!("event:{}", e.id);

    if let Some((occasion, summary)) = quark_triggers::occasion(e) {
        if !away.route(&e.project, occasion).wake {
            return None;
        }
        let need = Need::from_occasion(occasion);
        let key = match (need, &e.task, ask_key(e)) {
            (Need::Decision, Some(task), Some(k)) => format!("ask:{task}:{k}"),
            _ => by_event(),
        };
        return Some(item(need, summary, key));
    }

    let kind = e.kind.as_str();
    if let Some(rest) = kind.strip_prefix("dispatch.") {
        let field = |name: &str| {
            e.payload
                .get(name)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        return match rest {
            "escalated" => Some(item(
                Need::DispatchEscalated,
                format!("{task_prefix}pick an agent: {}", field("reason")),
                by_event(),
            )),
            "spawn_failed" => Some(item(
                Need::SpawnFailed,
                format!("{task_prefix}could not start: {}", field("reason")),
                by_event(),
            )),
            "host_unhealthy" => Some(item(
                Need::HostUnhealthy,
                format!(
                    "host {} has been {} for {}s",
                    field("host"),
                    field("health"),
                    e.payload
                        .get("unhealthy_secs")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0)
                ),
                format!("host:{}", field("host")),
            )),
            _ => None,
        };
    }
    if kind == format!("{PREFIX}.requested") {
        if let Ok(CoordinatorEvent::Requested { note, by }) = e.decode::<CoordinatorEvent>() {
            return Some(item(
                Need::Requested,
                format!("{task_prefix}{by} asks: {note}"),
                by_event(),
            ));
        }
    }
    None
}

/// The key a worker's question is asked under, when it has one.
fn ask_key(e: &Event) -> Option<String> {
    let p = &e.payload;
    p.pointer("/message/key")
        .or_else(|| p.get("key"))
        .and_then(|v| v.as_str())
        .filter(|k| !k.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use quark_core::worker::{Transport, WorkerEnvelope, WorkerMessage};
    use quark_core::{event::kinds, HostId, NewEvent, ProjectId};
    use serde_json::json;

    use super::*;

    fn ev(seq: u64, task: Option<&str>, kind: &str, payload: serde_json::Value) -> Event {
        NewEvent::new(
            HostId::from("h"),
            ProjectId::from("p"),
            task.map(TaskId::from),
            kind,
            payload,
        )
        .with_seq(Seq(seq))
    }

    fn worker(seq: u64, message: WorkerMessage) -> Event {
        let env = WorkerEnvelope {
            task: TaskId::from("t1"),
            generation: "g1".into(),
            via: Transport::Mcp,
            message,
        };
        ev(
            seq,
            Some("t1"),
            kinds::WORKER,
            serde_json::to_value(env).unwrap(),
        )
    }

    #[test]
    fn progress_and_signals_never_wake() {
        let away = AwayState::default();
        for m in [
            WorkerMessage::Report {
                state: "working".into(),
                note: "tests pass".into(),
            },
            WorkerMessage::Report {
                state: "paused".into(),
                note: "waiting on CI".into(),
            },
            WorkerMessage::Signal {
                signal: "turn_end".into(),
            },
            WorkerMessage::Learned { fact: "x".into() },
        ] {
            assert_eq!(judgment(&worker(1, m), &away), None);
        }
        assert_eq!(
            judgment(&ev(2, Some("t1"), "supervisor.launched", json!({})), &away),
            None
        );
        assert_eq!(
            judgment(
                &ev(3, Some("t1"), "task.transition", json!({"type": "resumed"})),
                &away
            ),
            None
        );
    }

    #[test]
    fn questions_blocks_failures_and_done_wake() {
        let away = AwayState::default();
        let ask = judgment(
            &worker(
                5,
                WorkerMessage::Ask {
                    key: "schema".into(),
                    question: "v14 or v15?".into(),
                },
            ),
            &away,
        )
        .unwrap();
        assert_eq!(ask.need, Need::Decision);
        assert_eq!(ask.key, "ask:t1:schema");
        assert_eq!(ask.source, Seq(5));
        let done = judgment(
            &worker(
                6,
                WorkerMessage::Done {
                    summary: "fix".into(),
                    pull_request: Some("https://x/pr/1".into()),
                },
            ),
            &away,
        )
        .unwrap();
        assert_eq!(done.need, Need::Done);
        assert!(done.summary.contains("https://x/pr/1"));
        let blocked = judgment(
            &worker(
                7,
                WorkerMessage::Report {
                    state: "blocked".into(),
                    note: "no creds".into(),
                },
            ),
            &away,
        )
        .unwrap();
        assert_eq!(blocked.need, Need::Blocked);
    }

    #[test]
    fn firstmate_status_lines_follow_the_away_route() {
        let away = AwayState::default();
        let line = |verb: &str, raw: &str| {
            ev(
                9,
                Some("t2"),
                "firstmate.status",
                json!({"verb": verb, "key": "k1", "note": "", "raw": raw, "offset": 0}),
            )
        };
        assert_eq!(judgment(&line("working", "working: build"), &away), None);
        let d = judgment(&line("needs-decision", "needs-decision: pick"), &away).unwrap();
        assert_eq!(d.need, Need::Decision);
        assert_eq!(d.key, "ask:t2:k1");
    }

    #[test]
    fn dispatch_and_explicit_requests_wake() {
        let away = AwayState::default();
        let esc = judgment(
            &ev(
                1,
                Some("t3"),
                "dispatch.escalated",
                json!({"type": "escalated", "reason": "tie"}),
            ),
            &away,
        )
        .unwrap();
        assert_eq!(esc.need, Need::DispatchEscalated);
        assert_eq!(esc.summary, "t3: pick an agent: tie");
        assert_eq!(
            judgment(
                &ev(
                    2,
                    Some("t3"),
                    "dispatch.held",
                    json!({"type": "held", "reason": "cpu"})
                ),
                &away
            ),
            None
        );
        let host = judgment(
            &ev(
                3,
                None,
                "dispatch.host_unhealthy",
                json!({"type": "host_unhealthy", "host": "mac", "health": "degraded", "unhealthy_secs": 900}),
            ),
            &away,
        )
        .unwrap();
        assert_eq!(host.key, "host:mac");
        let req = CoordinatorEvent::Requested {
            note: "check nightly".into(),
            by: "rule nightly".into(),
        };
        let r = judgment(
            &ev(
                4,
                None,
                "coordinator.requested",
                serde_json::to_value(&req).unwrap(),
            ),
            &away,
        )
        .unwrap();
        assert_eq!(r.need, Need::Requested);
        assert_eq!(r.summary, "rule nightly asks: check nightly");
    }
}
