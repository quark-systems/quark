//! The supervisor: spawn, steer, control, watch and recover workers.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash as _, Hasher as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use quark_core::isolation::ProcessSpec;
use quark_core::session::{SessionBackend, SessionId, SessionInfo, SessionSpec, TermSize};
use quark_core::worker::{WorkerEnvelope, WorkerMessage};
use quark_core::worktree::{ReturnOutcome, WorktreeRequest};
use quark_core::{
    replay, CoreError, EventLog, HostId, Isolation, NewEvent, ProjectId, Result, Seq, TaskEvent,
    TaskId, WorktreeProvider,
};
use quark_eventlog::{TaskLedger, TaskRecord};
use quark_harness::ManifestRegistry;
use quark_systems::TaskState;
use quark_worker::{Recorder, StatusFile, WorkerIdentity};
use tokio::sync::OwnedMutexGuard;

use crate::events::{Assignment, Cause, SupervisorEvent};
use crate::fleet::{is_worker_message, Fleet, Generation, Worker};
use crate::launch::{self, WorkerUrls};

/// Events read per batch when replaying or handling worker messages.
const BATCH: usize = 512;

/// Tunables. [`Config::new`] has the defaults.
#[derive(Debug, Clone)]
pub struct Config {
    /// This machine, stamped on every event.
    pub host: HostId,
    /// Where each generation's brief and status file live, outside the
    /// worktree so nothing of the supervisor's is ever committed.
    pub state_dir: PathBuf,
    /// Where workers reach the worker protocol over HTTP, if anywhere.
    pub worker_urls: Option<WorkerUrls>,
    pub size: TermSize,
    /// Longest to wait for a pasted-prompt harness to be ready before
    /// typing its brief anyway.
    pub ready_timeout: Duration,
    /// How long the screen must stay still to count as ready.
    pub ready_quiet: Duration,
    /// No screen change or worker message for this long is reported stale.
    pub stale_after: Duration,
    /// Times in a row the supervisor relaunches a worker whose session
    /// ended unexpectedly before failing the task.
    pub max_recoveries: u32,
    /// How long a cancelled worker gets to exit before its session is
    /// killed.
    pub exit_grace: Duration,
}

impl Config {
    pub fn new(host: HostId, state_dir: impl Into<PathBuf>) -> Self {
        Self {
            host,
            state_dir: state_dir.into(),
            worker_urls: None,
            size: TermSize::default(),
            ready_timeout: Duration::from_secs(30),
            ready_quiet: Duration::from_millis(1500),
            stale_after: Duration::from_secs(15 * 60),
            max_recoveries: 2,
            exit_grace: Duration::from_secs(3),
        }
    }
}

/// A new task for the supervisor.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    pub project: ProjectId,
    pub task: TaskId,
    pub assignment: Assignment,
}

/// A caller-requested replacement of a task's worker.
#[derive(Debug, Clone, Default)]
pub struct Relaunch {
    /// Another harness, model or effort; `None` keeps the current one.
    pub harness: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Told to the new worker before its brief.
    pub note: String,
    /// Replaces the assignment's extra environment, such as another
    /// account; `None` keeps it.
    pub env: Option<BTreeMap<String, String>>,
}

/// What the last [`Supervisor::tick`] did, for logs and tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Tick {
    /// Worker messages turned into task transitions.
    pub transitions: usize,
    pub delivered: usize,
    pub exited: usize,
    pub recovered: usize,
    pub stale: usize,
    pub resumed_spawns: usize,
}

/// What the supervisor watches about one generation in memory. Lost on a
/// crash and rebuilt from scratch, which is safe: the status file is
/// re-read from the start (its events are idempotent) and staleness starts
/// counting again.
struct Watch {
    generation: String,
    status: StatusFile,
    screen: u64,
    active_at: Instant,
    stale_reported: bool,
}

type Key = (ProjectId, TaskId);

/// Supervises workers: each one runs in its own isolated worktree, in a
/// session, on a harness described by a manifest, optionally sandboxed.
///
/// Every change is an event first. Task state goes through the
/// [`TaskLedger`]; everything else the supervisor must remember is a
/// `supervisor.*` event folded into the [`Fleet`]. A new supervisor on the
/// same log and session backend therefore picks up where a crashed one
/// stopped: [`Supervisor::recover`] adopts the sessions that survived and
/// [`Supervisor::tick`] relaunches the ones that did not.
pub struct Supervisor {
    log: Arc<dyn EventLog>,
    ledger: TaskLedger,
    fleet: Fleet,
    manifests: Arc<ManifestRegistry>,
    worktrees: Arc<dyn WorktreeProvider>,
    sessions: Arc<dyn SessionBackend>,
    isolation: Arc<dyn Isolation>,
    recorder: Arc<Recorder>,
    config: Config,
    locks: Mutex<HashMap<Key, Arc<tokio::sync::Mutex<()>>>>,
    watches: Mutex<HashMap<Key, Watch>>,
    handling: tokio::sync::Mutex<Seq>,
}

impl Supervisor {
    /// A supervisor recovered from `log`. Call [`Supervisor::recover`]
    /// next, then [`Supervisor::tick`] on a timer.
    pub async fn open(
        log: Arc<dyn EventLog>,
        manifests: Arc<ManifestRegistry>,
        worktrees: Arc<dyn WorktreeProvider>,
        sessions: Arc<dyn SessionBackend>,
        isolation: Arc<dyn Isolation>,
        config: Config,
    ) -> Result<Self> {
        let ledger = TaskLedger::open(log.clone(), config.host.clone()).await?;
        let fleet = Fleet::new(manifests.clone());
        replay(log.as_ref(), &fleet, BATCH).await?;
        let recorder = Arc::new(Recorder::new(
            log.clone(),
            Arc::new(fleet.clone()),
            config.host.clone(),
        ));
        let handled = fleet.handled_through();
        Ok(Self {
            log,
            ledger,
            fleet,
            manifests,
            worktrees,
            sessions,
            isolation,
            recorder,
            config,
            locks: Mutex::default(),
            watches: Mutex::default(),
            handling: tokio::sync::Mutex::new(handled),
        })
    }

    /// The worker protocol's recorder, bound to this supervisor's tasks, for
    /// `quark_worker::router` and the MCP server.
    pub fn recorder(&self) -> Arc<Recorder> {
        self.recorder.clone()
    }

    pub fn fleet(&self) -> &Fleet {
        &self.fleet
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The task as the ledger and the fleet see it.
    pub fn task(&self, project: &ProjectId, task: &TaskId) -> Option<(TaskRecord, Worker)> {
        let record = self.ledger.states().get(project, task)?;
        Some((record, self.fleet.get(project, task)?))
    }

    fn state(&self, project: &ProjectId, task: &TaskId) -> TaskState {
        self.ledger.states().state(project, task)
    }

    async fn refresh(&self) -> Result<()> {
        self.ledger.refresh().await?;
        replay(self.log.as_ref(), &self.fleet, BATCH).await?;
        Ok(())
    }

    async fn record(&self, project: &ProjectId, task: &TaskId, e: SupervisorEvent) -> Result<()> {
        let event = NewEvent::new(
            self.config.host.clone(),
            project.clone(),
            Some(task.clone()),
            e.kind(),
            serde_json::to_value(&e).map_err(|e| CoreError::Invalid(e.to_string()))?,
        );
        self.log.append(event).await?;
        replay(self.log.as_ref(), &self.fleet, BATCH).await?;
        Ok(())
    }

    async fn transition(&self, project: &ProjectId, task: &TaskId, e: TaskEvent) -> Result<()> {
        self.ledger
            .record(project.clone(), task.clone(), e)
            .await
            .map(drop)
    }

    async fn lock(&self, key: &Key) -> OwnedMutexGuard<()> {
        self.task_lock(key).lock_owned().await
    }

    fn task_lock(&self, key: &Key) -> Arc<tokio::sync::Mutex<()>> {
        self.locks
            .lock()
            .unwrap()
            .entry(key.clone())
            .or_default()
            .clone()
    }

    fn worker(&self, project: &ProjectId, task: &TaskId) -> Result<Worker> {
        self.fleet
            .get(project, task)
            .ok_or_else(|| CoreError::NotFound(format!("task {task} in {project}")))
    }

    // ---- spawn ---------------------------------------------------------

    /// Start the first worker on a new task: queue it, take an isolated
    /// worktree on a new branch, and launch its harness in a session. A
    /// task that fails to launch is marked failed and keeps its worktree.
    pub async fn spawn(&self, req: SpawnRequest) -> Result<Worker> {
        let SpawnRequest {
            project,
            task,
            assignment,
        } = req;
        if self.manifests.resolve(&assignment.harness).is_none() {
            return Err(CoreError::NotFound(format!(
                "harness {}",
                assignment.harness
            )));
        }
        if !self.isolation.modes().contains(&assignment.isolation) {
            return Err(CoreError::Unsupported(format!(
                "isolation {:?} on this host",
                assignment.isolation
            )));
        }
        let key = (project.clone(), task.clone());
        let _guard = self.lock(&key).await;
        self.refresh().await?;
        if self.state(&project, &task) != TaskState::Unknown {
            return Err(CoreError::Invalid(format!("task {task} already exists")));
        }
        self.transition(
            &project,
            &task,
            TaskEvent::Queued {
                title: assignment.title.clone(),
            },
        )
        .await?;
        self.record(&project, &task, SupervisorEvent::Assigned { assignment })
            .await?;
        self.start(&project, &task).await?;
        self.worker(&project, &task)
    }

    /// Take the worktree if the task has none yet, then launch its first
    /// worker. Also finishes a spawn a crash interrupted.
    async fn start(&self, project: &ProjectId, task: &TaskId) -> Result<()> {
        let w = self.worker(project, task)?;
        let result = async {
            if w.worktree.is_none() {
                let a = &w.assignment;
                let wt = self
                    .worktrees
                    .get(&WorktreeRequest {
                        project: project.clone(),
                        task: task.clone(),
                        repo: a.repo.clone(),
                        branch: a.branch.clone(),
                        base: a.base.clone(),
                    })
                    .await?;
                self.record(project, task, SupervisorEvent::Worktree { worktree: wt })
                    .await?;
            }
            self.launch(project, task, Cause::Spawn, None, None).await
        }
        .await;
        if let Err(e) = &result {
            // Leave a record a person can act on; the worktree stays.
            let reason = format!("could not start a worker: {e}");
            if let Err(e2) = self
                .transition(project, task, TaskEvent::Failed { reason })
                .await
            {
                tracing::warn!(%task, error = %e2, "could not mark the task failed");
            }
        }
        result.map(drop)
    }

    /// Launch a new worker generation on the task's worktree. The caller
    /// holds the task's lock.
    async fn launch(
        &self,
        project: &ProjectId,
        task: &TaskId,
        cause: Cause,
        note: Option<&str>,
        over: Option<&Relaunch>,
    ) -> Result<Generation> {
        let w = self.worker(project, task)?;
        let a = &w.assignment;
        let harness = over
            .and_then(|o| o.harness.clone())
            .or_else(|| w.current.as_ref().map(|g| g.harness.clone()))
            .unwrap_or_else(|| a.harness.clone());
        let model = over
            .and_then(|o| o.model.clone())
            .or_else(|| w.current.as_ref().and_then(|g| g.model.clone()))
            .or_else(|| a.model.clone());
        let effort = over
            .and_then(|o| o.effort.clone())
            .or_else(|| w.current.as_ref().and_then(|g| g.effort.clone()))
            .or_else(|| a.effort.clone());
        let env = over
            .and_then(|o| o.env.clone())
            .unwrap_or_else(|| a.env.clone());
        let manifest = self
            .manifests
            .resolve(&harness)
            .ok_or_else(|| CoreError::NotFound(format!("harness {harness}")))?
            .clone();
        let worktree = w
            .worktree
            .clone()
            .ok_or_else(|| CoreError::Invalid(format!("task {task} has no worktree")))?;
        assert_isolated(&worktree.path, &a.repo)?;

        let generation = uuid::Uuid::new_v4().simple().to_string();
        let dir = self
            .config
            .state_dir
            .join(path_safe(project.as_str()))
            .join(path_safe(task.as_str()))
            .join(&generation);
        std::fs::create_dir_all(&dir).map_err(|e| io(&dir, e))?;
        let steers: Vec<_> = w.undelivered().cloned().collect();
        let plan = launch::plan(&launch::Inputs {
            manifest: &manifest,
            project,
            task,
            generation: &generation,
            dir: &dir,
            worktree: &worktree.path,
            brief: &a.brief,
            model: model.as_deref(),
            effort: effort.as_deref(),
            note,
            steers: &steers,
            extra_env: &env,
            urls: self.config.worker_urls.as_ref(),
        })?;
        let brief_path = dir.join(launch::BRIEF_FILE);
        std::fs::write(&brief_path, &plan.brief).map_err(|e| io(&brief_path, e))?;

        let mut policy = a.policy.clone();
        for p in [&worktree.path, &dir] {
            if !policy.writable.contains(p) {
                policy.writable.push(p.clone());
            }
        }
        let (argv, env) = self
            .isolation
            .wrap(&ProcessSpec {
                argv: plan.argv,
                cwd: worktree.path.clone(),
                env: plan.env,
                mode: a.isolation,
                policy,
            })
            .await?;
        let info = self
            .sessions
            .create(&SessionSpec {
                task: Some(task.clone()),
                name: task.to_string(),
                cwd: worktree.path.clone(),
                argv,
                env,
                size: self.config.size,
            })
            .await?;
        self.record(
            project,
            task,
            SupervisorEvent::Launched {
                generation: generation.clone(),
                session: info.id.clone(),
                backend: self.sessions.name().to_string(),
                cause,
                dir: dir.clone(),
                harness: manifest.id.clone(),
                model,
                effort,
            },
        )
        .await?;
        // A relaunch from an open decision keeps the task waiting on it:
        // the reference machine has no `Started` out of `NeedsDecision`, and
        // the new worker reads the answer when it is steered in.
        if self.state(project, task) != TaskState::NeedsDecision {
            self.transition(
                project,
                task,
                TaskEvent::Started {
                    generation: generation.clone(),
                },
            )
            .await?;
        }
        if plan.type_brief {
            self.wait_ready(&info.id, &manifest.turn_signals.idle_patterns)
                .await;
            self.sessions
                .input(&info.id, &launch::paste(&plan.brief))
                .await?;
        }
        // Missed steering went into the brief, so it has been delivered.
        for s in steers {
            self.record(
                project,
                task,
                SupervisorEvent::Delivered {
                    id: s.id,
                    generation: generation.clone(),
                },
            )
            .await?;
        }
        self.watches
            .lock()
            .unwrap()
            .remove(&(project.clone(), task.clone()));
        tracing::info!(%project, %task, %generation, harness = %manifest.id, ?cause, "worker launched");
        self.worker(project, task)?
            .current
            .ok_or_else(|| CoreError::Backend("launched generation not recorded".into()))
    }

    /// Wait until the agent looks ready for input: an idle pattern is on
    /// screen, or the screen has something on it and has stopped changing.
    async fn wait_ready(&self, session: &SessionId, idle_patterns: &[String]) {
        let deadline = Instant::now() + self.config.ready_timeout;
        let mut last: Option<(u64, Instant)> = None;
        while Instant::now() < deadline {
            let Ok(snap) = self.sessions.snapshot(session).await else {
                return;
            };
            let text = String::from_utf8_lossy(&snap.bytes);
            if idle_patterns.iter().any(|p| text.contains(p.as_str())) {
                return;
            }
            let h = hash(&snap.bytes);
            match last {
                Some((prev, since)) if prev == h => {
                    if visible(&text) && since.elapsed() >= self.config.ready_quiet {
                        return;
                    }
                }
                _ => last = Some((h, Instant::now())),
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        tracing::warn!(session = %session.0, "agent not ready in time; typing its brief anyway");
    }

    // ---- steer and control --------------------------------------------

    /// Send `text` to the task's worker. The message is recorded first and
    /// delivered now if the worker is running, otherwise to the next
    /// generation; either way it is never lost. Returns the message id.
    pub async fn steer(&self, project: &ProjectId, task: &TaskId, text: &str) -> Result<String> {
        if text.trim().is_empty() {
            return Err(CoreError::Invalid("message is empty".into()));
        }
        let key = (project.clone(), task.clone());
        let _guard = self.lock(&key).await;
        self.refresh().await?;
        self.worker(project, task)?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        self.record(
            project,
            task,
            SupervisorEvent::Steer {
                id: id.clone(),
                text: text.to_string(),
            },
        )
        .await?;
        self.deliver(project, task).await?;
        Ok(id)
    }

    /// Answer the task's open decision `key` and tell the worker.
    pub async fn answer(
        &self,
        project: &ProjectId,
        task: &TaskId,
        key: &str,
        answer: &str,
    ) -> Result<String> {
        self.refresh().await?;
        let open = self
            .ledger
            .states()
            .get(project, task)
            .is_some_and(|r| r.open_decisions.contains(key));
        if !open {
            return Err(CoreError::NotFound(format!(
                "open decision {key} on {task}"
            )));
        }
        if self.state(project, task) == TaskState::NeedsDecision {
            self.transition(
                project,
                task,
                TaskEvent::DecisionAnswered {
                    key: key.to_string(),
                },
            )
            .await?;
        }
        self.steer(project, task, &format!("Answer to [{key}]: {answer}"))
            .await
    }

    /// Paste every undelivered message into the live session. The caller
    /// holds the task's lock. Returns how many were delivered.
    async fn deliver(&self, project: &ProjectId, task: &TaskId) -> Result<usize> {
        let w = self.worker(project, task)?;
        let Some(g) = w.current.clone().filter(|g| g.exited.is_none()) else {
            return Ok(0);
        };
        let mut n = 0;
        for s in w.undelivered() {
            // The message is already recorded; a session that just died
            // gets it through the next generation's brief instead.
            if let Err(e) = self
                .sessions
                .input(&g.session, &launch::paste(&s.text))
                .await
            {
                tracing::info!(%task, error = %e, "steering message waits for the next worker");
                break;
            }
            self.record(
                project,
                task,
                SupervisorEvent::Delivered {
                    id: s.id.clone(),
                    generation: g.id.clone(),
                },
            )
            .await?;
            n += 1;
        }
        Ok(n)
    }

    /// Interrupt the worker's current turn with its harness's interrupt
    /// keys. The worker keeps running.
    pub async fn interrupt(&self, project: &ProjectId, task: &TaskId) -> Result<()> {
        let key = (project.clone(), task.clone());
        let _guard = self.lock(&key).await;
        self.refresh().await?;
        let g = self.live(project, task)?;
        let manifest = self
            .manifests
            .resolve(&g.harness)
            .ok_or_else(|| CoreError::NotFound(format!("harness {}", g.harness)))?;
        for k in &manifest.keys.interrupt {
            self.sessions
                .input(&g.session, &launch::key_bytes(k)?)
                .await?;
        }
        Ok(())
    }

    fn live(&self, project: &ProjectId, task: &TaskId) -> Result<Generation> {
        self.worker(project, task)?
            .current
            .filter(|g| g.exited.is_none())
            .ok_or_else(|| CoreError::Refused(format!("task {task} has no running worker")))
    }

    /// Stop the task: it is marked cancelled first (so it is not
    /// recovered), the worker is asked to exit, and its session is killed.
    /// The worktree and everything in it are kept.
    pub async fn cancel(&self, project: &ProjectId, task: &TaskId) -> Result<()> {
        let key = (project.clone(), task.clone());
        let _guard = self.lock(&key).await;
        self.refresh().await?;
        let w = self.worker(project, task)?;
        if !matches!(
            self.state(project, task),
            TaskState::Done | TaskState::Failed
        ) {
            self.transition(project, task, TaskEvent::Cancelled).await?;
        }
        if let Some(g) = w.current.filter(|g| g.exited.is_none()) {
            self.stop(project, task, &g).await?;
        }
        Ok(())
    }

    /// Ask the generation's agent to exit, then kill its session, and
    /// record that it ended.
    async fn stop(&self, project: &ProjectId, task: &TaskId, g: &Generation) -> Result<()> {
        if let Some(m) = self.manifests.resolve(&g.harness) {
            let mut bytes = m.keys.exit.clone().into_bytes();
            bytes.push(b'\r');
            if self.sessions.input(&g.session, &bytes).await.is_ok() {
                let deadline = Instant::now() + self.config.exit_grace;
                while Instant::now() < deadline {
                    if !self.alive(&g.session).await {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
        let code = match self.session_info(&g.session).await {
            Some(info) if info.alive => {
                match self.sessions.kill(&g.session).await {
                    Ok(()) | Err(CoreError::NotFound(_)) => {}
                    Err(e) => return Err(e),
                }
                None
            }
            Some(info) => info.exit_code,
            None => None,
        };
        self.record(
            project,
            task,
            SupervisorEvent::Exited {
                generation: g.id.clone(),
                code,
            },
        )
        .await
    }

    async fn session_info(&self, id: &SessionId) -> Option<SessionInfo> {
        self.sessions
            .list()
            .await
            .ok()?
            .into_iter()
            .find(|s| &s.id == id)
    }

    async fn alive(&self, id: &SessionId) -> bool {
        self.session_info(id).await.is_some_and(|s| s.alive)
    }

    /// Replace the task's worker with a new generation in the same
    /// worktree, optionally on another harness, model or effort.
    pub async fn relaunch(
        &self,
        project: &ProjectId,
        task: &TaskId,
        relaunch: Relaunch,
    ) -> Result<Generation> {
        let key = (project.clone(), task.clone());
        let _guard = self.lock(&key).await;
        self.refresh().await?;
        let w = self.worker(project, task)?;
        if w.is_released() {
            return Err(CoreError::Refused(format!(
                "task {task} has given its worktree back"
            )));
        }
        if self.state(project, task) == TaskState::Done {
            return Err(CoreError::Refused(format!("task {task} is done")));
        }
        if let Some(h) = &relaunch.harness {
            if self.manifests.resolve(h).is_none() {
                return Err(CoreError::NotFound(format!("harness {h}")));
            }
        }
        if let Some(g) = w.current.filter(|g| g.exited.is_none()) {
            self.stop(project, task, &g).await?;
        }
        if w.worktree.is_none() {
            return self
                .start(project, task)
                .await
                .and_then(|()| self.live(project, task));
        }
        let note = Some(relaunch.note.as_str()).filter(|n| !n.trim().is_empty());
        self.launch(project, task, Cause::Relaunch, note, Some(&relaunch))
            .await
    }

    /// Mark the task's work landed (its pull request merged, its branch
    /// landed or its report delivered). The worker is stopped; the worktree
    /// waits for [`Supervisor::teardown`].
    pub async fn complete(&self, project: &ProjectId, task: &TaskId) -> Result<()> {
        let key = (project.clone(), task.clone());
        let _guard = self.lock(&key).await;
        self.refresh().await?;
        let w = self.worker(project, task)?;
        self.transition(project, task, TaskEvent::Completed).await?;
        if let Some(g) = w.current.filter(|g| g.exited.is_none()) {
            self.stop(project, task, &g).await?;
        }
        Ok(())
    }

    /// Give a finished task's worktree back. Refused while the task is
    /// still open; a worktree with uncommitted or unpushed work is kept and
    /// reported, never discarded.
    pub async fn teardown(&self, project: &ProjectId, task: &TaskId) -> Result<ReturnOutcome> {
        let key = (project.clone(), task.clone());
        let _guard = self.lock(&key).await;
        self.refresh().await?;
        let w = self.worker(project, task)?;
        let state = self.state(project, task);
        if !matches!(state, TaskState::Done | TaskState::Failed) {
            return Err(CoreError::Refused(format!(
                "task {task} is {}; cancel it or let it finish first",
                state.as_str()
            )));
        }
        if let Some(g) = w.current.clone().filter(|g| g.exited.is_none()) {
            self.stop(project, task, &g).await?;
        }
        if w.is_released() {
            return Ok(ReturnOutcome::Returned);
        }
        let Some(wt) = w.worktree else {
            return Ok(ReturnOutcome::Returned);
        };
        let outcome = self.worktrees.return_worktree(&wt.id).await?;
        self.record(
            project,
            task,
            SupervisorEvent::Released {
                outcome: outcome.clone(),
            },
        )
        .await?;
        self.watches.lock().unwrap().remove(&key);
        Ok(outcome)
    }

    // ---- recovery and supervision ------------------------------------

    /// After a start: kill sessions no task owns any more (left by a crash
    /// between starting a session and recording it, or by a replaced
    /// generation). Sessions of current generations are adopted as they
    /// are; [`Supervisor::tick`] relaunches the ones that are gone.
    pub async fn recover(&self) -> Result<usize> {
        self.refresh().await?;
        let current: HashMap<SessionId, ()> = self
            .fleet
            .all()
            .into_iter()
            .filter_map(|w| w.current)
            .filter(|g| g.backend == self.sessions.name())
            .map(|g| (g.session, ()))
            .collect();
        let mut killed = 0;
        for s in self.sessions.list().await? {
            if s.task.is_none() || current.contains_key(&s.id) || !s.alive {
                continue;
            }
            tracing::warn!(session = %s.id.0, task = ?s.task, "killing a session no worker owns");
            match self.sessions.kill(&s.id).await {
                Ok(()) | Err(CoreError::NotFound(_)) => killed += 1,
                Err(e) => return Err(e),
            }
        }
        Ok(killed)
    }

    /// One supervision pass. Safe to call at any time and after a crash.
    ///
    /// 1. Read each live worker's status file into the log.
    /// 2. Turn new worker messages into task transitions.
    /// 3. Finish spawns a crash interrupted.
    /// 4. Notice ended sessions: relaunch an active task's worker (up to
    ///    [`Config::max_recoveries`] times in a row), else fail the task.
    /// 5. Deliver steering messages that are still waiting.
    /// 6. Report workers that have shown no activity for
    ///    [`Config::stale_after`].
    pub async fn tick(&self) -> Result<Tick> {
        let mut t = Tick::default();
        self.refresh().await?;
        let sessions: HashMap<SessionId, SessionInfo> = self
            .sessions
            .list()
            .await?
            .into_iter()
            .map(|s| (s.id.clone(), s))
            .collect();
        self.poll_status_files().await;
        t.transitions = self.handle_messages().await?;

        for w in self.fleet.all() {
            let key = (w.project.clone(), w.task.clone());
            // A task someone is changing right now is looked at next time.
            let Ok(_guard) = self.task_lock(&key).try_lock_owned() else {
                continue;
            };
            if let Err(e) = self.supervise(&key, &sessions, &mut t).await {
                tracing::warn!(project = %key.0, task = %key.1, error = %e, "supervising task");
            }
        }
        Ok(t)
    }

    /// Run [`Supervisor::tick`] every `every` until the task is aborted.
    pub async fn run(self: Arc<Self>, every: Duration) {
        let mut timer = tokio::time::interval(every);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            timer.tick().await;
            match self.tick().await {
                Ok(t) if t != Tick::default() => tracing::debug!(?t, "supervision pass"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "supervision pass failed"),
            }
        }
    }

    async fn poll_status_files(&self) {
        let workers: Vec<(Key, Generation)> = self
            .fleet
            .all()
            .into_iter()
            .filter_map(|w| Some(((w.project, w.task), w.current?)))
            .collect();
        for (key, g) in workers {
            let mut status = {
                let mut watches = self.watches.lock().unwrap();
                let watch = watches
                    .entry(key.clone())
                    .and_modify(|w| {
                        if w.generation != g.id {
                            *w = Watch::new(&key.1, &g);
                        }
                    })
                    .or_insert_with(|| Watch::new(&key.1, &g));
                watch.status.clone()
            };
            let before = status.offset();
            match status.poll(&self.recorder).await {
                Ok(_) => {}
                Err(e) => tracing::warn!(task = %key.1, error = %e, "reading status file"),
            }
            if let Some(w) = self.watches.lock().unwrap().get_mut(&key) {
                if w.generation == g.id {
                    if status.offset() != before {
                        w.active_at = Instant::now();
                        w.stale_reported = false;
                    }
                    w.status = status;
                }
            }
        }
    }

    /// Turn worker messages logged since the last pass into task
    /// transitions, then record how far it got.
    async fn handle_messages(&self) -> Result<usize> {
        let mut at = self.handling.lock().await;
        *at = (*at).max(self.fleet.handled_through());
        let mut n = 0;
        let mut through = None;
        loop {
            let events = self.log.read(*at, BATCH).await?;
            let Some(last) = events.last().map(|e| e.seq) else {
                break;
            };
            for e in events.iter().filter(|e| is_worker_message(e)) {
                let Ok(env) = e.decode::<WorkerEnvelope>() else {
                    continue;
                };
                n += self.handle_message(&e.project, &env).await;
                through = Some(e.seq);
            }
            *at = last;
        }
        if let Some(through) = through {
            let event = NewEvent::new(
                self.config.host.clone(),
                ProjectId::engine(),
                None,
                SupervisorEvent::Handled { through }.kind(),
                serde_json::to_value(SupervisorEvent::Handled { through })
                    .map_err(|e| CoreError::Invalid(e.to_string()))?,
            );
            self.log.append(event).await?;
            replay(self.log.as_ref(), &self.fleet, BATCH).await?;
        }
        Ok(n)
    }

    /// One worker message to transitions; returns how many were recorded.
    /// A message from a replaced generation changes nothing. A transition
    /// the machine refuses (a replayed message after a crash, a `done`
    /// while a decision is open) is logged and skipped.
    async fn handle_message(&self, project: &ProjectId, env: &WorkerEnvelope) -> usize {
        let key = (project.clone(), env.task.clone());
        let Some(w) = self.fleet.get(project, &env.task) else {
            return 0;
        };
        if w.current.as_ref().map(|g| g.id.as_str()) != Some(env.generation.as_str()) {
            return 0;
        }
        if let Some(watch) = self.watches.lock().unwrap().get_mut(&key) {
            watch.active_at = Instant::now();
            watch.stale_reported = false;
        }
        let Some(record) = self.ledger.states().get(project, &env.task) else {
            return 0;
        };
        let paused = matches!(record.state, TaskState::Blocked | TaskState::Paused);
        let mut events = Vec::new();
        match &env.message {
            WorkerMessage::Report { state, note } => match state.to_ascii_lowercase().as_str() {
                "blocked" => events.push(TaskEvent::Blocked {
                    reason: note.clone(),
                }),
                "paused" => events.push(TaskEvent::Paused {
                    reason: note.clone(),
                }),
                "failed" => events.push(TaskEvent::Failed {
                    reason: note.clone(),
                }),
                _ if paused => events.push(TaskEvent::Resumed),
                _ => {}
            },
            WorkerMessage::Ask { key, question } => {
                if !record.open_decisions.contains(key) {
                    if paused {
                        events.push(TaskEvent::Resumed);
                    }
                    events.push(TaskEvent::DecisionNeeded {
                        key: key.clone(),
                        question: question.clone(),
                    });
                }
            }
            WorkerMessage::Done { pull_request, .. } => {
                if paused {
                    events.push(TaskEvent::Resumed);
                }
                events.push(match pull_request {
                    Some(pr) => TaskEvent::InReview {
                        pull_request: Some(pr.clone()),
                    },
                    None => TaskEvent::Completed,
                });
            }
            WorkerMessage::Learned { .. } | WorkerMessage::Signal { .. } => {}
        }
        let mut n = 0;
        for e in events {
            match self.transition(project, &env.task, e).await {
                Ok(()) => n += 1,
                Err(err) => {
                    tracing::info!(task = %env.task, error = %err, "worker message changes nothing");
                    break;
                }
            }
        }
        n
    }

    async fn supervise(
        &self,
        key: &Key,
        sessions: &HashMap<SessionId, SessionInfo>,
        t: &mut Tick,
    ) -> Result<()> {
        let (project, task) = key;
        let w = self.worker(project, task)?;
        let state = self.state(project, task);
        let active = matches!(
            state,
            TaskState::Running | TaskState::NeedsDecision | TaskState::Blocked | TaskState::Paused
        );

        let Some(g) = w.current.clone() else {
            // Assigned but never launched: a crash interrupted the spawn.
            if state == TaskState::Queued && !w.is_released() {
                self.start(project, task).await?;
                t.resumed_spawns += 1;
            }
            return Ok(());
        };
        if state == TaskState::Queued && g.exited.is_none() {
            // Launched, but the crash came before `Started` was recorded.
            self.transition(
                project,
                task,
                TaskEvent::Started {
                    generation: g.id.clone(),
                },
            )
            .await?;
        }

        if g.exited.is_none() {
            let info = (g.backend == self.sessions.name())
                .then(|| sessions.get(&g.session))
                .flatten();
            if !info.is_some_and(|i| i.alive) {
                let code = info.and_then(|i| i.exit_code);
                self.record(
                    project,
                    task,
                    SupervisorEvent::Exited {
                        generation: g.id.clone(),
                        code,
                    },
                )
                .await?;
                t.exited += 1;
                if active {
                    self.recover_worker(project, task, &w, code, t).await?;
                }
                return Ok(());
            }
            t.delivered += self.deliver(project, task).await?;
            if state == TaskState::Running {
                t.stale += self.check_stale(key, &g).await?;
            }
        } else if active && !w.is_released() {
            // Ended earlier and not yet replaced (a crash between noticing
            // and relaunching).
            self.recover_worker(project, task, &w, g.exited.flatten(), t)
                .await?;
        }
        Ok(())
    }

    async fn recover_worker(
        &self,
        project: &ProjectId,
        task: &TaskId,
        w: &Worker,
        code: Option<i32>,
        t: &mut Tick,
    ) -> Result<()> {
        let how = code.map_or("ended".to_string(), |c| format!("ended with exit code {c}"));
        if w.recoveries >= self.config.max_recoveries {
            let reason = format!(
                "the worker's session {how} and was restarted {} times already",
                w.recoveries
            );
            return self
                .transition(project, task, TaskEvent::Failed { reason })
                .await;
        }
        let note = format!(
            "Your previous session {how}. This is a new session in the same worktree, which \
             still has all of your work: check `git status` and the log, then carry on."
        );
        self.launch(project, task, Cause::Recover, Some(&note), None)
            .await?;
        t.recovered += 1;
        Ok(())
    }

    async fn check_stale(&self, key: &Key, g: &Generation) -> Result<usize> {
        let screen = match self.sessions.snapshot(&g.session).await {
            Ok(s) => hash(&s.bytes),
            Err(_) => return Ok(0),
        };
        let idle = {
            let mut watches = self.watches.lock().unwrap();
            let Some(w) = watches.get_mut(key).filter(|w| w.generation == g.id) else {
                return Ok(0);
            };
            if w.screen != screen {
                w.screen = screen;
                w.active_at = Instant::now();
                w.stale_reported = false;
                return Ok(0);
            }
            let idle = w.active_at.elapsed();
            if w.stale_reported || idle < self.config.stale_after {
                return Ok(0);
            }
            w.stale_reported = true;
            idle
        };
        self.record(
            &key.0,
            &key.1,
            SupervisorEvent::Stale {
                generation: g.id.clone(),
                idle_secs: idle.as_secs(),
            },
        )
        .await?;
        Ok(1)
    }
}

impl Watch {
    fn new(task: &TaskId, g: &Generation) -> Self {
        Self {
            generation: g.id.clone(),
            status: StatusFile::new(g.status_file(), WorkerIdentity::new(task.clone(), &g.id)),
            screen: 0,
            active_at: Instant::now(),
            stale_reported: false,
        }
    }
}

/// The isolation assertion: a worker only ever starts in a linked worktree,
/// never in the repository's primary checkout or anywhere inside it.
pub fn assert_isolated(worktree: &Path, repo: &Path) -> Result<()> {
    let refuse = |why: &str| {
        Err(CoreError::Refused(format!(
            "won't start a worker in {}: {why}",
            worktree.display()
        )))
    };
    let Ok(wt) = worktree.canonicalize() else {
        return refuse("it does not exist");
    };
    if let Ok(repo) = repo.canonicalize() {
        if wt == repo {
            return refuse("it is the primary checkout");
        }
        if wt.starts_with(&repo) {
            return refuse("it is inside the primary checkout");
        }
    }
    match std::fs::symlink_metadata(wt.join(".git")) {
        Ok(m) if m.is_file() => Ok(()),
        Ok(_) => refuse("it is a primary checkout, not a linked worktree"),
        Err(_) => refuse("it is not a git worktree"),
    }
}

/// `s` with anything but letters, digits, `-`, `_` and `.` replaced, so an
/// id is one path component.
fn path_safe(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() || out.chars().all(|c| c == '.') {
        "_".into()
    } else {
        out
    }
}

fn hash(bytes: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// Whether a snapshot shows anything but escape sequences and blanks.
fn visible(text: &str) -> bool {
    let mut in_escape = false;
    for c in text.chars() {
        match c {
            '\x1b' => in_escape = true,
            c if in_escape => {
                if c.is_ascii_alphabetic() || c == '~' {
                    in_escape = false;
                }
            }
            c if !c.is_whitespace() && !c.is_control() => return true,
            _ => {}
        }
    }
    false
}

fn io(path: &Path, e: std::io::Error) -> CoreError {
    CoreError::Backend(format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_safe_ids() {
        assert_eq!(path_safe("t-1_a.b"), "t-1_a.b");
        assert_eq!(path_safe("../x"), ".._x");
        assert_eq!(path_safe(".."), "_");
        assert_eq!(path_safe(""), "_");
    }

    #[test]
    fn visible_ignores_escapes() {
        assert!(!visible("\x1bc\x1b[?1049h\x1b[H  \r\n"));
        assert!(visible("\x1b[1m>\x1b[0m"));
    }

    #[test]
    fn isolation_assertion() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        assert!(assert_isolated(&repo, &repo).is_err());
        assert!(assert_isolated(&repo.join("sub"), &repo).is_err());
        assert!(assert_isolated(&wt, &repo).is_err(), "no .git");
        std::fs::write(wt.join(".git"), "gitdir: x").unwrap();
        assert!(assert_isolated(&wt, &repo).is_ok());
        let nested = repo.join("inner");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join(".git"), "gitdir: x").unwrap();
        assert!(assert_isolated(&nested, &repo).is_err());
    }
}
