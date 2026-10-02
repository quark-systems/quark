//! Pulls engine state through the adapter and projects it into the store.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use quark_transcript::{SessionFormat, SessionRoots};

use crate::sessions::{Sessions, TaskTarget};

use crate::engine::{EngineAdapter, EngineTask, WorkspaceRef};

use crate::store::{Store, TranscriptSource};
use crate::transcripts::TranscriptTap;

pub struct Projector {
    store: Arc<Store>,
    engine: Arc<dyn EngineAdapter>,
    transcripts: Arc<TranscriptTap>,
    sessions: Option<Sessions>,
}

impl Projector {
    pub fn new(store: Arc<Store>, engine: Arc<dyn EngineAdapter>) -> Self {
        let transcripts = Arc::new(TranscriptTap::new(store.clone(), SessionRoots::from_env()));
        Self {
            store,
            engine,
            transcripts,
            sessions: None,
        }
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

    /// Refreshes every Project that has a workspace attached. Adapter failures
    /// are recorded and logged per Project, never retried silently.
    pub async fn refresh_all(&self) -> anyhow::Result<()> {
        let store = self.store.clone();
        let projects = tokio::task::spawn_blocking(move || store.list_projects()).await??;
        for project in projects {
            let Some(path) = project.workspace_path else {
                continue;
            };
            let ws = WorkspaceRef {
                project_id: project.id.clone(),
                root: PathBuf::from(path),
            };
            self.refresh(&ws).await;
        }
        Ok(())
    }

    async fn refresh(&self, ws: &WorkspaceRef) {
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
        }
        self.tap_transcripts(ws, tasks).await;

        let started = Instant::now();
        let holds = self.engine.holds(ws).await;
        self.record(ws, "holds", started, holds.as_ref().err())
            .await;
        if let Ok(holds) = holds {
            let store = self.store.clone();
            let project_id = ws.project_id.clone();
            let res =
                tokio::task::spawn_blocking(move || store.apply_holds(&project_id, &holds)).await;
            log_apply(ws, "holds", res);
        }
    }

    /// Projects new coordinator and worker session-log entries. A log that
    /// cannot be read is logged and retried next tick.
    async fn tap_transcripts(&self, ws: &WorkspaceRef, tasks: Vec<EngineTask>) {
        let tap = self.transcripts.clone();
        let store = self.store.clone();
        let ws = ws.clone();
        let res = tokio::task::spawn_blocking(move || {
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
                let source = TranscriptSource::Task { task_id };
                if let Err(e) = tap.poll(project_id, source, worktree, &[format]) {
                    tracing::warn!(project = %project_id, task = %t.id, error = %e, "worker transcript");
                }
            }
        })
        .await;
        if let Err(e) = res {
            tracing::error!(error = %e, "transcript task panicked");
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

    async fn record(
        &self,
        ws: &WorkspaceRef,
        operation: &'static str,
        started: Instant,
        err: Option<&crate::engine::EngineError>,
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

    /// Refreshes on start and then every `interval` until the task is dropped.
    /// Maps each recorded coordinator window again after a daemon start.
    async fn restore_coordinators(&self) {
        let Some(sessions) = &self.sessions else {
            return;
        };
        let store = self.store.clone();
        let targets = match tokio::task::spawn_blocking(move || store.coordinator_terminals()).await
        {
            Ok(Ok(t)) => t,
            Ok(Err(e)) => {
                tracing::error!(error = %e, "reading coordinator windows failed");
                return;
            }
            Err(e) => {
                tracing::error!(error = %e, "coordinator window read panicked");
                return;
            }
        };
        for (project_id, target) in targets {
            if let Err(e) = sessions.set_coordinator(&project_id, Some(target)).await {
                tracing::warn!(project = %project_id, error = %e, "mapping the coordinator terminal failed");
            }
        }
    }

    pub async fn run(self, interval: Duration) {
        self.restore_coordinators().await;
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if let Err(e) = self.refresh_all().await {
                tracing::error!(error = %e, "projection refresh failed");
            }
        }
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
