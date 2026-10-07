//! Pulls engine state through the adapter and projects it into the store.
//!
//! Each refresh of a workspace reads its snapshot (the board), tails every
//! task's status log (task activity) and reads its holds (decisions). Refreshes
//! run on a timer and, when the adapter names directories to watch, as soon as
//! a task file changes there, so the board moves while a worker reports.
//! Each refresh also hands the engine the verification gates the Project
//! repo's `project.yaml` declares and the dispatch profiles its
//! `dispatch.yaml` declares, whenever either declaration changes, turns
//! what finished tasks learned into memory proposals, and records why each
//! newly spawned worker got its agent ([`crate::dispatch`]). A worker whose
//! session log ends at a rate limit is handed to [`Failover`].

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use quark_systems::{DispatchTrigger, MemoryEvidence};
use quark_transcript::{RateLimit, SessionFormat, SessionRoots};
use tokio::sync::mpsc;

use crate::sessions::{Sessions, TaskTarget};

use crate::dispatch::{self, Resolved};
use crate::engine::{EngineAdapter, EngineError, EngineTask, WorkspaceRef};
use crate::failover::Failover;

use crate::memory;
use crate::store::{NewProposal, Store, TranscriptSource};
use crate::transcripts::TranscriptTap;

/// How long to wait after a file change for the rest of a burst (an atomic
/// rename, several status lines) before refreshing.
const DEBOUNCE: Duration = Duration::from_millis(250);

pub struct Projector {
    store: Arc<Store>,
    engine: Arc<dyn EngineAdapter>,
    transcripts: Arc<TranscriptTap>,
    sessions: Option<Sessions>,
    /// Moves rate-limited workers to another account, when set.
    failover: Option<Arc<Failover>>,
    /// Command-center workspace whose secondmates are the coordinators.
    command: Option<PathBuf>,
    /// Coordinator targets last handed to the session layer, by Project id.
    coordinators: Mutex<HashMap<String, Option<String>>>,
    /// The Project repo declaration last applied as gate config, by Project id.
    gates: Mutex<HashMap<String, String>>,
    /// The `dispatch.yaml` blob and user-level settings last applied as
    /// dispatch profiles, by Project id.
    dispatch: Mutex<HashMap<String, (String, String)>>,
    /// The user-level settings file whose `classifier` block is the default
    /// for every Project.
    user_config: Option<PathBuf>,
    /// Re-runs each dispatch resolution natively, in shadow, when set.
    dispatch_shadow: Option<Arc<crate::native_dispatch::DispatchShadow>>,
}

impl Projector {
    pub fn new(store: Arc<Store>, engine: Arc<dyn EngineAdapter>) -> Self {
        let transcripts = Arc::new(TranscriptTap::new(store.clone(), SessionRoots::from_env()));
        Self {
            store,
            engine,
            transcripts,
            sessions: None,
            failover: None,
            command: None,
            coordinators: Mutex::default(),
            gates: Mutex::default(),
            dispatch: Mutex::default(),
            user_config: None,
            dispatch_shadow: None,
        }
    }

    /// Takes each Project's default classifier from the settings file at
    /// `path` (`~/.quark/config.yaml`).
    pub fn with_user_config(mut self, path: PathBuf) -> Self {
        self.user_config = Some(path);
        self
    }

    /// Reads harness session logs from `roots` instead of the current user's
    /// default harness directories.
    pub fn with_session_roots(mut self, roots: SessionRoots) -> Self {
        self.transcripts = Arc::new(TranscriptTap::new(self.store.clone(), roots));
        self
    }

    /// Also maps each refreshed workspace's tmux windows to its tasks.
    pub fn with_sessions(mut self, sessions: Sessions) -> Self {
        self.sessions = Some(sessions);
        self
    }

    /// Also compares each dispatch resolution with the native resolver's.
    pub fn with_dispatch_shadow(
        mut self,
        shadow: Arc<crate::native_dispatch::DispatchShadow>,
    ) -> Self {
        self.dispatch_shadow = Some(shadow);
        self
    }

    /// Also fails workers over to another pool account on rate limits.
    pub fn with_failover(mut self, failover: Arc<Failover>) -> Self {
        self.failover = Some(failover);
        self
    }

    /// Also maps each Project's coordinator window, read from the
    /// command-center workspace at `command`.
    pub fn with_command(mut self, command: impl Into<PathBuf>) -> Self {
        self.command = Some(command.into());
        self
    }

    /// Refreshes every Project that has a workspace attached and returns those
    /// workspaces. Adapter failures are recorded and logged per Project, never
    /// retried silently.
    pub async fn refresh_all(&self) -> anyhow::Result<Vec<WorkspaceRef>> {
        let store = self.store.clone();
        let projects = tokio::task::spawn_blocking(move || store.list_projects()).await??;
        let ids: Vec<_> = projects
            .iter()
            .filter(|p| p.workspace_path.is_some())
            .map(|p| p.id.clone())
            .collect();
        self.sync_coordinators(&ids).await;
        let mut workspaces = Vec::new();
        for project in projects {
            let Some(path) = project.workspace_path else {
                continue;
            };
            let ws = WorkspaceRef {
                project_id: project.id.clone(),
                root: PathBuf::from(path),
            };
            self.refresh(&ws).await;
            if let Some(repo) = &project.project_repo_path {
                self.sync_gates(&ws, PathBuf::from(repo)).await;
                self.sync_dispatch(&ws, PathBuf::from(repo)).await;
            }
            workspaces.push(ws);
        }
        Ok(workspaces)
    }

    /// Refreshes one workspace: board, task activity, then decisions.
    pub async fn refresh(&self, ws: &WorkspaceRef) {
        let started = Instant::now();
        let snapshot = self.engine.snapshot(ws).await;
        self.record(ws, "snapshot", started, snapshot.as_ref().err())
            .await;
        let mut tasks = Vec::new();
        if let Ok(snapshot) = snapshot {
            tasks = snapshot.tasks.clone();
            let terminals: Vec<(String, String)> = snapshot
                .tasks
                .iter()
                .filter_map(|t| Some((t.id.clone(), t.terminal.clone()?)))
                .collect();
            let store = self.store.clone();
            let project_id = ws.project_id.clone();
            let res =
                tokio::task::spawn_blocking(move || store.apply_snapshot(&project_id, &snapshot))
                    .await;
            log_apply(ws, "snapshot", res);
            self.sync_sessions(ws, terminals).await;
            let models = self.record_dispatches(ws, &tasks).await;
            self.sync_agents(ws, &tasks, models).await;
        }
        self.tap_transcripts(ws, tasks).await;

        self.tail_statuses(ws).await;
        self.propose_learnings(ws).await;

        let started = Instant::now();
        // Taken before the read, so the store can tell this read from one
        // that saw the engine before an answer landed.
        let observed_at = crate::now_rfc3339();
        let holds = self.engine.holds(ws).await;
        self.record(ws, "holds", started, holds.as_ref().err())
            .await;
        if let Ok(holds) = holds {
            let store = self.store.clone();
            let project_id = ws.project_id.clone();
            let res = tokio::task::spawn_blocking(move || {
                store.apply_holds(&project_id, &holds, &observed_at)
            })
            .await;
            log_apply(ws, "holds", res);
        }
    }

    /// Compile the Project repo's gate declaration and hand it to the engine
    /// when it changed since the last success. A failure is recorded and
    /// retried next tick.
    pub async fn sync_gates(&self, ws: &WorkspaceRef, repo: PathBuf) {
        let started = Instant::now();
        let declared = {
            let repo = repo.clone();
            tokio::task::spawn_blocking(move || crate::gates::read_declared(&repo)).await
        };
        let declared = match declared {
            Ok(Ok(Some(d))) => d,
            Ok(Ok(None)) => return,
            Ok(Err(e)) => {
                let err = EngineError::Command(e);
                self.record(ws, "gates", started, Some(&err)).await;
                return;
            }
            Err(e) => {
                tracing::error!(error = %e, "gates read task panicked");
                return;
            }
        };
        if self.gates.lock().unwrap().get(&ws.project_id) == Some(&declared.blob) {
            return;
        }
        let config =
            crate::gates::compile(&declared.project_yaml, &repo, &declared.holdout_sources)
                .and_then(|c| serde_json::to_string(&c).map_err(|e| e.to_string()));
        // A declaration that does not compile is recorded once and waits for
        // the next change; an engine failure is retried.
        let (res, done) = match config {
            Ok(json) => {
                let res = self.engine.set_gates(ws, &json).await;
                let ok = res.is_ok();
                (res, ok)
            }
            Err(e) => (Err(EngineError::Parse(e)), true),
        };
        self.record(ws, "gates", started, res.as_ref().err()).await;
        if done {
            self.gates
                .lock()
                .unwrap()
                .insert(ws.project_id.clone(), declared.blob);
        }
    }

    /// Compile the Project repo's `dispatch.yaml` and hand it to the engine
    /// when it, or the user-level default classifier, changed since it was
    /// last applied. A file that does not compile,
    /// or that the engine refuses as invalid, is recorded once and waits for
    /// the next change, leaving the last good config in place; any other
    /// engine failure is recorded and retried next tick.
    pub async fn sync_dispatch(&self, ws: &WorkspaceRef, repo: PathBuf) {
        let started = Instant::now();
        let declared =
            tokio::task::spawn_blocking(move || crate::crew_dispatch::read_declared(&repo)).await;
        let declared = match declared {
            Ok(Ok(Some(d))) => d,
            Ok(Ok(None)) => return,
            Ok(Err(e)) => {
                let err = EngineError::Command(e);
                self.record(ws, "dispatch", started, Some(&err)).await;
                return;
            }
            Err(e) => {
                tracing::error!(error = %e, "dispatch read task panicked");
                return;
            }
        };
        let user = match &self.user_config {
            Some(path) => crate::classifier::read_user_file(path),
            None => Ok(String::new()),
        };
        // An unreadable settings file is applied as its error, once.
        let applied = (
            declared.blob,
            user.clone().unwrap_or_else(|e| format!("\0{e}")),
        );
        if self.dispatch.lock().unwrap().get(&ws.project_id) == Some(&applied) {
            return;
        }
        let res = match user
            .and_then(|text| crate::classifier::user_default(&text))
            .and_then(|user| crate::crew_dispatch::compile(&declared.dispatch_yaml, user.as_ref()))
            .and_then(|c| serde_json::to_string(&c).map_err(|e| e.to_string()))
        {
            Ok(json) => self.engine.set_crew_dispatch(ws, &json).await,
            Err(e) => Err(EngineError::Invalid(e)),
        };
        self.record(ws, "dispatch", started, res.as_ref().err())
            .await;
        if matches!(res, Ok(()) | Err(EngineError::Invalid(_))) {
            self.dispatch
                .lock()
                .unwrap()
                .insert(ws.project_id.clone(), applied);
        }
    }

    /// Records each worker spawn not yet recorded: a task's first spawn with
    /// the engine's dispatch resolution of its brief, when the spawn is fresh,
    /// and each later relaunch as such. One adapter-call record covers the
    /// pass and carries the first failure. Returns the model each spawned
    /// task's worker was started with.
    async fn record_dispatches(
        &self,
        ws: &WorkspaceRef,
        tasks: &[EngineTask],
    ) -> HashMap<String, Option<String>> {
        let mut models = HashMap::new();
        let spawned: Vec<String> = tasks
            .iter()
            .filter(|t| t.harness.is_some())
            .map(|t| t.id.clone())
            .collect();
        if spawned.is_empty() {
            return models;
        }
        let store = self.store.clone();
        let project_id = ws.project_id.clone();
        let known = tokio::task::spawn_blocking(move || {
            Ok((
                store.task_ids_by_engine(&project_id)?,
                store.dispatch_generations(&project_id)?,
            ))
        })
        .await;
        let (ids, recorded) = match known {
            Ok(Ok(k)) => k,
            res => {
                log_apply(ws, "dispatch_record", res.map(|r| r.map(|_| ())));
                return models;
            }
        };

        let started = Instant::now();
        let mut first_err = None;
        for engine_id in spawned {
            let Some(task_id) = ids.get(&engine_id) else {
                continue;
            };
            let spawn = match self.engine.spawn(ws, &engine_id).await {
                Ok(Some(s)) => s,
                Ok(None) => continue,
                Err(e) => {
                    first_err.get_or_insert(e);
                    continue;
                }
            };
            models.insert(engine_id.clone(), spawn.model.clone());
            let seen = recorded.get(task_id).map(Vec::as_slice).unwrap_or_default();
            if seen.contains(&spawn.generation) {
                continue;
            }
            let now = time::OffsetDateTime::now_utc().unix_timestamp();
            let (trigger, resolved) = if !seen.is_empty() {
                (
                    DispatchTrigger::Relaunch,
                    Resolved::NotRun("relaunched in the same worktree".into()),
                )
            } else if !dispatch::fresh(&spawn, now) {
                (
                    DispatchTrigger::Spawn,
                    Resolved::NotRun(
                        "The worker started before Quark saw it, so the dispatch resolution was not run"
                            .into(),
                    ),
                )
            } else {
                let project = spawn.project.as_deref();
                let resolved = match self.engine.resolve_dispatch(ws, &engine_id, project).await {
                    Ok(Some(r)) => {
                        if let Some(shadow) = &self.dispatch_shadow {
                            shadow.compare(ws, Some(&engine_id), &r).await;
                        }
                        Resolved::Ran(Box::new(r))
                    }
                    Ok(None) => Resolved::NotRun(
                        "The engine reported no dispatch resolution for this task".into(),
                    ),
                    Err(e) => Resolved::Failed(e.to_string()),
                };
                (DispatchTrigger::Spawn, resolved)
            };
            let record = dispatch::build(task_id, &ws.project_id, &spawn, trigger, resolved);
            let store = self.store.clone();
            let res = tokio::task::spawn_blocking(move || {
                store.record_dispatch(&record, &spawn.generation).map(drop)
            })
            .await;
            log_apply(ws, "dispatch_record", res);
        }
        self.record(ws, "dispatch_record", started, first_err.as_ref())
            .await;
        models
    }

    /// Records each task's configured model (from `models`) and the branch
    /// its working copy has out. A task whose spawn could not be read keeps
    /// the model it had.
    async fn sync_agents(
        &self,
        ws: &WorkspaceRef,
        tasks: &[EngineTask],
        models: HashMap<String, Option<String>>,
    ) {
        let store = self.store.clone();
        let project_id = ws.project_id.clone();
        let tasks: Vec<(String, Option<PathBuf>)> = tasks
            .iter()
            .filter(|t| models.contains_key(&t.id))
            .map(|t| (t.id.clone(), t.worktree.clone()))
            .collect();
        let res = tokio::task::spawn_blocking(move || {
            for (id, worktree) in tasks {
                let branch = worktree
                    .as_deref()
                    .and_then(crate::worktree::current_branch);
                let model = models.get(&id).cloned().flatten();
                store.set_task_agent(&project_id, &id, model.as_deref(), branch.as_deref())?;
            }
            Ok::<_, crate::store::StoreError>(())
        })
        .await;
        log_apply(ws, "task_agents", res);
    }

    /// Projects new coordinator and worker session-log entries. A log that
    /// cannot be read is logged and retried next tick. A worker whose log
    /// now ends at a rate limit is failed over.
    async fn tap_transcripts(&self, ws: &WorkspaceRef, tasks: Vec<EngineTask>) {
        let tap = self.transcripts.clone();
        let store = self.store.clone();
        let project = ws.clone();
        let res = tokio::task::spawn_blocking(move || {
            let ws = project;
            let mut limited: Vec<(String, RateLimit)> = Vec::new();
            let project_id = &ws.project_id;
            let coordinator = TranscriptSource::Coordinator {
                project_id: project_id.clone(),
            };
            if let Err(e) = tap.poll(project_id, coordinator, &ws.root, &SessionFormat::ALL) {
                tracing::warn!(project = %project_id, error = %e, "coordinator transcript");
            }
            for t in tasks {
                let (Some(worktree), Some(format)) = (
                    t.worktree.as_deref(),
                    t.harness.as_deref().and_then(SessionFormat::for_harness),
                ) else {
                    continue;
                };
                let task_id = match store.task_id_for_engine(project_id, &t.id) {
                    Ok(Some(id)) => id,
                    Ok(None) => continue,
                    Err(e) => {
                        tracing::error!(project = %project_id, error = %e, "task lookup");
                        continue;
                    }
                };
                let source = TranscriptSource::Task {
                    task_id: task_id.clone(),
                };
                match tap.poll(project_id, source, worktree, &[format]) {
                    Ok(Some(limit)) => limited.push((task_id, limit)),
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!(project = %project_id, task = %t.id, error = %e, "worker transcript")
                    }
                }
            }
            limited
        })
        .await;
        let limited = match res {
            Ok(limited) => limited,
            Err(e) => {
                tracing::error!(error = %e, "transcript task panicked");
                return;
            }
        };
        let Some(failover) = &self.failover else {
            return;
        };
        for (task_id, limit) in limited {
            match failover.rate_limited(ws, &task_id, &limit).await {
                Ok(Some(f)) => {
                    tracing::info!(task = %task_id, from = %f.from_account_id, to = ?f.to_account_id, outcome = ?f.outcome, signal = %f.signal, "rate limit")
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::error!(task = %task_id, error = %e, "rate-limit failover failed")
                }
            }
        }
    }

    /// Hands the workspace's task window targets to the session layer.
    async fn sync_sessions(&self, ws: &WorkspaceRef, terminals: Vec<(String, String)>) {
        let Some(sessions) = &self.sessions else {
            return;
        };
        let store = self.store.clone();
        let project_id = ws.project_id.clone();
        let ids = match tokio::task::spawn_blocking(move || store.task_ids_by_engine(&project_id))
            .await
        {
            Ok(Ok(ids)) => ids,
            Ok(Err(e)) => {
                tracing::error!(project = %ws.project_id, error = %e, "reading task ids failed");
                return;
            }
            Err(e) => {
                tracing::error!(project = %ws.project_id, error = %e, "task id read panicked");
                return;
            }
        };
        let targets = terminals
            .into_iter()
            .filter_map(|(engine_id, target)| {
                Some(TaskTarget {
                    task_id: ids.get(&engine_id)?.clone(),
                    target,
                })
            })
            .collect();
        if let Err(e) = sessions.sync(&ws.project_id, targets).await {
            tracing::debug!(project = %ws.project_id, error = %e, "terminal sync skipped");
        }
    }

    /// Maps each Project's coordinator window from the command center's
    /// records, so the mapping survives daemon restarts. Only changes reach
    /// the session layer; a failed read leaves the last mapping in place.
    async fn sync_coordinators(&self, project_ids: &[String]) {
        let (Some(sessions), Some(command)) = (&self.sessions, &self.command) else {
            return;
        };
        if sessions.server().is_err() {
            return;
        }
        let mut found = match self.engine.coordinator_terminals(command).await {
            Ok(found) => found,
            Err(e) => {
                tracing::warn!(error = %e, "reading coordinator windows failed");
                return;
            }
        };
        for id in project_ids {
            let target = found.remove(id);
            let changed = {
                let mut last = self.coordinators.lock().unwrap();
                if last.get(id) == Some(&target) {
                    false
                } else {
                    last.insert(id.clone(), target.clone());
                    true
                }
            };
            if !changed {
                continue;
            }
            if let Err(e) = sessions.set_coordinator(id, target).await {
                tracing::debug!(project = %id, error = %e, "coordinator mapping skipped");
                self.coordinators.lock().unwrap().remove(id);
            }
        }
    }

    /// Reads every task's status log from its stored cursor. One adapter-call
    /// record covers the pass and carries the first failure.
    async fn tail_statuses(&self, ws: &WorkspaceRef) {
        let store = self.store.clone();
        let project_id = ws.project_id.clone();
        let targets =
            match tokio::task::spawn_blocking(move || store.tail_targets(&project_id)).await {
                Ok(Ok(t)) => t,
                res => {
                    log_apply(ws, "status", res.map(|r| r.map(|_| ())));
                    return;
                }
            };

        let started = Instant::now();
        let mut first_err = None;
        for target in targets {
            match self
                .engine
                .status_tail(ws, &target.engine_id, target.offset)
                .await
            {
                Ok(tail) if tail.entries.is_empty() && tail.next_offset == target.offset => {}
                Ok(tail) => {
                    let store = self.store.clone();
                    let res = tokio::task::spawn_blocking(move || {
                        store.apply_status(
                            &target.task_id,
                            target.offset,
                            &tail.entries,
                            tail.next_offset,
                        )
                    })
                    .await;
                    log_apply(ws, "status", res);
                }
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        self.record(ws, "status", started, first_err.as_ref()).await;
    }

    /// Turns `learned` lines of finished tasks into memory proposals. Files
    /// the line does not name are read from the task's working tree, when it
    /// has one.
    async fn propose_learnings(&self, ws: &WorkspaceRef) {
        let store = self.store.clone();
        let project_id = ws.project_id.clone();
        let pending =
            match tokio::task::spawn_blocking(move || store.pending_learnings(&project_id)).await {
                Ok(Ok(p)) => p,
                res => {
                    log_apply(ws, "memory", res.map(|r| r.map(|_| ())));
                    return;
                }
            };
        for l in pending {
            let learning = memory::parse_learning(&l.note, &l.raw);
            let mut files = learning.files;
            if files.is_empty() {
                if let Some(tree) = &l.worktree {
                    match crate::worktree::changes(std::path::Path::new(tree)).await {
                        Ok(c) => files.extend(
                            c.files
                                .into_iter()
                                .take(memory::MAX_EVIDENCE_FILES)
                                .map(|f| f.path),
                        ),
                        Err(e) => {
                            tracing::debug!(task = %l.task_id, error = %e, "no changed files for a learning")
                        }
                    }
                }
            }
            let proposal = NewProposal {
                task_event_id: Some(l.task_event_id),
                task_id: Some(l.task_id.clone()),
                text: learning.text,
                evidence: MemoryEvidence {
                    task_id: Some(l.task_id),
                    task_title: Some(l.task_title),
                    pull_request_url: l.pull_request_url,
                    files,
                },
                source: learning.source,
            };
            let store = self.store.clone();
            let project_id = ws.project_id.clone();
            let res = tokio::task::spawn_blocking(move || {
                store.propose_memory(&project_id, proposal).map(drop)
            })
            .await;
            log_apply(ws, "memory", res);
        }
    }

    async fn record(
        &self,
        ws: &WorkspaceRef,
        operation: &'static str,
        started: Instant,
        err: Option<&EngineError>,
    ) {
        let elapsed = started.elapsed().as_millis() as u64;
        let detail = err.map(|e| e.to_string());
        if let Some(d) = &detail {
            tracing::warn!(project = %ws.project_id, operation, error = %d, "engine read failed");
        }
        let store = self.store.clone();
        let project_id = ws.project_id.clone();
        let res = tokio::task::spawn_blocking(move || {
            store.record_adapter_call(
                Some(&project_id),
                operation,
                detail.is_none(),
                elapsed,
                detail.as_deref(),
            )
        })
        .await;
        match res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::error!(error = %e, "could not record adapter call"),
            Err(e) => tracing::error!(error = %e, "adapter-call record task panicked"),
        }
    }

    /// Refreshes on start, every `interval`, and shortly after a task file
    /// changes in a watched workspace, until the task is dropped.
    pub async fn run(self, interval: Duration) {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut watches = Watches::new(self.engine.clone(), tx);
        let mut workspaces: HashMap<String, WorkspaceRef> = HashMap::new();
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = ticker.tick() => match self.refresh_all().await {
                    Ok(list) => {
                        watches.sync(&list);
                        workspaces = list.into_iter().map(|ws| (ws.project_id.clone(), ws)).collect();
                    }
                    Err(e) => tracing::error!(error = %e, "projection refresh failed"),
                },
                Some(project_id) = rx.recv() => {
                    tokio::time::sleep(DEBOUNCE).await;
                    let mut due = HashSet::from([project_id]);
                    while let Ok(p) = rx.try_recv() {
                        due.insert(p);
                    }
                    for ws in due.iter().filter_map(|p| workspaces.get(p)) {
                        self.refresh(ws).await;
                    }
                }
            }
        }
    }
}

/// Filesystem watchers, one per workspace, that send the Project id when a
/// task file changes.
struct Watches {
    engine: Arc<dyn EngineAdapter>,
    tx: mpsc::UnboundedSender<String>,
    by_project: HashMap<String, (Vec<PathBuf>, RecommendedWatcher)>,
}

impl Watches {
    fn new(engine: Arc<dyn EngineAdapter>, tx: mpsc::UnboundedSender<String>) -> Self {
        Self {
            engine,
            tx,
            by_project: HashMap::new(),
        }
    }

    /// Watches exactly `workspaces`. A workspace whose directories cannot all
    /// be watched yet (not created, say) is retried on the next sync and
    /// meanwhile refreshed by the timer alone.
    fn sync(&mut self, workspaces: &[WorkspaceRef]) {
        let mut keep = HashSet::new();
        for ws in workspaces {
            keep.insert(ws.project_id.clone());
            let dirs = self.engine.watch_dirs(ws);
            if dirs.is_empty() {
                self.by_project.remove(&ws.project_id);
                continue;
            }
            if self
                .by_project
                .get(&ws.project_id)
                .is_some_and(|(watched, _)| *watched == dirs)
            {
                continue;
            }
            self.by_project.remove(&ws.project_id);
            match self.watch(ws, &dirs) {
                Ok(w) => {
                    self.by_project.insert(ws.project_id.clone(), (dirs, w));
                }
                Err(e) => {
                    tracing::debug!(project = %ws.project_id, error = %e, "workspace not watched yet")
                }
            }
        }
        self.by_project.retain(|p, _| keep.contains(p));
    }

    fn watch(&self, ws: &WorkspaceRef, dirs: &[PathBuf]) -> notify::Result<RecommendedWatcher> {
        let engine = self.engine.clone();
        let tx = self.tx.clone();
        let project_id = ws.project_id.clone();
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                let Ok(event) = res else {
                    return;
                };
                if matches!(event.kind, EventKind::Access(_)) {
                    return;
                }
                if event.paths.iter().any(|p| engine.is_task_change(p)) {
                    let _ = tx.send(project_id.clone());
                }
            })?;
        for dir in dirs {
            watcher.watch(dir, RecursiveMode::NonRecursive)?;
        }
        Ok(watcher)
    }
}

fn log_apply(
    ws: &WorkspaceRef,
    what: &str,
    res: Result<crate::store::Result<()>, tokio::task::JoinError>,
) {
    match res {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::error!(project = %ws.project_id, what, error = %e, "projection failed")
        }
        Err(e) => {
            tracing::error!(project = %ws.project_id, what, error = %e, "projection task panicked")
        }
    }
}
