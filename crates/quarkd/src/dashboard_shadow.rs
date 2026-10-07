//! Slice 8's shadow: the dashboard's Overview against firstmate's fleet.
//!
//! The Overview tab reads each task's live state from the native event log
//! ([`crate::overview::OverviewModel`]) rather than from firstmate. With
//! `QUARK_SHADOW_DASHBOARD=1` ([`ENV`]), or `QUARK_SHADOWS=all`, every
//! firstmate snapshot is also compared with the Overview's live list: the
//! same tasks, and the same state for each task whose state firstmate read
//! from its status log. A finished task the Overview still shows after
//! firstmate cleaned it up is not a divergence; the Overview keeps those
//! for [`crate::overview::FINISHED_FOR`] on purpose.

use async_trait::async_trait;
use quark_core::{ProjectId, Slice};
use quark_eventlog::{FirstmateBridge, SqliteEventLog};
use quark_systems::{PulseState, TaskState};
use time::OffsetDateTime;

use crate::engine::shadow::SnapshotCheck;
use crate::engine::{EngineError, EngineTask, FleetSnapshot, WorkspaceRef};

/// Opt-in flag for this shadow.
pub const ENV: &str = "QUARK_SHADOW_DASHBOARD";

pub fn enabled() -> bool {
    crate::shadows::opt_in(ENV)
}

/// The Overview's live tasks as a fleet snapshot.
pub struct OverviewCheck {
    log: SqliteEventLog,
    bridge: FirstmateBridge,
}

impl OverviewCheck {
    /// `bridge` must write to `log`; it is the daemon's shared bridge, so
    /// the Overview is caught up to the moment firstmate answered.
    pub fn new(log: SqliteEventLog, bridge: FirstmateBridge) -> Self {
        Self { log, bridge }
    }
}

fn task_state(s: PulseState) -> TaskState {
    match s {
        PulseState::Working => TaskState::Running,
        PulseState::NeedsDecision => TaskState::NeedsDecision,
        PulseState::Blocked => TaskState::Blocked,
        PulseState::Paused => TaskState::Paused,
        PulseState::Done => TaskState::Done,
        PulseState::Failed => TaskState::Failed,
    }
}

#[async_trait]
impl SnapshotCheck for OverviewCheck {
    fn slice(&self) -> Slice {
        Slice::Sandbox
    }

    fn operation(&self) -> &'static str {
        "overview_live"
    }

    fn switched(&self) -> bool {
        false
    }

    async fn view(&self, ws: &WorkspaceRef) -> Result<FleetSnapshot, EngineError> {
        let fail = |e: quark_core::CoreError| EngineError::Command(format!("overview: {e}"));
        self.bridge
            .ingest(&ProjectId::new(ws.project_id.clone()), &ws.root)
            .await
            .map_err(fail)?;
        let model = crate::overview::shared(&self.log);
        let mut model = model.lock().await;
        model.catch_up().await.map_err(fail)?;
        let live = model
            .overview(&ws.project_id, None, OffsetDateTime::now_utc())
            .live;
        Ok(FleetSnapshot {
            tasks: live
                .tasks
                .into_iter()
                .map(|t| EngineTask {
                    id: t.engine_task.clone(),
                    title: t.engine_task,
                    kind: None,
                    state: task_state(t.state),
                    state_note: Some(t.note),
                    state_source: Some(crate::engine::eventlog::SOURCE.to_string()),
                    harness: t.harness,
                    pull_request_url: None,
                    terminal: None,
                    worktree: None,
                })
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::Arc;

    use quark_core::fake::MemoryEventLog;
    use quark_core::slice::Divergence;
    use quark_core::{HostId, SliceSwitch};

    use super::*;
    use crate::engine::shadow::ShadowEngine;
    use crate::engine::{EngineAdapter, StubEngine};

    fn task(id: &str, state: TaskState) -> EngineTask {
        EngineTask {
            id: id.into(),
            title: id.into(),
            kind: None,
            state,
            state_note: None,
            state_source: Some("status-log".into()),
            harness: None,
            pull_request_url: None,
            terminal: None,
            worktree: None,
        }
    }

    #[tokio::test]
    async fn the_overview_is_compared_with_every_snapshot() {
        let home = tempfile::tempdir().unwrap();
        let state = home.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("t1.meta"), "spawn_gen=g\nharness=claude\n").unwrap();
        std::fs::write(state.join("t2.meta"), "spawn_gen=g\nharness=claude\n").unwrap();
        let mut f = std::fs::File::create(state.join("t1.status")).unwrap();
        f.write_all(b"working: go\nblocked: token\n").unwrap();
        std::fs::write(state.join("t2.status"), "done: shipped\n").unwrap();

        let db = tempfile::tempdir().unwrap();
        let events = SqliteEventLog::open(db.path().join("events.db")).unwrap();
        let bridge = FirstmateBridge::new(events.clone(), HostId::from("h"));
        let bash = Arc::new(StubEngine::new());
        // firstmate has cleaned up t2 and reads t1 as working.
        bash.set_snapshot(FleetSnapshot {
            tasks: vec![task("t1", TaskState::Running)],
        });
        let log = MemoryEventLog::new();
        let engine = ShadowEngine::new(
            bash.clone(),
            Arc::new(StubEngine::new()),
            SliceSwitch::new(),
            Arc::new(log.clone()),
            HostId::from("h"),
        )
        .comparing(&[])
        .checking(Arc::new(OverviewCheck::new(events, bridge)));
        let ws = WorkspaceRef {
            project_id: "p".into(),
            root: home.path().into(),
        };

        engine.snapshot(&ws).await.unwrap();
        let found = log.events();
        assert_eq!(found.len(), 1, "the finished t2 is not a divergence");
        let d: Divergence = found[0].decode().unwrap();
        assert_eq!(d.slice, Slice::Sandbox);
        assert_eq!(d.operation, "overview_live");
        assert_eq!(d.bash["t1"]["state"], "working");
        assert_eq!(d.native["t1"]["state"], "blocked");

        bash.set_snapshot(FleetSnapshot {
            tasks: vec![task("t1", TaskState::Blocked)],
        });
        engine.snapshot(&ws).await.unwrap();
        assert_eq!(log.events().len(), 1, "agreement records nothing");
    }
}
