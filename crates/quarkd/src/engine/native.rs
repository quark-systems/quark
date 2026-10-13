//! The native engine: quarkd's own engine, with no firstmate behind it.
//!
//! [`NativeEngine`] serves the adapter seam from the native slices:
//!
//! - reads (`snapshot`, `status_tail`, `holds`) come from the event log:
//!   task state from the [`quark_eventlog::TaskLedger`] the supervisor
//!   writes, workers from its [`quark_supervisor::Fleet`], and each task's
//!   messages from the `worker.message` events the worker protocol records
//!   ([`WorkerLines`]); a running worker's endpoint is its PTY session,
//!   which [`crate::sessions`] streams as the task's terminal;
//! - steering, answers and lifecycle control go to the native
//!   [`Supervisor`] (slice 4);
//! - merges, standing approval and the verification gates go to
//!   [`NativeVerify`] (slice 2).
//! - adding a repo, seeding a Project workspace and starting its
//!   coordinator go to [`ProjectSetup`], when the engine has one.
//!
//! Operations whose native slice is not wired here yet are refused with
//! [`EngineError::Invalid`] naming the slice, never silently ignored.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use quark_core::worker::{WorkerEnvelope, WorkerMessage};
use quark_core::{
    replay, CoreError, Event, EventLog, ProjectId, ReadModel, Seq, Slice, SliceMode, SliceSwitch,
    TaskId,
};
use quark_eventlog::TaskRecord;
use quark_supervisor::{Relaunch, Supervisor, Worker};
use quark_systems::{AgentConfig, DeliveryPolicy, Evidence, MergeMethod, TaskKind};

use super::native_dispatch_engine::{self, NativeDispatch};
use super::native_project::ProjectSetup;
use super::native_verify::NativeVerify;
use super::{
    EngineAdapter, EngineError, EngineResolution, EngineSpawn, EngineTask, FleetSnapshot, Hold,
    NewTask, SourceRepo, StatusEntry, StatusTail, TaskControl, TaskStart, WorkspacePlan,
    WorkspaceRef,
};

/// `state_source` of every task this engine reports.
pub const SOURCE: &str = "native";
/// The name of the PTY session backend ([`quark_sessions::pty`]), whose
/// sessions the app's worker terminals show.
const PTY_BACKEND: &str = "pty";
const REPLAY_BATCH: usize = 512;
/// The task part of the red-main decision's hold id, `red-main:<decision>`.
/// Task ids never contain this exact word followed by a colon.
pub const RED_MAIN: &str = "red-main";
/// The answer that overrides the red-main guardrail.
pub const MERGE_ANYWAY: &str = "merge anyway";

fn red_main_brief() -> quark_systems::DecisionBrief {
    quark_systems::DecisionBrief {
        context: Some(
            "Merges wait while main is red, except a change that fixes it. \
             Overriding lets every green, verified pull request merge until main is green again."
                .into(),
        ),
        options: vec![quark_systems::DecisionOption {
            label: MERGE_ANYWAY.into(),
            consequence: Some("Green pull requests merge until main is green.".into()),
        }],
        asked_by: Some("merge guard".into()),
        ..Default::default()
    }
}

pub struct NativeEngine {
    log: Arc<dyn EventLog>,
    supervisor: Arc<Supervisor>,
    lines: WorkerLines,
    dispatch: Option<Arc<NativeDispatch>>,
    verify: NativeVerify,
    /// Project setup and coordinator start; refused without it.
    project: Option<Arc<ProjectSetup>>,
}

impl NativeEngine {
    /// Reads and merges GitHub through `gh`.
    pub fn new(log: Arc<dyn EventLog>, supervisor: Arc<Supervisor>) -> Self {
        Self::with_forge(log, supervisor, Arc::new(quark_verify::GhForge::default()))
    }

    pub fn with_forge(
        log: Arc<dyn EventLog>,
        supervisor: Arc<Supervisor>,
        forge: Arc<dyn quark_verify::Forge>,
    ) -> Self {
        let host = supervisor.config().host.clone();
        Self {
            verify: NativeVerify::new(forge, log.clone(), host),
            log,
            supervisor,
            lines: WorkerLines::default(),
            dispatch: None,
            project: None,
        }
    }

    /// Start tasks and resolve dispatch through `dispatch` (slice 5).
    pub fn with_dispatch(mut self, dispatch: Arc<NativeDispatch>) -> Self {
        self.dispatch = Some(dispatch);
        self
    }

    fn dispatch(&self) -> Result<&NativeDispatch, EngineError> {
        self.dispatch
            .as_deref()
            .map_or_else(|| Self::refuse("starting a task", "dispatch (slice 5)"), Ok)
    }

    pub fn verify(&self) -> &NativeVerify {
        &self.verify
    }

    /// Creates Projects and starts their coordinators with `setup`.
    pub fn with_project_setup(mut self, setup: Arc<ProjectSetup>) -> Self {
        self.project = Some(setup);
        self
    }

    pub fn supervisor(&self) -> &Arc<Supervisor> {
        &self.supervisor
    }

    async fn tasks(&self, ws: &WorkspaceRef) -> Result<Vec<(TaskRecord, Worker)>, EngineError> {
        self.supervisor
            .tasks(&project(ws))
            .await
            .map_err(engine_error)
    }

    async fn catch_up(&self) -> Result<(), EngineError> {
        replay(self.log.as_ref(), &self.lines, REPLAY_BATCH)
            .await
            .map(drop)
            .map_err(engine_error)
    }

    fn refuse<T>(op: &str, slice: &str) -> Result<T, EngineError> {
        Err(EngineError::Invalid(format!(
            "{op} needs {slice}, which the native engine does not serve yet"
        )))
    }
}

fn project(ws: &WorkspaceRef) -> ProjectId {
    ProjectId::new(ws.project_id.clone())
}

fn task_id(task: &str) -> Result<TaskId, EngineError> {
    quark_engine::validate_task_id(task).map_err(|_| EngineError::TaskNotFound(task.into()))?;
    Ok(TaskId::from(task))
}

fn engine_error(e: CoreError) -> EngineError {
    match e {
        CoreError::NotFound(m) => EngineError::TaskNotFound(m),
        CoreError::Invalid(m) | CoreError::Refused(m) | CoreError::Unsupported(m) => {
            EngineError::Invalid(m)
        }
        other => EngineError::Command(other.to_string()),
    }
}

fn engine_task(record: &TaskRecord, worker: &Worker, last: Option<&StatusEntry>) -> EngineTask {
    let current = worker.current.as_ref();
    EngineTask {
        id: worker.task.to_string(),
        title: record
            .title
            .clone()
            .unwrap_or_else(|| worker.assignment.title.clone()),
        // Native tasks have no scout kind yet: every one delivers a change.
        kind: Some(TaskKind::Ship),
        state: record.state,
        state_note: last.map(|e| e.note.clone()).filter(|n| !n.is_empty()),
        state_source: Some(SOURCE.to_string()),
        harness: Some(
            current
                .map(|g| g.harness.clone())
                .unwrap_or_else(|| worker.assignment.harness.clone()),
        ),
        pull_request_url: record.pull_request.clone(),
        // The current generation's PTY session while its program runs; a
        // relaunch names the new one, and the terminal follows it.
        terminal: current
            .filter(|g| g.backend == PTY_BACKEND && g.exited.is_none())
            .map(|g| crate::sessions::pty::target(&g.session)),
        worktree: worker
            .worktree
            .as_ref()
            .filter(|_| !worker.is_released())
            .map(|w| w.path.clone()),
    }
}

#[async_trait]
impl EngineAdapter for NativeEngine {
    fn name(&self) -> &'static str {
        "native"
    }

    fn slices(&self) -> SliceSwitch {
        let mut s = SliceSwitch::new();
        for slice in [
            Slice::EventLog,
            Slice::Verification,
            Slice::WorkerProtocol,
            Slice::Supervision,
        ] {
            // Infallible for a fresh switch.
            let _ = s.set(slice, SliceMode::Native, "native engine");
        }
        s
    }

    async fn snapshot(&self, ws: &WorkspaceRef) -> Result<FleetSnapshot, EngineError> {
        self.catch_up().await?;
        let p = project(ws);
        let mut tasks: Vec<EngineTask> = self
            .tasks(ws)
            .await?
            .iter()
            .map(|(r, w)| engine_task(r, w, self.lines.last(&p, &w.task).as_ref()))
            .collect();
        if let Some(d) = &self.dispatch {
            let running = tasks.iter().map(|t| t.id.clone()).collect();
            tasks.extend(d.queued(ws, &running).await?);
        }
        Ok(FleetSnapshot { tasks })
    }

    async fn status_tail(
        &self,
        ws: &WorkspaceRef,
        task: &str,
        offset: u64,
    ) -> Result<StatusTail, EngineError> {
        let task = task_id(task)?;
        self.catch_up().await?;
        let lines = self.lines.get(&project(ws), &task);
        let start = (offset as usize).min(lines.len());
        Ok(StatusTail {
            entries: lines[start..].to_vec(),
            next_offset: lines.len() as u64,
        })
    }

    async fn holds(&self, ws: &WorkspaceRef) -> Result<Vec<Hold>, EngineError> {
        self.catch_up().await?;
        let p = project(ws);
        let mut holds = Vec::new();
        for (record, worker) in self.tasks(ws).await? {
            for key in &record.open_decisions {
                let question = self
                    .lines
                    .question(&p, &worker.task, key)
                    .unwrap_or_else(|| format!("{} is waiting on {key}", worker.task));
                holds.push(Hold {
                    id: format!("{}:{key}", worker.task),
                    task_id: Some(worker.task.to_string()),
                    question,
                    answer: None,
                    answered_by: None,
                    brief: super::firstmate::worker_brief(worker.task.as_str()),
                });
            }
        }
        if let Some((decision, question)) = self.verify.red_main_question(&p).await? {
            holds.push(Hold {
                id: format!("{RED_MAIN}:{decision}"),
                task_id: None,
                question,
                answer: None,
                answered_by: None,
                brief: red_main_brief(),
            });
        }
        Ok(holds)
    }

    async fn send_message(
        &self,
        ws: &WorkspaceRef,
        task: &str,
        text: &str,
    ) -> Result<(), EngineError> {
        self.supervisor
            .steer(&project(ws), &task_id(task)?, text)
            .await
            .map(drop)
            .map_err(engine_error)
    }

    async fn answer(
        &self,
        ws: &WorkspaceRef,
        hold_id: &str,
        answer: &str,
        _answered_by: &str,
    ) -> Result<(), EngineError> {
        let (task, key) = hold_id
            .split_once(':')
            .ok_or_else(|| EngineError::Invalid(format!("hold id {hold_id} is not task:key")))?;
        if task == RED_MAIN {
            // Only an explicit "merge anyway" overrides the guardrail;
            // waiting needs nothing, so the question stays until main is
            // green.
            if !answer.trim().to_lowercase().starts_with(MERGE_ANYWAY) {
                return Err(EngineError::Invalid(format!(
                    "answer \"{MERGE_ANYWAY}\" to override a red main; it clears when main is green"
                )));
            }
            return self.verify.override_red_main(key).await;
        }
        self.supervisor
            .answer(&project(ws), &task_id(task)?, key, answer)
            .await
            .map(drop)
            .map_err(engine_error)
    }

    async fn control(
        &self,
        ws: &WorkspaceRef,
        task: &str,
        action: &TaskControl,
    ) -> Result<(), EngineError> {
        let (p, t) = (project(ws), task_id(task)?);
        match action {
            TaskControl::Cancel => self.supervisor.cancel(&p, &t).await,
            TaskControl::Relaunch {
                harness,
                model,
                effort,
                note,
                account_env,
            } => self
                .supervisor
                .relaunch(
                    &p,
                    &t,
                    Relaunch {
                        harness: harness.clone(),
                        model: model.clone(),
                        effort: effort.clone(),
                        note: note.clone(),
                        env: (!account_env.is_empty())
                            .then(|| account_env.iter().cloned().collect()),
                    },
                )
                .await
                .map(drop),
        }
        .map_err(engine_error)
    }

    async fn spawn(
        &self,
        ws: &WorkspaceRef,
        task: &str,
    ) -> Result<Option<EngineSpawn>, EngineError> {
        let (p, t) = (project(ws), task_id(task)?);
        let Some((_, w)) = self.supervisor.task(&p, &t) else {
            return Ok(None);
        };
        Ok(w.current.map(|g| EngineSpawn {
            generation: g.id,
            harness: g.harness,
            model: g.model,
            effort: g.effort,
            spawned_at: None,
            project: w
                .assignment
                .repo
                .file_name()
                .map(|n| n.to_string_lossy().into_owned()),
        }))
    }

    async fn merge_pull_request(
        &self,
        ws: &WorkspaceRef,
        task: &str,
        url: &str,
        method: Option<MergeMethod>,
    ) -> Result<(), EngineError> {
        self.verify
            .merge(&self.supervisor, ws, &task_id(task)?, url, method)
            .await
    }

    async fn pull_request_merged(
        &self,
        ws: &WorkspaceRef,
        task: &str,
        url: &str,
    ) -> Result<(), EngineError> {
        self.verify
            .merged_outside(&self.supervisor, ws, &task_id(task)?, url)
            .await
    }

    async fn set_standing_approval(
        &self,
        ws: &WorkspaceRef,
        repos: &[String],
        on: bool,
    ) -> Result<(), EngineError> {
        self.verify.set_standing_approval(ws, repos, on).await
    }

    async fn set_gates(&self, ws: &WorkspaceRef, config: &str) -> Result<(), EngineError> {
        self.verify.set_gates(ws, config)
    }

    async fn gate_evidence(
        &self,
        ws: &WorkspaceRef,
        task: &str,
    ) -> Result<Option<Evidence>, EngineError> {
        self.verify.gate_evidence(ws, &task_id(task)?)
    }

    fn gate_artifact(
        &self,
        ws: &WorkspaceRef,
        task: &str,
        path: &str,
    ) -> Result<PathBuf, EngineError> {
        self.verify.gate_artifact(ws, &task_id(task)?, path)
    }

    /// Delivery is the Project's own setting in the store; the clone does
    /// not carry it.
    async fn add_source(
        &self,
        command: &Path,
        source: &SourceRepo,
        _: DeliveryPolicy,
    ) -> Result<(), EngineError> {
        match &self.project {
            Some(p) => p.add_source(command, source).await,
            None => Self::refuse("adding a repo", "native Project setup"),
        }
    }

    async fn seed_workspace(
        &self,
        command: &Path,
        plan: &WorkspacePlan,
    ) -> Result<PathBuf, EngineError> {
        match &self.project {
            Some(p) => p.seed_workspace(command, plan).await,
            None => Self::refuse("creating a Project", "native Project setup"),
        }
    }

    async fn start_coordinator(
        &self,
        command: &Path,
        ws: &WorkspaceRef,
        agent: &AgentConfig,
        account_env: &[(String, String)],
        resume: bool,
    ) -> Result<(), EngineError> {
        match &self.project {
            Some(p) => {
                p.start_coordinator(command, ws, agent, account_env, resume)
                    .await
            }
            None => Self::refuse(
                "starting the coordinator",
                "the native coordinator (slice 6)",
            ),
        }
    }

    async fn coordinator_terminals(
        &self,
        command: &Path,
    ) -> Result<HashMap<String, String>, EngineError> {
        match &self.project {
            Some(p) => p.coordinator_terminals(command),
            None => Ok(HashMap::new()),
        }
    }

    fn account_envs(&self) -> &'static [&'static str] {
        native_dispatch_engine::ACCOUNT_ENVS
    }

    async fn start_task(
        &self,
        ws: &WorkspaceRef,
        task: &NewTask,
    ) -> Result<TaskStart, EngineError> {
        self.dispatch()?.start(ws, task).await
    }

    async fn choose_profile(
        &self,
        ws: &WorkspaceRef,
        task: &str,
        agent: &AgentConfig,
    ) -> Result<TaskStart, EngineError> {
        self.dispatch()?.choose(ws, task, agent).await
    }

    async fn choose_rule(
        &self,
        ws: &WorkspaceRef,
        task: &str,
        rule: &str,
    ) -> Result<TaskStart, EngineError> {
        self.dispatch()?.choose_rule(ws, task, rule).await
    }

    async fn resolve_dispatch(
        &self,
        ws: &WorkspaceRef,
        task: &str,
        _: Option<&str>,
    ) -> Result<Option<EngineResolution>, EngineError> {
        match &self.dispatch {
            Some(d) => d.resolution(ws, task).await,
            None => Ok(None),
        }
    }

    async fn resolve_description(
        &self,
        ws: &WorkspaceRef,
        description: &str,
    ) -> Result<Option<EngineResolution>, EngineError> {
        Ok(match &self.dispatch {
            Some(d) => Some(d.describe(ws, description).await),
            None => None,
        })
    }

    async fn set_crew_dispatch(&self, ws: &WorkspaceRef, config: &str) -> Result<(), EngineError> {
        match &self.dispatch {
            Some(d) => d.set_config(ws, config).await,
            None => Ok(()),
        }
    }
}

type Key = (ProjectId, TaskId);

/// Each task's worker messages, as status lines, folded from
/// `worker.message` events. A line's offset is its index, so a status tail
/// resumes where the last one stopped. Messages from replaced generations
/// are kept: they were said, and the task's history shows them.
#[derive(Default)]
pub struct WorkerLines {
    inner: Mutex<LinesInner>,
}

#[derive(Default)]
struct LinesInner {
    applied: Seq,
    lines: HashMap<Key, Vec<StatusEntry>>,
    questions: HashMap<(Key, String), String>,
}

impl WorkerLines {
    fn lock(&self) -> std::sync::MutexGuard<'_, LinesInner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn get(&self, project: &ProjectId, task: &TaskId) -> Vec<StatusEntry> {
        self.lock()
            .lines
            .get(&(project.clone(), task.clone()))
            .cloned()
            .unwrap_or_default()
    }

    /// The newest line that says something about the task's state.
    pub fn last(&self, project: &ProjectId, task: &TaskId) -> Option<StatusEntry> {
        self.lock()
            .lines
            .get(&(project.clone(), task.clone()))?
            .last()
            .cloned()
    }

    /// The latest wording of the question asked under `key`.
    pub fn question(&self, project: &ProjectId, task: &TaskId, key: &str) -> Option<String> {
        self.lock()
            .questions
            .get(&((project.clone(), task.clone()), key.to_string()))
            .cloned()
    }
}

/// The status line a worker message reads as; `None` for harness signals,
/// which say nothing a person reads.
pub fn status_entry(message: &WorkerMessage) -> Option<StatusEntry> {
    let (kind, key, note) = match message {
        WorkerMessage::Report { state, note, key } => (state.clone(), key.clone(), note.clone()),
        WorkerMessage::Ask { key, question } => {
            ("needs-decision".into(), Some(key.clone()), question.clone())
        }
        WorkerMessage::Learned { fact } => ("note".into(), None, fact.clone()),
        WorkerMessage::Done {
            summary,
            pull_request,
        } => (
            "done".into(),
            None,
            match pull_request {
                Some(pr) => format!("PR {pr} {summary}").trim().to_string(),
                None => summary.clone(),
            },
        ),
        WorkerMessage::Signal { .. } => return None,
    };
    let raw = match &key {
        Some(k) => format!("{kind}: [{k}] {note}"),
        None => format!("{kind}: {note}"),
    };
    Some(StatusEntry {
        kind,
        decision_key: key,
        note,
        raw,
    })
}

#[async_trait]
impl ReadModel for WorkerLines {
    async fn applied_through(&self) -> quark_core::Result<Seq> {
        Ok(self.lock().applied)
    }

    async fn apply(&self, event: &Event) -> quark_core::Result<()> {
        let mut inner = self.lock();
        if event.seq <= inner.applied {
            return Ok(());
        }
        inner.applied = event.seq;
        if event.kind.as_str() != quark_core::event::kinds::WORKER {
            return Ok(());
        }
        // A malformed payload is skipped, not fatal: the log stays readable.
        let Ok(envelope) = event.decode::<WorkerEnvelope>() else {
            return Ok(());
        };
        let key = (event.project.clone(), envelope.task.clone());
        if let WorkerMessage::Ask { key: k, question } = &envelope.message {
            inner
                .questions
                .insert((key.clone(), k.clone()), question.clone());
        }
        if let Some(entry) = status_entry(&envelope.message) {
            inner.lines.entry(key).or_default().push(entry);
        }
        Ok(())
    }

    async fn reset(&self) -> quark_core::Result<()> {
        *self.lock() = LinesInner::default();
        Ok(())
    }
}
