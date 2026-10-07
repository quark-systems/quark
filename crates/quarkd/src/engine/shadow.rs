//! The slice switch and the shadow engine.
//!
//! [`ShadowEngine`] sits where the daemon expects one [`EngineAdapter`] and
//! routes each operation by the mode of the slice it belongs to:
//!
//! - `bash`: firstmate only.
//! - `shadow`: firstmate serves the call. Reads also run on the native
//!   engine, and any disagreement is appended to the event log as a
//!   `shadow.divergence` event; callers always get firstmate's answer.
//!   Writes go to firstmate only, so nothing acts twice. Comparing a native
//!   *decision* for a write (for example a merge verdict) without acting on
//!   it is each native slice's own job.
//! - `native`: the native engine only.
//!
//! | Slice | Operations |
//! |---|---|
//! | 1 event log | `snapshot`, `status_tail`, `holds`, `watch_dirs`, `is_task_change` |
//! | 2 verification | `merge_pull_request`, `set_standing_approval`, `gate_evidence`, `gate_artifact`, `set_gates` |
//! | 3 worker protocol | `answer` |
//! | 4 supervision | `send_message`, `control`, `spawn`, `coordinator_terminals`, `account_envs` |
//! | 5 dispatch | `resolve_dispatch`, `resolve_description`, `set_crew_dispatch` |
//! | 7 sub-coordinators | `add_source`, `seed_workspace`, `start_coordinator`, `inbox_note` |
//!
//! Modes change at runtime with [`ShadowEngine::set_mode`] and
//! [`ShadowEngine::rollback`]; each change is logged as a `slice.mode`
//! event. Startup modes come from `QUARK_ENGINE_SLICES`
//! (see [`slices_from_env`]).

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use quark_core::event::kinds;
use quark_core::slice::{Divergence, SliceChange};
use quark_core::{CoreError, EventLog, HostId, NewEvent, ProjectId, Slice, SliceMode, SliceSwitch};
use quark_systems::{AgentConfig, DeliveryPolicy, Evidence, MergeMethod};
use serde::Serialize;

use super::{
    EngineAdapter, EngineError, EngineResolution, EngineSpawn, FleetSnapshot, Hold, SourceRepo,
    StatusTail, TaskControl, WorkspacePlan, WorkspaceRef,
};

/// The variable holding startup slice modes, such as `1=shadow`.
pub const SLICES_ENV: &str = "QUARK_ENGINE_SLICES";

/// Startup slice modes from [`SLICES_ENV`]; every slice on bash when unset.
pub fn slices_from_env() -> Result<SliceSwitch, CoreError> {
    match std::env::var(SLICES_ENV) {
        Ok(spec) => SliceSwitch::parse(&spec),
        Err(_) => Ok(SliceSwitch::new()),
    }
}

type Call<'a, T> = Pin<Box<dyn Future<Output = Result<T, EngineError>> + Send + 'a>>;

/// Runs firstmate and the native engine side by side, per slice.
pub struct ShadowEngine {
    bash: Arc<dyn EngineAdapter>,
    native: Arc<dyn EngineAdapter>,
    switch: RwLock<SliceSwitch>,
    log: Arc<dyn EventLog>,
    host: HostId,
}

impl ShadowEngine {
    pub fn new(
        bash: Arc<dyn EngineAdapter>,
        native: Arc<dyn EngineAdapter>,
        switch: SliceSwitch,
        log: Arc<dyn EventLog>,
        host: HostId,
    ) -> Self {
        Self {
            bash,
            native,
            switch: RwLock::new(switch),
            log,
            host,
        }
    }

    pub fn mode(&self, slice: Slice) -> SliceMode {
        self.switch.read().unwrap().mode(slice)
    }

    /// Switch `slice` to `mode`, keeping the switch-on order.
    pub async fn set_mode(&self, slice: Slice, mode: SliceMode, by: &str) -> Result<(), CoreError> {
        let change = self.switch.write().unwrap().set(slice, mode, by)?;
        self.record_changes(change.into_iter().collect()).await;
        Ok(())
    }

    /// Put `slice` back in its previous mode (and any later slice the order
    /// rule would leave ahead of it).
    pub async fn rollback(&self, slice: Slice) -> Result<(), CoreError> {
        let changes = self.switch.write().unwrap().rollback(slice)?;
        self.record_changes(changes).await;
        Ok(())
    }

    async fn record_changes(&self, changes: Vec<SliceChange>) {
        for c in changes {
            tracing::info!(slice = %c.slice, from = c.from.as_str(), to = c.to.as_str(), by = %c.by, "engine slice mode");
            self.record(ProjectId::engine(), kinds::SLICE_MODE, &c)
                .await;
        }
    }

    async fn record<T: Serialize>(&self, project: ProjectId, kind: &str, payload: &T) {
        let event = NewEvent::typed(self.host.clone(), project, None, kind, payload);
        let result = match event {
            Ok(e) => self.log.append(e).await.map(|_| ()),
            Err(e) => Err(e),
        };
        if let Err(e) = result {
            tracing::warn!(kind, error = %e, "could not record engine event");
        }
    }

    /// The adapter that acts for `slice`.
    fn acting(&self, slice: Slice) -> &dyn EngineAdapter {
        match self.mode(slice) {
            SliceMode::Native => self.native.as_ref(),
            SliceMode::Bash | SliceMode::Shadow => self.bash.as_ref(),
        }
    }

    /// Run a read for `slice`, shadowing it on the native engine when the
    /// slice is in shadow mode.
    async fn read<'a, T, F>(
        &'a self,
        slice: Slice,
        operation: &str,
        project: &str,
        call: F,
    ) -> Result<T, EngineError>
    where
        T: Serialize,
        F: Fn(&'a dyn EngineAdapter) -> Call<'a, T>,
    {
        match self.mode(slice) {
            SliceMode::Bash => call(self.bash.as_ref()).await,
            SliceMode::Native => call(self.native.as_ref()).await,
            SliceMode::Shadow => {
                let bash = call(self.bash.as_ref()).await;
                let native = call(self.native.as_ref()).await;
                let (b, n) = (outcome(&bash), outcome(&native));
                if b != n {
                    let d = Divergence {
                        slice,
                        operation: operation.to_string(),
                        bash: b,
                        native: n,
                    };
                    self.record(ProjectId::new(project), kinds::SHADOW_DIVERGENCE, &d)
                        .await;
                }
                bash
            }
        }
    }
}

/// A call's result as JSON, for comparison. Errors compare by kind only,
/// since their messages name engine internals.
fn outcome<T: Serialize>(r: &Result<T, EngineError>) -> serde_json::Value {
    match r {
        Ok(v) => serde_json::to_value(v)
            .unwrap_or_else(|e| serde_json::json!({ "unserializable": e.to_string() })),
        Err(e) => {
            let kind = match e {
                EngineError::Invalid(_) => "invalid",
                EngineError::WorkspaceNotFound(_) => "workspace_not_found",
                EngineError::TaskNotFound(_) => "task_not_found",
                EngineError::Command(_) => "command",
                EngineError::Parse(_) => "parse",
                EngineError::Io(_) => "io",
            };
            serde_json::json!({ "error": kind, "message": e.to_string() })
        }
    }
}

#[async_trait]
impl EngineAdapter for ShadowEngine {
    fn name(&self) -> &'static str {
        "shadow"
    }

    fn slices(&self) -> SliceSwitch {
        self.switch.read().unwrap().clone()
    }

    async fn snapshot(&self, ws: &WorkspaceRef) -> Result<FleetSnapshot, EngineError> {
        self.read(Slice::EventLog, "snapshot", &ws.project_id, |e| {
            e.snapshot(ws)
        })
        .await
    }

    async fn status_tail(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
        offset: u64,
    ) -> Result<StatusTail, EngineError> {
        self.read(Slice::EventLog, "status_tail", &ws.project_id, |e| {
            e.status_tail(ws, task_id, offset)
        })
        .await
    }

    async fn holds(&self, ws: &WorkspaceRef) -> Result<Vec<Hold>, EngineError> {
        self.read(Slice::EventLog, "holds", &ws.project_id, |e| e.holds(ws))
            .await
    }

    fn watch_dirs(&self, ws: &WorkspaceRef) -> Vec<PathBuf> {
        self.acting(Slice::EventLog).watch_dirs(ws)
    }

    fn is_task_change(&self, path: &Path) -> bool {
        self.acting(Slice::EventLog).is_task_change(path)
    }

    async fn send_message(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
        text: &str,
    ) -> Result<(), EngineError> {
        self.acting(Slice::Supervision)
            .send_message(ws, task_id, text)
            .await
    }

    async fn inbox_note(&self, ws: &WorkspaceRef, text: &str) -> Result<(), EngineError> {
        self.acting(Slice::SubCoordinators)
            .inbox_note(ws, text)
            .await
    }

    async fn answer(
        &self,
        ws: &WorkspaceRef,
        hold_id: &str,
        answer: &str,
        answered_by: &str,
    ) -> Result<(), EngineError> {
        self.acting(Slice::WorkerProtocol)
            .answer(ws, hold_id, answer, answered_by)
            .await
    }

    async fn control(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
        action: &TaskControl,
    ) -> Result<(), EngineError> {
        self.acting(Slice::Supervision)
            .control(ws, task_id, action)
            .await
    }

    async fn merge_pull_request(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
        url: &str,
        method: Option<MergeMethod>,
    ) -> Result<(), EngineError> {
        self.acting(Slice::Verification)
            .merge_pull_request(ws, task_id, url, method)
            .await
    }

    async fn set_standing_approval(
        &self,
        ws: &WorkspaceRef,
        repos: &[String],
        on: bool,
    ) -> Result<(), EngineError> {
        self.acting(Slice::Verification)
            .set_standing_approval(ws, repos, on)
            .await
    }

    async fn gate_evidence(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
    ) -> Result<Option<Evidence>, EngineError> {
        self.read(Slice::Verification, "gate_evidence", &ws.project_id, |e| {
            e.gate_evidence(ws, task_id)
        })
        .await
    }

    fn gate_artifact(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
        path: &str,
    ) -> Result<PathBuf, EngineError> {
        self.acting(Slice::Verification)
            .gate_artifact(ws, task_id, path)
    }

    async fn add_source(
        &self,
        command: &Path,
        source: &SourceRepo,
        delivery: DeliveryPolicy,
    ) -> Result<(), EngineError> {
        self.acting(Slice::SubCoordinators)
            .add_source(command, source, delivery)
            .await
    }

    async fn seed_workspace(
        &self,
        command: &Path,
        plan: &WorkspacePlan,
    ) -> Result<PathBuf, EngineError> {
        self.acting(Slice::SubCoordinators)
            .seed_workspace(command, plan)
            .await
    }

    async fn start_coordinator(
        &self,
        command: &Path,
        ws: &WorkspaceRef,
        agent: &AgentConfig,
        account_env: &[(String, String)],
    ) -> Result<(), EngineError> {
        self.acting(Slice::SubCoordinators)
            .start_coordinator(command, ws, agent, account_env)
            .await
    }

    fn account_envs(&self) -> &'static [&'static str] {
        self.acting(Slice::Supervision).account_envs()
    }

    async fn set_gates(&self, ws: &WorkspaceRef, config: &str) -> Result<(), EngineError> {
        self.acting(Slice::Verification).set_gates(ws, config).await
    }

    async fn set_crew_dispatch(&self, ws: &WorkspaceRef, config: &str) -> Result<(), EngineError> {
        self.acting(Slice::Dispatch)
            .set_crew_dispatch(ws, config)
            .await
    }

    async fn spawn(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
    ) -> Result<Option<EngineSpawn>, EngineError> {
        self.read(Slice::Supervision, "spawn", &ws.project_id, |e| {
            e.spawn(ws, task_id)
        })
        .await
    }

    async fn resolve_dispatch(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
        project: Option<&str>,
    ) -> Result<Option<EngineResolution>, EngineError> {
        self.read(Slice::Dispatch, "resolve_dispatch", &ws.project_id, |e| {
            e.resolve_dispatch(ws, task_id, project)
        })
        .await
    }

    async fn resolve_description(
        &self,
        ws: &WorkspaceRef,
        description: &str,
    ) -> Result<Option<EngineResolution>, EngineError> {
        self.read(
            Slice::Dispatch,
            "resolve_description",
            &ws.project_id,
            |e| e.resolve_description(ws, description),
        )
        .await
    }

    async fn coordinator_terminals(
        &self,
        command: &Path,
    ) -> Result<HashMap<String, String>, EngineError> {
        self.read(
            Slice::Supervision,
            "coordinator_terminals",
            ProjectId::ENGINE,
            |e| e.coordinator_terminals(command),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{EngineTask, StubEngine, StubWrite};
    use quark_core::fake::MemoryEventLog;
    use quark_systems::TaskState;

    fn ws() -> WorkspaceRef {
        WorkspaceRef {
            project_id: "p1".into(),
            root: "/ws".into(),
        }
    }

    fn task(id: &str, state: TaskState) -> EngineTask {
        EngineTask {
            id: id.into(),
            title: id.into(),
            kind: None,
            state,
            state_note: None,
            harness: None,
            pull_request_url: None,
            terminal: None,
            worktree: None,
        }
    }

    struct Rig {
        bash: Arc<StubEngine>,
        native: Arc<StubEngine>,
        log: MemoryEventLog,
        engine: ShadowEngine,
    }

    fn rig(switch: &str) -> Rig {
        let bash = Arc::new(StubEngine::new());
        let native = Arc::new(StubEngine::new());
        bash.set_snapshot(FleetSnapshot {
            tasks: vec![task("t1", TaskState::Running)],
        });
        native.set_snapshot(FleetSnapshot {
            tasks: vec![task("t1", TaskState::Blocked)],
        });
        let log = MemoryEventLog::new();
        let engine = ShadowEngine::new(
            bash.clone(),
            native.clone(),
            SliceSwitch::parse(switch).unwrap(),
            Arc::new(log.clone()),
            HostId::from("local"),
        );
        Rig {
            bash,
            native,
            log,
            engine,
        }
    }

    #[tokio::test]
    async fn bash_mode_never_calls_native() {
        let r = rig("");
        let snap = r.engine.snapshot(&ws()).await.unwrap();
        assert_eq!(snap.tasks[0].state, TaskState::Running);
        r.engine.send_message(&ws(), "t1", "hi").await.unwrap();
        assert_eq!(r.bash.writes().len(), 1);
        assert!(r.native.writes().is_empty());
        assert!(r.log.events().is_empty());
    }

    #[tokio::test]
    async fn shadow_returns_bash_and_records_divergence() {
        let r = rig("1=shadow");
        let snap = r.engine.snapshot(&ws()).await.unwrap();
        assert_eq!(snap.tasks[0].state, TaskState::Running);
        let events = r.log.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind.as_str(), kinds::SHADOW_DIVERGENCE);
        assert_eq!(events[0].project.as_str(), "p1");
        let d: Divergence = events[0].decode().unwrap();
        assert_eq!(d.slice, Slice::EventLog);
        assert_eq!(d.operation, "snapshot");
        assert_eq!(d.native["tasks"][0]["state"], "blocked");

        // Agreement records nothing.
        r.engine.holds(&ws()).await.unwrap();
        assert_eq!(r.log.events().len(), 1);
    }

    #[tokio::test]
    async fn shadow_writes_act_once() {
        let r = rig("1=shadow,2=shadow,3=shadow,4=shadow");
        r.engine.send_message(&ws(), "t1", "hi").await.unwrap();
        r.engine
            .merge_pull_request(&ws(), "t1", "https://x/pr/1", None)
            .await
            .unwrap();
        assert_eq!(r.bash.writes().len(), 2);
        assert!(r.native.writes().is_empty());
    }

    #[tokio::test]
    async fn native_mode_routes_to_native() {
        let r = rig("1=native,2=native,3=native,4=native");
        let snap = r.engine.snapshot(&ws()).await.unwrap();
        assert_eq!(snap.tasks[0].state, TaskState::Blocked);
        r.engine.send_message(&ws(), "t1", "hi").await.unwrap();
        assert!(matches!(r.native.writes()[0], StubWrite::Message { .. }));
        assert!(r.bash.writes().is_empty());
        // Slice 5 is still bash.
        r.engine.set_crew_dispatch(&ws(), "{}").await.unwrap();
        assert_eq!(r.bash.writes().len(), 1);
        assert!(r.log.events().is_empty());
    }

    #[tokio::test]
    async fn mode_changes_and_rollback_are_logged() {
        let r = rig("");
        assert!(r
            .engine
            .set_mode(Slice::Verification, SliceMode::Shadow, "matt")
            .await
            .is_err());
        r.engine
            .set_mode(Slice::EventLog, SliceMode::Shadow, "matt")
            .await
            .unwrap();
        assert_eq!(r.engine.slices().mode(Slice::EventLog), SliceMode::Shadow);
        r.engine.snapshot(&ws()).await.unwrap();
        r.engine.rollback(Slice::EventLog).await.unwrap();
        assert_eq!(r.engine.mode(Slice::EventLog), SliceMode::Bash);
        let kinds: Vec<_> = r
            .log
            .events()
            .iter()
            .map(|e| e.kind.as_str().to_string())
            .collect();
        assert_eq!(
            kinds,
            [
                kinds::SLICE_MODE,
                kinds::SHADOW_DIVERGENCE,
                kinds::SLICE_MODE
            ]
        );
        let back: SliceChange = r.log.events()[2].decode().unwrap();
        assert_eq!((back.from, back.to), (SliceMode::Shadow, SliceMode::Bash));
        assert_eq!(back.by, "rollback");
    }
}
