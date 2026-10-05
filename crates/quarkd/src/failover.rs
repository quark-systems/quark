//! Failover on rate limits (ADR-11): a worker whose account runs out moves
//! to another account in its pool.
//!
//! The transcript tap reports a rate limit when a worker's session log ends
//! at one (see [`quark_transcript::RateLimit`] for the lines matched). The
//! daemon then picks the next healthy account and has the engine relaunch
//! the worker from its branch, in the same worktree, with that account's
//! variable (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`) set. The engine owns the
//! relaunch; nothing of it is done here.
//!
//! The pool is the Project agent config's when the worker runs that
//! harness, else any pool the worker's account is in.
//!
//! A task never goes back, on its own, to an account it already left, and
//! passes over accounts any task left within [`RATE_LIMIT_COOLDOWN`]. So a
//! task fails over at most once per account, and with no healthy account
//! left it opens a decision instead of relaunching again. Answering that
//! decision relaunches the worker, under another account if one is healthy
//! by then and under its own otherwise.

use std::sync::Arc;
use std::time::Duration;

use quark_systems::{AccountFailover, AgentConfig, Decision, FailoverOutcome, Task, TaskState};
use quark_transcript::RateLimit;

use crate::accounts::{AccountError, Accounts, Holder, Lease};
use crate::engine::{EngineAdapter, EngineError, TaskControl, WorkspaceRef};
use crate::harness::HarnessRegistry;
use crate::store::{default_account_id, later, Store, StoreError};

/// How long an account a worker reported a rate limit on is passed over
/// when choosing where another worker goes.
pub const RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(60 * 60);

/// What a relaunch on answering a rate-limit decision is recorded as, when
/// it moved the worker.
const ANSWER_SIGNAL: &str = "quark: rate-limit decision answered";

#[derive(Debug, thiserror::Error)]
pub enum FailoverError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Account(#[from] AccountError),
    #[error(transparent)]
    Engine(#[from] EngineError),
}

pub struct Failover {
    store: Arc<Store>,
    engine: Arc<dyn EngineAdapter>,
    accounts: Arc<Accounts>,
    harnesses: Arc<HarnessRegistry>,
}

impl Failover {
    pub fn new(
        store: Arc<Store>,
        engine: Arc<dyn EngineAdapter>,
        accounts: Arc<Accounts>,
        harnesses: Arc<HarnessRegistry>,
    ) -> Self {
        Self {
            store,
            engine,
            accounts,
            harnesses,
        }
    }

    /// Handles a rate limit `task_id`'s worker reported: relaunches it under
    /// the next healthy account of its pool, or opens a decision when there
    /// is none or the relaunch fails. Returns what was recorded; `None` for
    /// a report that needs nothing (the task is finished, is already waiting
    /// on a rate-limit decision, or was moved after the report was written).
    pub async fn rate_limited(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
        limit: &RateLimit,
    ) -> Result<Option<AccountFailover>, FailoverError> {
        let task = self.task(task_id).await?;
        if matches!(task.state, TaskState::Done | TaskState::Failed) {
            return Ok(None);
        }
        if let (Some(ts), Some(last)) = (&limit.ts, task.failovers.last()) {
            if !later(ts, Some(&last.at)) {
                return Ok(None);
            }
        }
        if self.open_decision(task_id).await?.is_some() {
            return Ok(None);
        }
        let Some(target) = self.target(&task).await? else {
            return Ok(None);
        };

        // The reading catches up in the background, so the Accounts screen
        // and later starts see the account is out.
        let (accounts, from) = (self.accounts.clone(), target.from.clone());
        tokio::spawn(async move {
            if let Err(e) = accounts.refresh_quota(Some(&from)).await {
                tracing::debug!(account = %from, error = %e, "quota refresh after a rate limit failed");
            }
        });

        let mut exclude: Vec<String> = task
            .failovers
            .iter()
            .map(|f| f.from_account_id.clone())
            .collect();
        exclude.extend(self.cooling_down().await?);
        let next = self
            .accounts
            .next_account(task_id, &target.harness, target.pool.as_deref(), &exclude)
            .await?;

        let from_label = self.label(&target.from).await;
        let failover = |outcome, to: Option<&Lease>, detail: Option<String>| AccountFailover {
            from_account_id: target.from.clone(),
            to_account_id: to.map(|l| l.account_id.clone()),
            pool: target.pool.clone(),
            outcome,
            signal: limit.signal.to_string(),
            detail,
            at: crate::now_rfc3339(),
        };
        let (record, note, question) = match next {
            None => {
                let within = match &target.pool {
                    Some(p) => format!("pool `{p}`"),
                    None => "its pools".to_string(),
                };
                (
                    failover(FailoverOutcome::NoHealthyAccount, None, limit.message.clone()),
                    format!("rate limit on {from_label}; no other account in {within} is healthy"),
                    Some(format!(
                        "{}: the worker hit a rate limit on {from_label} and no other account in {within} is healthy. Answer once an account can be used again (log one in, add one to the pool, or wait for the limit to reset): Quark then relaunches the worker and passes it your answer as a note.",
                        task.title
                    )),
                )
            }
            Some(lease) => {
                let to_label = self.label(&lease.account_id).await;
                let note = format!(
                    "Relaunched by Quark under another account after a rate limit on {from_label}. Pick up from the current state of the branch and worktree, and the task brief."
                );
                match self.relaunch(ws, &target.engine_id, note, &lease).await {
                    Ok(()) => (
                        failover(FailoverOutcome::Relaunched, Some(&lease), limit.message.clone()),
                        format!("rate limit on {from_label}; relaunched under {to_label}"),
                        None,
                    ),
                    Err(e) => (
                        failover(FailoverOutcome::RelaunchFailed, None, Some(e.to_string())),
                        format!("rate limit on {from_label}; relaunch under {to_label} failed: {e}"),
                        Some(format!(
                            "{}: the worker hit a rate limit on {from_label}, and relaunching it under {to_label} failed: {e}. Answer once that is fixed: Quark then relaunches the worker and passes it your answer as a note.",
                            task.title
                        )),
                    ),
                }
            }
        };
        self.record(task_id, record.clone(), note, question).await?;
        Ok(Some(record))
    }

    /// Relaunches a task's worker after its rate-limit decision was
    /// answered: under another account of its pool when one is healthy (the
    /// accounts the task left earlier count again, since a person said to go
    /// on), else under its own. `answer` reaches the new worker as its note.
    /// The caller records the answer once this returns `Ok`.
    pub async fn resume(
        &self,
        ws: &WorkspaceRef,
        decision: &Decision,
        answer: &str,
    ) -> Result<(), FailoverError> {
        let task_id = decision.task_id.as_deref().ok_or(StoreError::NotFound)?;
        let task = self.task(task_id).await?;
        let Some(target) = self.target(&task).await? else {
            return Err(
                EngineError::Invalid("the task has no harness with accounts".into()).into(),
            );
        };
        let exclude = self.cooling_down().await?;
        let next = self
            .accounts
            .next_account(task_id, &target.harness, target.pool.as_deref(), &exclude)
            .await?;
        let lease = match &next {
            Some(lease) => lease.clone(),
            // Its own account, as any relaunch would keep it.
            None => {
                let config = AgentConfig {
                    harness: target.harness.clone(),
                    model: None,
                    effort: None,
                    pool: None,
                };
                self.accounts
                    .lease(&Holder::Task(task_id.to_string()), &config)
                    .await?
                    .unwrap_or(Lease {
                        account_id: target.from.clone(),
                        env: Vec::new(),
                    })
            }
        };
        let note = format!(
            "Relaunched by Quark after a rate limit. Pick up from the current state of the branch and worktree, and the task brief. A person answered: {}",
            answer.trim()
        );
        self.relaunch(ws, &target.engine_id, note, &lease).await?;
        if next.is_some() {
            let record = AccountFailover {
                from_account_id: target.from.clone(),
                to_account_id: Some(lease.account_id.clone()),
                pool: target.pool.clone(),
                outcome: FailoverOutcome::Relaunched,
                signal: ANSWER_SIGNAL.to_string(),
                detail: None,
                at: crate::now_rfc3339(),
            };
            let note = format!(
                "rate limit on {}; relaunched under {}",
                self.label(&target.from).await,
                self.label(&lease.account_id).await
            );
            self.record(task_id, record, note, None).await?;
        }
        Ok(())
    }

    /// Through the engine's own lifecycle control (`fm-control.sh <task>
    /// relaunch` for firstmate), keeping the worker's harness, model and
    /// effort.
    async fn relaunch(
        &self,
        ws: &WorkspaceRef,
        engine_id: &str,
        note: String,
        lease: &Lease,
    ) -> Result<(), EngineError> {
        let action = TaskControl::Relaunch {
            harness: None,
            model: None,
            effort: None,
            note,
            account_env: lease.env.clone(),
        };
        self.engine.control(ws, engine_id, &action).await
    }

    /// Where the task runs and which accounts it may move among; `None` for
    /// a harness without accounts.
    async fn target(&self, task: &Task) -> Result<Option<Target>, FailoverError> {
        let Some(harness) = task.harness.clone() else {
            return Ok(None);
        };
        let Some(h) = self.harnesses.resolve(&harness).cloned() else {
            return Ok(None);
        };
        if h.account_env().is_none() {
            return Ok(None);
        }
        let (store, task_id, project_id) =
            (self.store.clone(), task.id.clone(), task.project_id.clone());
        let (engine_id, project) = blocking(move || {
            Ok((
                store.task_target(&task_id)?.engine_id,
                store.get_project(&project_id)?,
            ))
        })
        .await?;
        let pool = project
            .agent_config
            .filter(|a| self.harnesses.resolve(&a.harness).map(|x| x.id()) == Some(h.id()))
            .and_then(|a| a.pool);
        Ok(Some(Target {
            engine_id,
            from: task
                .account_id
                .clone()
                .unwrap_or_else(|| default_account_id(h.id())),
            harness,
            pool,
        }))
    }

    async fn task(&self, task_id: &str) -> Result<Task, StoreError> {
        let (store, id) = (self.store.clone(), task_id.to_string());
        blocking(move || store.get_task(&id)).await
    }

    async fn open_decision(&self, task_id: &str) -> Result<Option<Decision>, StoreError> {
        let (store, id) = (self.store.clone(), task_id.to_string());
        blocking(move || store.open_failover_decision(&id)).await
    }

    async fn cooling_down(&self) -> Result<Vec<String>, StoreError> {
        let since = (time::OffsetDateTime::now_utc() - RATE_LIMIT_COOLDOWN)
            .format(&time::format_description::well_known::Rfc3339)
            .expect("RFC 3339 formatting never fails for UTC");
        let store = self.store.clone();
        blocking(move || store.rate_limited_since(&since)).await
    }

    async fn record(
        &self,
        task_id: &str,
        failover: AccountFailover,
        note: String,
        question: Option<String>,
    ) -> Result<(), StoreError> {
        let (store, id) = (self.store.clone(), task_id.to_string());
        blocking(move || {
            store
                .record_failover(&id, &failover, &note, question.as_deref())
                .map(drop)
        })
        .await
    }

    /// An account as a person knows it, e.g. `Claude Code account "Work"`.
    async fn label(&self, account_id: &str) -> String {
        match self.accounts.get(account_id).await {
            Ok(a) => {
                let harness = self
                    .harnesses
                    .get(&a.harness)
                    .map_or(a.harness.clone(), |h| h.name().to_string());
                format!("{harness} account \"{}\"", a.label)
            }
            Err(_) => format!("account {account_id}"),
        }
    }
}

struct Target {
    engine_id: String,
    /// The harness as the task reports it.
    harness: String,
    /// The account the worker runs under now.
    from: String,
    pool: Option<String>,
}

async fn blocking<T, F>(f: F) -> Result<T, StoreError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, StoreError> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| StoreError::Invalid(e.to_string()))?
}
