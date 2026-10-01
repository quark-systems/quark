//! Pulls engine state through the adapter and projects it into the store.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use quark_engine::{EngineAdapter, WorkspaceRef};

use crate::store::Store;

pub struct Projector {
    store: Arc<Store>,
    engine: Arc<dyn EngineAdapter>,
}

impl Projector {
    pub fn new(store: Arc<Store>, engine: Arc<dyn EngineAdapter>) -> Self {
        Self { store, engine }
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
        if let Ok(snapshot) = snapshot {
            let store = self.store.clone();
            let project_id = ws.project_id.clone();
            let res =
                tokio::task::spawn_blocking(move || store.apply_snapshot(&project_id, &snapshot))
                    .await;
            log_apply(ws, "snapshot", res);
        }

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

    async fn record(
        &self,
        ws: &WorkspaceRef,
        operation: &'static str,
        started: Instant,
        err: Option<&quark_engine::EngineError>,
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
    pub async fn run(self, interval: Duration) {
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
