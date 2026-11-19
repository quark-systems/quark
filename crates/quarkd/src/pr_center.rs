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
//! A pull request merged outside the engine (on the forge, or by its own
//! worker) is handed to the engine once per daemon run, so the task lands
//! and the coordinator hears of the merge.
//!
//! Standing approval: for each Project that has it on, every open pull
//! request that is mergeable, green and not held by a review is merged
//! through the engine's guarded merge. Each head commit is tried once, so a
//! refusal is not repeated until the pull request moves. A stale head is the
//! exception: main moved during the merge, so the next pass rebases again.
//!
//! An open pull request that conflicts with its base or has a failed check
//! cannot merge until someone acts, so its worker is told, once per head.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use quark_systems::{ChecksState, Mergeability, PullRequest};

use crate::engine::{EngineAdapter, WorkspaceRef};
use crate::forge::Forge;
use crate::store::{PrOwner, Store, StoreError};

pub struct PrCenter {
    store: Arc<Store>,
    engine: Arc<dyn EngineAdapter>,
    forge: Arc<dyn Forge>,
    /// `(pull request id, head sha)` pairs standing approval already tried.
    attempted: Mutex<HashSet<(String, String)>>,
    /// `(pull request id, head sha, problem)` its worker was told of.
    flagged: Mutex<HashSet<(String, String, &'static str)>>,
    /// Merged pull requests the engine has been told of.
    told: Mutex<HashSet<String>>,
}

impl PrCenter {
    pub fn new(store: Arc<Store>, engine: Arc<dyn EngineAdapter>, forge: Arc<dyn Forge>) -> Self {
        Self {
            store,
            engine,
            forge,
            attempted: Mutex::default(),
            flagged: Mutex::default(),
            told: Mutex::default(),
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
        self.finish_landed().await;
        self.merged_outside().await;
        self.coordinator_merges().await;
        self.stranded().await;
        self.tell_workers().await;
        self.retargets().await;
        self.standing_approval().await;
    }

    /// Tells the engine of each merged pull request it has not heard of
    /// from here. The engine ignores merges it made or already knows, so
    /// this only matters for merges made outside it; a failed call is
    /// retried next pass.
    async fn merged_outside(&self) {
        let store = self.store.clone();
        let merged = match tokio::task::spawn_blocking(move || store.merged_with_task()).await {
            Ok(Ok(m)) => m,
            Ok(Err(e)) => {
                tracing::error!(error = %e, "listing merged pull requests failed");
                return;
            }
            Err(e) => {
                tracing::error!(error = %e, "merged pull request listing panicked");
                return;
            }
        };
        for owner in merged {
            let pr = &owner.pull_request;
            if self.told.lock().unwrap().contains(&pr.id) {
                continue;
            }
            let Ok((ws, task)) = owner_target(&owner) else {
                continue;
            };
            match self.engine.pull_request_merged(&ws, &task, &pr.url).await {
                Ok(()) => {
                    self.told.lock().unwrap().insert(pr.id.clone());
                }
                Err(e) => {
                    tracing::warn!(pr = %pr.url, error = %e, "engine not told of a merge")
                }
            }
        }
    }

    /// Tells each Project's coordinator of a merged pull request it opened
    /// itself. No task owns those, so nothing else would wake it, and work
    /// it held for the merge would wait for good. Told once; a failed call is
    /// retried next pass.
    async fn coordinator_merges(&self) {
        let store = self.store.clone();
        let merged =
            match tokio::task::spawn_blocking(move || store.coordinator_merges_untold()).await {
                Ok(Ok(m)) => m,
                Ok(Err(e)) => {
                    tracing::error!(error = %e, "listing coordinator merges failed");
                    return;
                }
                Err(e) => {
                    tracing::error!(error = %e, "coordinator merge listing panicked");
                    return;
                }
            };
        for m in merged {
            let Some(root) = m.workspace_path.as_ref() else {
                continue;
            };
            let ws = WorkspaceRef {
                project_id: m.project_id.clone(),
                root: root.into(),
            };
            if let Err(e) = self
                .engine
                .coordinator_pull_request_merged(&ws, &m.url)
                .await
            {
                tracing::warn!(pr = %m.url, error = %e, "coordinator not told of a merge");
                continue;
            }
            let store = self.store.clone();
            let id = m.id.clone();
            match tokio::task::spawn_blocking(move || store.mark_merge_told(&id)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::warn!(pr = %m.url, error = %e, "merge not marked told"),
                Err(e) => tracing::warn!(pr = %m.url, error = %e, "merge marking panicked"),
            }
        }
    }

    /// Finishes the tasks whose pull request landed since a forge read: one
    /// merged into another branch lands when that branch's pull request
    /// merges later.
    async fn finish_landed(&self) {
        let store = self.store.clone();
        match tokio::task::spawn_blocking(move || store.finish_landed()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::error!(error = %e, "finishing landed tasks failed"),
            Err(e) => tracing::error!(error = %e, "finishing landed tasks panicked"),
        }
    }

    /// Tells the engine of each merge that missed the default branch, so the
    /// coordinator learns the change never landed instead of starting work
    /// that waited on it. Told once; a failed call is retried next pass.
    async fn stranded(&self) {
        let store = self.store.clone();
        let stranded = match tokio::task::spawn_blocking(move || store.stranded_untold()).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                tracing::error!(error = %e, "listing stranded merges failed");
                return;
            }
            Err(e) => {
                tracing::error!(error = %e, "stranded merge listing panicked");
                return;
            }
        };
        for s in stranded {
            let Some(root) = s.workspace_path.as_ref() else {
                continue;
            };
            let ws = WorkspaceRef {
                project_id: s.project_id.clone(),
                root: root.into(),
            };
            tracing::warn!(pr = %s.url, base = %s.base, "merge missed the default branch");
            if let Err(e) = self
                .engine
                .pull_request_stranded(&ws, s.task.as_deref(), &s.url, &s.base, &s.default_branch)
                .await
            {
                tracing::warn!(pr = %s.url, error = %e, "coordinator not told of a stranded merge");
                continue;
            }
            let store = self.store.clone();
            let id = s.id.clone();
            match tokio::task::spawn_blocking(move || store.mark_stranded_told(&id)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::warn!(pr = %s.url, error = %e, "stranded merge not marked"),
                Err(e) => tracing::warn!(pr = %s.url, error = %e, "stranded marking panicked"),
            }
        }
    }

    /// Tells each worker whose open pull request sits on a branch whose own
    /// pull request already merged to retarget it, before a merge strands
    /// it. Once per pull request and base.
    async fn retargets(&self) {
        let store = self.store.clone();
        let list = match tokio::task::spawn_blocking(move || store.needing_retarget()).await {
            Ok(Ok(l)) => l,
            Ok(Err(e)) => {
                tracing::error!(error = %e, "listing pull requests to retarget failed");
                return;
            }
            Err(e) => {
                tracing::error!(error = %e, "retarget listing panicked");
                return;
            }
        };
        for r in list {
            let pr = &r.owner.pull_request;
            let key = (pr.id.clone(), r.base.clone(), "retarget");
            if self.flagged.lock().unwrap().contains(&key) {
                continue;
            }
            let Ok((ws, task)) = owner_target(&r.owner) else {
                continue;
            };
            let message = retarget_message(&pr.url, &r.base, &r.default_branch);
            match self.engine.send_message(&ws, &task, &message).await {
                Ok(()) => {
                    tracing::info!(pr = %pr.url, "told the worker to retarget");
                    self.flagged.lock().unwrap().insert(key);
                }
                Err(e) => tracing::warn!(pr = %pr.url, error = %e, "worker not told to retarget"),
            }
        }
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

    /// Sends each pull request's worker what stands in its way. A failed
    /// send is retried next pass.
    async fn tell_workers(&self) {
        let store = self.store.clone();
        let stuck = match tokio::task::spawn_blocking(move || store.needing_worker()).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                tracing::error!(error = %e, "listing stuck pull requests failed");
                return;
            }
            Err(e) => {
                tracing::error!(error = %e, "stuck pull request listing panicked");
                return;
            }
        };
        for owner in stuck {
            let pr = &owner.pull_request;
            let Some((problem, message)) = worker_problem(pr) else {
                continue;
            };
            let key = (
                pr.id.clone(),
                pr.head_sha.clone().unwrap_or_default(),
                problem,
            );
            if self.flagged.lock().unwrap().contains(&key) {
                continue;
            }
            let Ok((ws, task)) = owner_target(&owner) else {
                continue;
            };
            match self.engine.send_message(&ws, &task, &message).await {
                Ok(()) => {
                    tracing::info!(pr = %pr.url, problem, "told the worker");
                    self.flagged.lock().unwrap().insert(key);
                }
                Err(e) => tracing::warn!(pr = %pr.url, error = %e, "worker not told"),
            }
        }
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
            if !self.attempted.lock().unwrap().insert(key.clone()) {
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
                    tracing::warn!(pr = %pr.url, error = %e, "standing approval merge refused");
                    if e.to_string().contains("stale head") {
                        self.attempted.lock().unwrap().remove(&key);
                    }
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
    /// The pull request targets a branch other than the default, so merging
    /// it would not land its change.
    #[error("{0}")]
    OffDefault(String),
}

/// What stands in `pr`'s way, and the steering message that says so. A
/// conflict comes first: checks do not run on a conflicted head.
fn worker_problem(pr: &PullRequest) -> Option<(&'static str, String)> {
    let url = &pr.url;
    let base = pr.base_ref.as_deref().unwrap_or("its base branch");
    if pr.mergeable == Mergeability::Conflicting {
        return Some((
            "conflict",
            format!(
                "Your pull request {url} has merge conflicts with {base}. Fetch the latest \
                 {base} from the remote that hosts the pull request, rebase onto it, resolve \
                 the conflicts, and force-push the same branch with a lease."
            ),
        ));
    }
    (pr.checks_state == ChecksState::Failing).then(|| {
        (
            "checks",
            format!(
                "Checks failed on your pull request {url}. Read the failures, fix them on the \
                 same branch and push."
            ),
        )
    })
}

/// What a worker is told when its pull request's base branch already
/// merged into `default_branch`.
pub fn retarget_message(url: &str, base: &str, default_branch: &str) -> String {
    format!(
        "Your pull request {url} targets {base}, whose own pull request has already merged. \
         Merged as it is, its change lands on {base} and never reaches {default_branch}. \
         Rebase it onto the latest {default_branch}, force-push the same branch with a lease, \
         and change its base to {default_branch} (gh pr edit {url} --base {default_branch})."
    )
}

/// Reads one pull request from its forge and projects it. A failed read is
/// recorded on the pull request and returned.
pub async fn sync(
    store: &Arc<Store>,
    forge: &dyn Forge,
    id: &str,
    url: &str,
) -> Result<PullRequest, PrError> {
    let mut read = forge.pull_request(url).await;
    // A failed read of the default branch keeps the one recorded.
    if let Ok(pr) = read.as_mut() {
        match forge.default_branch(url).await {
            Ok(b) => pr.default_branch = b,
            Err(e) => tracing::debug!(pr = url, error = %e, "default branch not read"),
        }
    }
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
    let off = {
        let store = store.clone();
        let id = pr.id.clone();
        tokio::task::spawn_blocking(move || store.off_default(&id))
            .await
            .map_err(|e| PrError::Store(StoreError::Invalid(e.to_string())))??
    };
    if let Some((base, default_branch)) = off {
        return Err(PrError::OffDefault(format!(
            "{} targets {base}, not {default_branch}: merged now, its change would not reach \
             {default_branch}. Change its base to {default_branch} first.",
            pr.url
        )));
    }
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
