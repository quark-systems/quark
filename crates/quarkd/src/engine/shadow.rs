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
//! A read that disagrees is asked again of both engines before it is
//! recorded, so a file that changed between the two reads is not counted.
//! Slice 1's reads compare only what the event log can know (see
//! [`super::eventlog`]): which tasks exist, the state of those whose state
//! firstmate read from the status log, their open decisions, and the status
//! lines themselves.
//!
//! [`ShadowEngine::comparing`] limits which slices' reads go to the native
//! engine; a shadow slice outside it is served by firstmate alone, because
//! its comparison lives elsewhere (slice 2's in `verify_shadow`).
//!
//! Modes change at runtime with [`ShadowEngine::set_mode`] and
//! [`ShadowEngine::rollback`]; each change is logged as a `slice.mode`
//! event. Startup modes come from `QUARK_ENGINE_SLICES`
//! (see [`slices_from_env`]).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use quark_core::event::kinds;
use quark_core::slice::{Divergence, SliceChange};
use quark_core::{CoreError, EventLog, HostId, NewEvent, ProjectId, Slice, SliceMode, SliceSwitch};
use quark_systems::{AgentConfig, DeliveryPolicy, Evidence, MergeMethod, TaskState};
use serde::Serialize;

use super::{
    EngineAdapter, EngineError, EngineResolution, EngineSpawn, FleetSnapshot, Hold, SourceRepo,
    StatusTail, TaskControl, WorkspacePlan, WorkspaceRef,
};

/// The variable holding startup slice modes, such as `1=shadow`.
pub const SLICES_ENV: &str = "QUARK_ENGINE_SLICES";

/// Startup slice modes from [`SLICES_ENV`]. When it is unset, every slice
/// is on bash, or under `QUARK_SHADOWS=all` every slice quarkd can shadow
/// is in shadow ([`crate::shadows::all_slices`]).
pub fn slices_from_env() -> Result<SliceSwitch, CoreError> {
    match std::env::var(SLICES_ENV) {
        Ok(spec) => SliceSwitch::parse(&spec),
        Err(_) if crate::shadows::all() => Ok(crate::shadows::all_slices()),
        Err(_) => Ok(SliceSwitch::new()),
    }
}

type Call<'a, T> = Pin<Box<dyn Future<Output = Result<T, EngineError>> + Send + 'a>>;

/// Compares a bash and a native answer: `None` when they agree, else what
/// to record for each side.
type Compare<'a, T> = &'a (dyn Fn(&T, &T) -> Option<(serde_json::Value, serde_json::Value)> + Sync);

/// What firstmate's last snapshot said about one task: where it read the
/// state, and the state.
type Seen = (Option<String>, TaskState);

/// Runs firstmate and the native engine side by side, per slice.
pub struct ShadowEngine {
    bash: Arc<dyn EngineAdapter>,
    native: Arc<dyn EngineAdapter>,
    switch: RwLock<SliceSwitch>,
    log: Arc<dyn EventLog>,
    host: HostId,
    /// Slices whose reads go to the native engine too; `None` for all.
    compared: Option<BTreeSet<Slice>>,
    /// Per project, per task: firstmate's last snapshot.
    seen: Mutex<HashMap<String, HashMap<String, Seen>>>,
    /// Later slices' checks of firstmate's snapshot.
    checks: Vec<Arc<dyn SnapshotCheck>>,
}

/// A later slice's view of the fleet, compared with every firstmate
/// snapshot while that slice is in shadow, the way slice 1 compares its own
/// (which tasks exist, and status-log states).
#[async_trait]
pub trait SnapshotCheck: Send + Sync {
    fn slice(&self) -> Slice;
    /// The operation its divergences are recorded under.
    fn operation(&self) -> &'static str;
    async fn view(&self, ws: &WorkspaceRef) -> Result<FleetSnapshot, EngineError>;
    /// Whether the slice switch turns it on. A check that returns false
    /// runs whenever it is installed, because its own flag decided that.
    fn switched(&self) -> bool {
        true
    }
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
            compared: None,
            seen: Mutex::new(HashMap::new()),
            checks: Vec::new(),
        }
    }

    /// Compare `check`'s view with firstmate's snapshot while its slice is
    /// in shadow.
    pub fn checking(mut self, check: Arc<dyn SnapshotCheck>) -> Self {
        self.checks.push(check);
        self
    }

    /// Run every check whose slice is in shadow against `bash`, asking both
    /// again before recording, as [`Self::read_compared`] does.
    async fn run_checks(&self, ws: &WorkspaceRef, bash: &FleetSnapshot) {
        for check in &self.checks {
            if check.switched() && self.mode(check.slice()) != SliceMode::Shadow {
                continue;
            }
            let Ok(native) = check.view(ws).await else {
                continue;
            };
            if fleet_view(bash, &unfinished_extras(bash, native)).is_none() {
                continue;
            }
            let (Ok(bash), Ok(native)) = (self.bash.snapshot(ws).await, check.view(ws).await)
            else {
                continue;
            };
            if let Some((b, n)) = fleet_view(&bash, &unfinished_extras(&bash, native)) {
                let d = Divergence {
                    slice: check.slice(),
                    operation: check.operation().to_string(),
                    bash: b,
                    native: n,
                };
                self.record(ProjectId::new(&ws.project_id), kinds::SHADOW_DIVERGENCE, &d)
                    .await;
            }
        }
    }

    /// Send only `slices`' reads to the native engine.
    pub fn comparing(mut self, slices: &[Slice]) -> Self {
        self.compared = Some(slices.iter().copied().collect());
        self
    }

    fn compares(&self, slice: Slice) -> bool {
        self.compared.as_ref().is_none_or(|c| c.contains(&slice))
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
        self.read_compared(slice, operation, project, call, None)
            .await
    }

    /// [`Self::read`], comparing two successful answers with `compare`
    /// instead of as whole values. A failure on either side still compares
    /// by [`outcome`].
    async fn read_compared<'a, T, F>(
        &'a self,
        slice: Slice,
        operation: &str,
        project: &str,
        call: F,
        compare: Option<Compare<'_, T>>,
    ) -> Result<T, EngineError>
    where
        T: Serialize,
        F: Fn(&'a dyn EngineAdapter) -> Call<'a, T>,
    {
        match self.mode(slice) {
            SliceMode::Bash => call(self.bash.as_ref()).await,
            SliceMode::Native => call(self.native.as_ref()).await,
            SliceMode::Shadow if !self.compares(slice) => call(self.bash.as_ref()).await,
            SliceMode::Shadow => {
                let mut bash = call(self.bash.as_ref()).await;
                let native = call(self.native.as_ref()).await;
                if differ(&bash, &native, compare).is_none() {
                    return bash;
                }
                // Ask both again: a file that changed between the two reads
                // is not a divergence.
                bash = call(self.bash.as_ref()).await;
                let native = call(self.native.as_ref()).await;
                if let Some((b, n)) = differ(&bash, &native, compare) {
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

    /// Slice 1's snapshot comparison; remembers firstmate's answer for
    /// [`Self::compare_holds`].
    fn compare_snapshot(
        &self,
        project: &str,
        bash: &FleetSnapshot,
        native: &FleetSnapshot,
    ) -> Option<(serde_json::Value, serde_json::Value)> {
        let seen: HashMap<String, Seen> = bash
            .tasks
            .iter()
            .map(|t| (t.id.clone(), (t.state_source.clone(), t.state)))
            .collect();
        self.seen.lock().unwrap().insert(project.to_string(), seen);
        fleet_view(bash, native)
    }

    /// Slice 1's holds comparison: the keyed decisions of tasks whose open
    /// decisions firstmate's last snapshot kept from the status log.
    fn compare_holds(
        &self,
        project: &str,
        bash: &[Hold],
        native: &[Hold],
    ) -> Option<(serde_json::Value, serde_json::Value)> {
        let seen = self.seen.lock().unwrap();
        let tasks = seen.get(project)?;
        holds_view(tasks, bash, native)
    }
}

/// Whether two answers differ, and if so what to record for each.
fn differ<T: Serialize>(
    bash: &Result<T, EngineError>,
    native: &Result<T, EngineError>,
    compare: Option<Compare<'_, T>>,
) -> Option<(serde_json::Value, serde_json::Value)> {
    if let (Ok(b), Ok(n), Some(compare)) = (bash, native, compare) {
        return compare(b, n);
    }
    let (b, n) = (outcome(bash), outcome(native));
    (b != n).then_some((b, n))
}

/// Firstmate's status-log word for a state: done covers in review.
fn state_word(s: TaskState) -> &'static str {
    match s {
        TaskState::Running => "working",
        TaskState::NeedsDecision => "parked",
        TaskState::Blocked => "blocked",
        TaskState::Paused => "paused",
        TaskState::InReview | TaskState::Done => "done",
        TaskState::Failed => "failed",
        TaskState::Queued | TaskState::Unknown => "unknown",
    }
}

/// Where firstmate read a task's state when that is the status log, the
/// only source the event log carries.
const STATUS_LOG: &str = "status-log";

/// Slice 1's fleet comparison: the same tasks, and the same state for each
/// task whose state firstmate read from the status log. Queued work with no
/// worker comes from firstmate's backlog, which the log does not carry.
fn fleet_view(
    bash: &FleetSnapshot,
    native: &FleetSnapshot,
) -> Option<(serde_json::Value, serde_json::Value)> {
    let ours: BTreeMap<&str, TaskState> = native
        .tasks
        .iter()
        .map(|t| (t.id.as_str(), t.state))
        .collect();
    let mut b = BTreeMap::new();
    let mut n = BTreeMap::new();
    for t in bash.tasks.iter().filter(|t| t.state != TaskState::Queued) {
        let judged = t.state_source.as_deref() == Some(STATUS_LOG);
        let word = |s: TaskState| judged.then(|| state_word(s));
        b.insert(t.id.as_str(), Some(word(t.state)));
        n.insert(t.id.as_str(), ours.get(t.id.as_str()).map(|s| word(*s)));
    }
    let extra: Vec<&str> = ours
        .keys()
        .filter(|id| !b.contains_key(*id))
        .copied()
        .collect();
    for id in extra {
        b.insert(id, None);
        n.insert(id, Some(None));
    }
    // `null`: no such task; `{"id": null}` style entries carry no state.
    let view = |m: BTreeMap<&str, Option<Option<&str>>>| {
        serde_json::Value::Object(
            m.into_iter()
                .map(|(id, v)| {
                    let v = match v {
                        None => serde_json::Value::Null,
                        Some(None) => serde_json::json!({}),
                        Some(Some(w)) => serde_json::json!({ "state": w }),
                    };
                    (id.to_string(), v)
                })
                .collect(),
        )
    };
    (b != n).then(|| (view(b), view(n)))
}

/// `native` without the finished tasks firstmate no longer lists: a later
/// slice's view may keep a task a while after firstmate cleaned it up.
fn unfinished_extras(bash: &FleetSnapshot, mut native: FleetSnapshot) -> FleetSnapshot {
    native.tasks.retain(|t| {
        bash.tasks.iter().any(|b| b.id == t.id)
            || !matches!(
                t.state,
                TaskState::InReview | TaskState::Done | TaskState::Failed
            )
    });
    native
}

/// Slice 1's holds comparison, for the tasks in `seen` whose open decisions
/// firstmate keeps from the status log. Firstmate drops a task's decisions
/// when its pane or validation run says it moved on, which the log can't
/// see, and once the task is done or failed. Captain holds come from the
/// backlog, which the log does not carry.
fn holds_view(
    seen: &HashMap<String, Seen>,
    bash: &[Hold],
    native: &[Hold],
) -> Option<(serde_json::Value, serde_json::Value)> {
    let kept = |task: &Option<String>| {
        let Some((source, state)) = task.as_ref().and_then(|t| seen.get(t)) else {
            return false;
        };
        let moved_on = matches!(source.as_deref(), Some("run-step" | "pane"))
            && !matches!(state, TaskState::NeedsDecision | TaskState::Blocked);
        let finished = matches!(
            state,
            TaskState::InReview | TaskState::Done | TaskState::Failed
        );
        !moved_on && !finished
    };
    let view = |holds: &[Hold]| {
        holds
            .iter()
            .filter(|h| h.id.contains(':') && kept(&h.task_id))
            .map(|h| (h.id.clone(), h.question.clone()))
            .collect::<BTreeMap<_, _>>()
    };
    let (b, n) = (view(bash), view(native));
    (b != n).then(|| (serde_json::json!(b), serde_json::json!(n)))
}

/// Slice 1's status tail comparison. The native read runs second, so a
/// line appended in between may be in its answer only; that is not a
/// divergence.
fn tail_view(
    bash: &StatusTail,
    native: &StatusTail,
) -> Option<(serde_json::Value, serde_json::Value)> {
    let later = native.entries.len() >= bash.entries.len()
        && native.entries[..bash.entries.len()] == bash.entries[..]
        && native.next_offset >= bash.next_offset;
    let same = native.entries == bash.entries && native.next_offset == bash.next_offset;
    (!same && !later).then(|| (outcome::<_>(&Ok(bash)), outcome::<_>(&Ok(native))))
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
        let project = ws.project_id.as_str();
        let snapshot = self
            .read_compared(
                Slice::EventLog,
                "snapshot",
                project,
                |e| e.snapshot(ws),
                Some(&|b: &FleetSnapshot, n: &FleetSnapshot| self.compare_snapshot(project, b, n)),
            )
            .await;
        if let Ok(s) = &snapshot {
            self.run_checks(ws, s).await;
        }
        snapshot
    }

    async fn status_tail(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
        offset: u64,
    ) -> Result<StatusTail, EngineError> {
        self.read_compared(
            Slice::EventLog,
            "status_tail",
            &ws.project_id,
            |e| e.status_tail(ws, task_id, offset),
            Some(&tail_view),
        )
        .await
    }

    async fn holds(&self, ws: &WorkspaceRef) -> Result<Vec<Hold>, EngineError> {
        let project = ws.project_id.as_str();
        self.read_compared(
            Slice::EventLog,
            "holds",
            project,
            |e| e.holds(ws),
            Some(&|b: &Vec<Hold>, n: &Vec<Hold>| self.compare_holds(project, b, n)),
        )
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
        resume: bool,
    ) -> Result<(), EngineError> {
        self.acting(Slice::SubCoordinators)
            .start_coordinator(command, ws, agent, account_env, resume)
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
    use crate::engine::{EngineTask, StatusEntry, StubEngine, StubWrite};
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
            state_source: Some(STATUS_LOG.into()),
            harness: None,
            pull_request_url: None,
            terminal: None,
            worktree: None,
        }
    }

    fn hold(id: &str, question: &str) -> Hold {
        Hold {
            id: id.into(),
            task_id: Some(id.split(':').next().unwrap().into()),
            question: question.into(),
            answer: None,
            answered_by: None,
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
        assert_eq!(d.bash["t1"]["state"], "working");
        assert_eq!(d.native["t1"]["state"], "blocked");

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

    #[test]
    fn the_fleet_compares_what_the_log_can_know() {
        let mut queued = task("q", TaskState::Queued);
        queued.state_source = None;
        let mut by_pane = task("p", TaskState::Running);
        by_pane.state_source = Some("pane".into());
        let bash = FleetSnapshot {
            tasks: vec![task("a", TaskState::InReview), by_pane, queued],
        };
        let native = FleetSnapshot {
            tasks: vec![
                task("a", TaskState::InReview),
                task("p", TaskState::Blocked),
            ],
        };
        assert_eq!(
            fleet_view(&bash, &native),
            None,
            "pane state and backlog skipped"
        );

        // A done ship task reads in review or done; both are done.
        let native_done = FleetSnapshot {
            tasks: vec![task("a", TaskState::Done), task("p", TaskState::Running)],
        };
        assert_eq!(fleet_view(&bash, &native_done), None);

        // A task only one side has.
        let extra = FleetSnapshot {
            tasks: vec![
                task("a", TaskState::InReview),
                task("p", TaskState::Running),
                task("x", TaskState::Running),
            ],
        };
        let (b, n) = fleet_view(&bash, &extra).unwrap();
        assert_eq!(b["x"], serde_json::Value::Null);
        assert_eq!(n["x"], serde_json::json!({}));
        let missing = FleetSnapshot {
            tasks: vec![task("p", TaskState::Running)],
        };
        let (b, n) = fleet_view(&bash, &missing).unwrap();
        assert_eq!(b["a"]["state"], "done");
        assert_eq!(n["a"], serde_json::Value::Null);
    }

    #[test]
    fn holds_compare_only_where_firstmate_keeps_the_log_s_decisions() {
        let mut seen = HashMap::new();
        seen.insert(
            "a".to_string(),
            (Some(STATUS_LOG.into()), TaskState::NeedsDecision),
        );
        seen.insert("b".to_string(), (Some("pane".into()), TaskState::Running));
        seen.insert(
            "c".to_string(),
            (Some("run-step".into()), TaskState::NeedsDecision),
        );
        let bash = [
            hold("a:k", "q"),
            hold("c:k", "gate"),
            hold("held-1", "captain"),
        ];
        let native = [hold("a:k", "q"), hold("b:k", "stale"), hold("c:k", "gate")];
        assert_eq!(holds_view(&seen, &bash, &native), None);
        let (b, n) = holds_view(&seen, &bash, &[hold("a:k", "other")]).unwrap();
        assert_eq!(b["c:k"], "gate");
        assert_eq!(n["a:k"], "other");
    }

    #[test]
    fn a_later_native_tail_is_not_a_divergence() {
        let entry = |raw: &str| StatusEntry {
            kind: "working".into(),
            decision_key: Some("default".into()),
            note: raw.into(),
            raw: raw.into(),
        };
        let bash = StatusTail {
            entries: vec![entry("a")],
            next_offset: 10,
        };
        let later = StatusTail {
            entries: vec![entry("a"), entry("b")],
            next_offset: 20,
        };
        assert_eq!(tail_view(&bash, &later), None);
        assert!(tail_view(&later, &bash).is_some());
        let other = StatusTail {
            entries: vec![entry("z")],
            next_offset: 10,
        };
        assert!(tail_view(&bash, &other).is_some());
    }

    #[tokio::test]
    async fn holds_wait_for_a_snapshot_and_slices_outside_comparing_stay_bash() {
        let r = rig("1=shadow,2=shadow");
        r.bash.set_holds(vec![hold("t1:k", "q")]);
        // No snapshot yet: nothing to judge holds by.
        r.engine.holds(&ws()).await.unwrap();
        assert!(r.log.events().is_empty());

        let engine = ShadowEngine::new(
            r.bash.clone(),
            r.native.clone(),
            SliceSwitch::parse("1=shadow,2=shadow").unwrap(),
            Arc::new(r.log.clone()),
            HostId::from("local"),
        )
        .comparing(&[]);
        engine.snapshot(&ws()).await.unwrap();
        assert!(r.log.events().is_empty());
    }

    /// Answers `first` once, then `rest`.
    struct Flaky {
        calls: Mutex<u32>,
        first: TaskState,
        rest: TaskState,
    }

    #[async_trait]
    impl EngineAdapter for Flaky {
        fn name(&self) -> &'static str {
            "flaky"
        }
        async fn snapshot(&self, _: &WorkspaceRef) -> Result<FleetSnapshot, EngineError> {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            let state = if *calls == 1 { self.first } else { self.rest };
            Ok(FleetSnapshot {
                tasks: vec![task("t1", state)],
            })
        }
        async fn status_tail(
            &self,
            _: &WorkspaceRef,
            _: &str,
            _: u64,
        ) -> Result<StatusTail, EngineError> {
            unimplemented!()
        }
        async fn holds(&self, _: &WorkspaceRef) -> Result<Vec<Hold>, EngineError> {
            unimplemented!()
        }
        async fn send_message(
            &self,
            _: &WorkspaceRef,
            _: &str,
            _: &str,
        ) -> Result<(), EngineError> {
            unimplemented!()
        }
        async fn answer(
            &self,
            _: &WorkspaceRef,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<(), EngineError> {
            unimplemented!()
        }
        async fn control(
            &self,
            _: &WorkspaceRef,
            _: &str,
            _: &TaskControl,
        ) -> Result<(), EngineError> {
            unimplemented!()
        }
        async fn merge_pull_request(
            &self,
            _: &WorkspaceRef,
            _: &str,
            _: &str,
            _: Option<MergeMethod>,
        ) -> Result<(), EngineError> {
            unimplemented!()
        }
        async fn set_standing_approval(
            &self,
            _: &WorkspaceRef,
            _: &[String],
            _: bool,
        ) -> Result<(), EngineError> {
            unimplemented!()
        }
        async fn add_source(
            &self,
            _: &Path,
            _: &SourceRepo,
            _: DeliveryPolicy,
        ) -> Result<(), EngineError> {
            unimplemented!()
        }
        async fn seed_workspace(
            &self,
            _: &Path,
            _: &WorkspacePlan,
        ) -> Result<PathBuf, EngineError> {
            unimplemented!()
        }
        async fn start_coordinator(
            &self,
            _: &Path,
            _: &WorkspaceRef,
            _: &AgentConfig,
            _: &[(String, String)],
            _: bool,
        ) -> Result<(), EngineError> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn a_disagreement_that_clears_on_a_second_read_is_not_recorded() {
        let bash = Arc::new(Flaky {
            calls: Mutex::new(0),
            first: TaskState::Running,
            rest: TaskState::Blocked,
        });
        let native = Arc::new(StubEngine::new());
        native.set_snapshot(FleetSnapshot {
            tasks: vec![task("t1", TaskState::Blocked)],
        });
        let log = MemoryEventLog::new();
        let engine = ShadowEngine::new(
            bash,
            native,
            SliceSwitch::parse("1=shadow").unwrap(),
            Arc::new(log.clone()),
            HostId::from("local"),
        );
        let snap = engine.snapshot(&ws()).await.unwrap();
        assert_eq!(
            snap.tasks[0].state,
            TaskState::Blocked,
            "the second answer is served"
        );
        assert!(log.events().is_empty());
    }

    struct FixedCheck(FleetSnapshot);

    #[async_trait]
    impl SnapshotCheck for FixedCheck {
        fn slice(&self) -> Slice {
            Slice::Supervision
        }
        fn operation(&self) -> &'static str {
            "supervised_state"
        }
        async fn view(&self, _: &WorkspaceRef) -> Result<FleetSnapshot, EngineError> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn snapshot_checks_run_only_while_their_slice_shadows() {
        let check = || {
            Arc::new(FixedCheck(FleetSnapshot {
                tasks: vec![task("t1", TaskState::Blocked)],
            }))
        };
        let r = rig("1=shadow,2=shadow,3=shadow");
        let engine = r.engine.comparing(&[]).checking(check());
        engine.snapshot(&ws()).await.unwrap();
        assert!(r.log.events().is_empty(), "slice 4 is bash");

        let r = rig("1=shadow,2=shadow,3=shadow,4=shadow");
        let engine = r.engine.comparing(&[]).checking(check());
        let snap = engine.snapshot(&ws()).await.unwrap();
        assert_eq!(snap.tasks[0].state, TaskState::Running, "bash still serves");
        let events = r.log.events();
        assert_eq!(events.len(), 1);
        let d: Divergence = events[0].decode().unwrap();
        assert_eq!(d.slice, Slice::Supervision);
        assert_eq!(d.operation, "supervised_state");
        assert_eq!(d.native["t1"]["state"], "blocked");
    }
}
