//! Keeps the PR center's projection current and applies standing approval.
//!
//! On a timer, records a row for every pull request a task reports, reads
//! each one that is still open (or never read) from its forge, and projects
//! the result. Merged and closed pull requests are read once more and then
//! left alone, so the forge is only polled for live work.
//!
//! Each refresh also reads the owning task's verification gate results
//! (ADR-15) into the pull request's `evidence`.
//!
//! Standing approval: for each Project that has it on, every open pull
//! request that is mergeable, green and not held by a review is merged
//! through the engine's guarded merge. Each head commit is tried once, so a
//! refusal is not repeated until the pull request moves.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use quark_systems::PullRequest;

use crate::engine::{EngineAdapter, WorkspaceRef};
use crate::forge::Forge;
use crate::store::{PrOwner, Store, StoreError};

pub struct PrCenter {
    store: Arc<Store>,
    engine: Arc<dyn EngineAdapter>,
    forge: Arc<dyn Forge>,
    /// `(pull request id, head sha)` pairs standing approval already tried.
    attempted: Mutex<HashSet<(String, String)>>,
}

impl PrCenter {
    pub fn new(store: Arc<Store>, engine: Arc<dyn EngineAdapter>, forge: Arc<dyn Forge>) -> Self {
        Self {
            store,
            engine,
            forge,
            attempted: Mutex::default(),
        }
    }

    /// One pass: discover, read from the forge, then merge what standing
    /// approval allows.
    pub async fn refresh(&self) {
        let store = self.store.clone();
        let targets = match tokio::task::spawn_blocking(move || store.ensure_pull_requests()).await
        {
            Ok(Ok(t)) => t,
            Ok(Err(e)) => {
                tracing::error!(error = %e, "listing pull requests failed");
                return;
            }
            Err(e) => {
                tracing::error!(error = %e, "pull request listing panicked");
                return;
            }
        };
        for t in targets {
            if let Err(e) = sync(&self.store, self.forge.as_ref(), &t.id, &t.url).await {
                tracing::debug!(pr = %t.url, error = %e, "pull request not refreshed");
            }
            if let Err(e) = self.evidence(&t.id).await {
                tracing::debug!(pr = %t.url, error = %e, "gate evidence not refreshed");
            }
        }
        self.standing_approval().await;
    }

    /// Reads the owning task's gate results into the pull request.
    async fn evidence(&self, id: &str) -> Result<(), PrError> {
        let store = self.store.clone();
        let pid = id.to_string();
        let owner = tokio::task::spawn_blocking(move || store.pull_request_owner(&pid))
            .await
            .map_err(|e| PrError::Store(StoreError::Invalid(e.to_string())))??;
        let Ok((ws, task)) = owner_target(&owner) else {
            return Ok(());
        };
        let evidence = self.engine.gate_evidence(&ws, &task).await?;
        let store = self.store.clone();
        let pid = id.to_string();
        tokio::task::spawn_blocking(move || store.apply_evidence(&pid, evidence))
            .await
            .map_err(|e| PrError::Store(StoreError::Invalid(e.to_string())))??;
        Ok(())
    }

    async fn standing_approval(&self) {
        let store = self.store.clone();
        let ready = match tokio::task::spawn_blocking(move || store.standing_approval_ready()).await
        {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                tracing::error!(error = %e, "reading standing approval failed");
                return;
            }
            Err(e) => {
                tracing::error!(error = %e, "standing approval read panicked");
                return;
            }
        };
        for owner in ready {
            let pr = &owner.pull_request;
            let key = (pr.id.clone(), pr.head_sha.clone().unwrap_or_default());
            if !self.attempted.lock().unwrap().insert(key) {
                continue;
            }
            tracing::info!(pr = %pr.url, "merging under standing approval");
            match merge(
                &self.store,
                self.engine.as_ref(),
                self.forge.as_ref(),
                &owner,
                None,
            )
            .await
            {
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(pr = %pr.url, error = %e, "standing approval merge refused")
                }
            }
        }
    }

    /// Refreshes on start and every `interval` until the task is dropped.
    pub async fn run(self, interval: Duration) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            self.refresh().await;
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PrError {
    #[error("{0}")]
    Store(#[from] StoreError),
    #[error("{0}")]
    Forge(#[from] crate::forge::ForgeError),
    #[error("{0}")]
    Engine(#[from] crate::engine::EngineError),
    /// The pull request has no task, or the task's Project no workspace.
    #[error("{0}")]
    NoOwner(&'static str),
}

/// Reads one pull request from its forge and projects it. A failed read is
/// recorded on the pull request and returned.
pub async fn sync(
    store: &Arc<Store>,
    forge: &dyn Forge,
    id: &str,
    url: &str,
) -> Result<PullRequest, PrError> {
    let read = forge.pull_request(url).await;
    let store = store.clone();
    let id = id.to_string();
    let res = tokio::task::spawn_blocking(move || match read {
        Ok(pr) => store.apply_forge_pr(&id, &pr).map_err(PrError::from),
        Err(e) => {
            store.set_pr_sync_error(&id, &e.to_string())?;
            Err(PrError::from(e))
        }
    })
    .await
    .map_err(|e| PrError::Store(StoreError::Invalid(e.to_string())))?;
    res
}

/// The engine workspace and task that own a pull request.
pub fn owner_target(owner: &PrOwner) -> Result<(WorkspaceRef, String), PrError> {
    let task = owner
        .task
        .as_ref()
        .ok_or(PrError::NoOwner("no task owns this pull request"))?;
    let root = task.workspace_path.as_ref().ok_or(PrError::NoOwner(
        "the pull request's Project has no workspace attached",
    ))?;
    Ok((
        WorkspaceRef {
            project_id: task.project_id.clone(),
            root: root.into(),
        },
        task.engine_id.clone(),
    ))
}

/// Merges through the engine, then reads the result back from the forge.
pub async fn merge(
    store: &Arc<Store>,
    engine: &dyn EngineAdapter,
    forge: &dyn Forge,
    owner: &PrOwner,
    method: Option<quark_systems::MergeMethod>,
) -> Result<PullRequest, PrError> {
    let (ws, task) = owner_target(owner)?;
    let pr = &owner.pull_request;
    engine
        .merge_pull_request(&ws, &task, &pr.url, method)
        .await?;
    match sync(store, forge, &pr.id, &pr.url).await {
        Ok(pr) => Ok(pr),
        // Merged, but the read-back failed: report the last known state.
        Err(_) => {
            let store = store.clone();
            let id = pr.id.clone();
            Ok(
                tokio::task::spawn_blocking(move || store.get_pull_request(&id))
                    .await
                    .map_err(|e| PrError::Store(StoreError::Invalid(e.to_string())))??,
            )
        }
    }
}
