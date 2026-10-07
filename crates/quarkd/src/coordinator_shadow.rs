//! Slice 6's comparison, run beside the shadow coordinator.
//!
//! While the native coordinator shadows ([`crate::native_coordinator`]),
//! every wake turn in which firstmate's coordinator acted should have a
//! native `coordinator.would_wake` for the same Project within
//! [`quark_coordinator::shadow::WINDOW`]. Each one without is a
//! `shadow.divergence` (operation `acting_wake`), judged by
//! [`quark_coordinator::shadow::WakeCoverage`]. Daemon starts
//! ([`crate::shadows::DAEMON_STARTED`], then [`crate::shadows::STARTED`])
//! say when the shadow was watching, so firstmate's turns from before it
//! ran, or from while the daemon was down, are not judged.

use quark_coordinator::shadow::{WakeCoverage, WINDOW};
use quark_core::{Event, Slice};
use serde_json::json;

use crate::log_shadow::{Found, Judge};
use crate::shadows::{Started, DAEMON_STARTED, STARTED};

/// Operation the misses are recorded under.
pub const OPERATION: &str = "acting_wake";

/// Judges firstmate's acting wake turns against native `would_wake`s.
#[derive(Default)]
pub struct WakeTurns(WakeCoverage);

impl Judge for WakeTurns {
    fn slice(&self) -> Slice {
        Slice::Coordinator
    }

    fn checkpoint(&self) -> &'static str {
        "shadow/coordinator"
    }

    fn judge(&mut self, event: &Event) -> Vec<Found> {
        if event.kind.as_str() == DAEMON_STARTED {
            self.0.daemon_started(event.ts);
            return Vec::new();
        }
        if event.kind.as_str() == STARTED {
            if let Ok(s) = event.decode::<Started>() {
                self.0
                    .started(event.ts, s.slices.contains(&Slice::Coordinator));
            }
            return Vec::new();
        }
        self.0
            .apply(event)
            .into_iter()
            .map(|miss| Found {
                project: Some(miss.project),
                operation: OPERATION.to_string(),
                bash: json!({
                    "turn": miss.turn.id,
                    "at": miss.turn.at,
                    "acted": true,
                    "tool_calls": miss.turn.tool_calls,
                }),
                native: json!({
                    "would_wake": false,
                    "window_secs": WINDOW.whole_seconds(),
                }),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use quark_coordinator::baseline::{BaselineTurn, Cause};
    use quark_coordinator::{CoordinatorEvent, Usage};
    use quark_core::slice::Divergence;
    use quark_core::{EventLog, HostId, NewEvent, ProjectId};
    use quark_eventlog::SqliteEventLog;
    use time::format_description::well_known::Rfc3339;
    use time::OffsetDateTime;

    use super::*;
    use crate::log_shadow::LogShadow;

    #[tokio::test]
    async fn an_acting_turn_nothing_would_have_woken_is_a_divergence() {
        let db = tempfile::tempdir().unwrap();
        let log = SqliteEventLog::open(db.path().join("events.db")).unwrap();
        let host = HostId::from("h");
        crate::shadows::record_started(&log, host.clone(), vec![Slice::Coordinator], vec![]).await;

        // A turn firstmate took well after the start, then the log moves on.
        let later: OffsetDateTime = OffsetDateTime::now_utc() + WINDOW * 2;
        let turn = CoordinatorEvent::Baseline {
            turn: BaselineTurn {
                id: "u1".into(),
                at: later.format(&Rfc3339).unwrap(),
                cause: Cause::Wake,
                usage: Usage::default(),
                tool_calls: 3,
                acts: true,
            },
        };
        let p = ProjectId::from("p");
        let append = |e: &CoordinatorEvent, ts: OffsetDateTime| {
            let mut ev = NewEvent::typed(host.clone(), p.clone(), None, e.kind(), e).unwrap();
            ev.ts = ts;
            ev
        };
        log.append(append(&turn, later)).await.unwrap();
        let tick = CoordinatorEvent::Cursor {
            through: quark_core::Seq(1),
        };
        log.append(append(&tick, later + WINDOW * 2)).await.unwrap();

        let mut shadow = LogShadow::new(log.clone(), host, WakeTurns::default());
        assert_eq!(shadow.pass().await.unwrap().diverged, 1);
        let found = log.read(quark_core::Seq::ZERO, 100).await.unwrap();
        let d = found
            .iter()
            .find(|e| e.kind.as_str() == quark_core::event::kinds::SHADOW_DIVERGENCE)
            .unwrap();
        assert_eq!(d.project.as_str(), "p");
        let d: Divergence = d.decode().unwrap();
        assert_eq!(d.slice, Slice::Coordinator);
        assert_eq!(d.operation, OPERATION);
        assert_eq!(d.bash["turn"], "u1");
    }
}
