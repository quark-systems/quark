//! Slice 9 in shadow: treehouse serves, the native pool is compared.
//!
//! [`ShadowPool`] is a [`Pool`] that hands every call to treehouse (or any
//! other `bash` pool) and returns its answer, and around each call asks the
//! [`NativePool`] what it would have done on the same pool:
//!
//! | Call | Compared |
//! |---|---|
//! | [`Pool::acquire`] | which slot it would reuse or create, or that it would refuse ([`NativePool::plan_acquire`], asked first, from local refs) |
//! | [`Pool::release`] | return, leave as is (dirty), or refuse ([`NativePool::plan_release`], asked first) |
//! | [`Pool::list`] | each slot's status, branch, lease id and holder (native reads the state treehouse just wrote, writing nothing) |
//!
//! The native side never acts: plans change nothing, and its listing skips
//! the write-back. Each disagreement is appended to the event log as a
//! `shadow.divergence` (slice `worktree_pool`), once until it changes, so a
//! persistent difference in a pool read every minute is one event.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use quark_core::event::kinds;
use quark_core::slice::Divergence;
use quark_core::{CoreError, EventLog, HostId, NewEvent, ProjectId, Result, Slice};
use serde::Serialize;
use serde_json::{json, Value};

use crate::native::{AcquirePlan, NativePool, ReleasePlan};
use crate::treehouse::{LeaseInfo, Pool, PoolEntry, Release, TreehouseCli};

/// Treehouse serving, the native pool compared. See the module docs.
pub struct ShadowPool<P = TreehouseCli> {
    bash: P,
    native: NativePool,
    log: Option<(Arc<dyn EventLog>, HostId)>,
    /// The last divergence recorded per operation and repo.
    last: Mutex<HashMap<String, Value>>,
}

impl ShadowPool<TreehouseCli> {
    /// Treehouse from `PATH` against the native pool, both on the default
    /// pool root.
    pub fn treehouse() -> Self {
        Self::new(TreehouseCli::new(), NativePool::new())
    }
}

impl<P: Pool> ShadowPool<P> {
    /// `native` must look at the same pools as `bash` (the same root).
    pub fn new(bash: P, native: NativePool) -> Self {
        Self {
            bash,
            native,
            log: None,
            last: Mutex::default(),
        }
    }

    /// Record divergences in `log`, stamped with `host`.
    pub fn with_events(mut self, log: Arc<dyn EventLog>, host: HostId) -> Self {
        self.log = Some((log, host));
        self
    }

    pub fn bash(&self) -> &P {
        &self.bash
    }

    async fn diverged(&self, operation: &str, repo: &Path, bash: Value, native: Value) {
        let key = format!("{operation}\0{}", repo.display());
        let seen = json!([bash, native]);
        {
            let mut last = self.last.lock().unwrap();
            if last.get(&key) == Some(&seen) {
                return;
            }
            last.insert(key, seen);
        }
        tracing::warn!(operation, repo = %repo.display(), %bash, %native, "native worktree pool disagrees with treehouse");
        let Some((log, host)) = &self.log else {
            return;
        };
        let divergence = Divergence {
            slice: Slice::WorktreePool,
            operation: operation.into(),
            bash: json!({ "repo": repo, "result": bash }),
            native: json!({ "repo": repo, "result": native }),
        };
        let event = NewEvent::typed(
            host.clone(),
            ProjectId::engine(),
            None,
            kinds::SHADOW_DIVERGENCE,
            &divergence,
        );
        let appended = match event {
            Ok(e) => log.append(e).await.map(|_| ()),
            Err(e) => Err(e),
        };
        if let Err(e) = appended {
            tracing::warn!(error = %e, "could not record worktree pool divergence");
        }
    }

    fn agreed(&self, operation: &str, repo: &Path) {
        let key = format!("{operation}\0{}", repo.display());
        self.last.lock().unwrap().remove(&key);
    }

    async fn plan<T: Send + 'static>(
        &self,
        f: impl FnOnce(NativePool) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let native = self.native.clone();
        tokio::task::spawn_blocking(move || f(native))
            .await
            .map_err(|e| CoreError::Backend(format!("native pool task: {e}")))?
    }
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| crate::native::state::clean(p))
}

/// Whether treehouse's acquisition matches the native plan.
fn acquire_agrees(plan: &Result<AcquirePlan>, got: &Result<LeaseInfo>) -> bool {
    match (plan, got) {
        (Ok(AcquirePlan::Reuse { path } | AcquirePlan::Create { path }), Ok(info)) => {
            canon(path) == canon(&info.path)
        }
        (Ok(AcquirePlan::Refuse { .. }), Err(_)) => true,
        _ => false,
    }
}

fn release_agrees(plan: &Result<ReleasePlan>, got: &Result<Release>) -> bool {
    matches!(
        (plan, got),
        (Ok(ReleasePlan::Return), Ok(Release::Returned))
            | (
                Ok(ReleasePlan::NotReturned { .. }),
                Ok(Release::NotReturned(_))
            )
            | (Ok(ReleasePlan::Refuse { .. }), Err(_))
    )
}

fn outcome<T: Serialize>(r: &Result<T>) -> Value {
    match r {
        Ok(v) => serde_json::to_value(v).unwrap_or(Value::Null),
        Err(e) => json!({ "outcome": "error", "error": e.to_string() }),
    }
}

/// The fields both implementations must agree on, per slot path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SlotView {
    status: String,
    branch: String,
    detached: bool,
    lease_id: String,
    lease_holder: String,
}

fn view(entries: &[PoolEntry]) -> BTreeMap<PathBuf, SlotView> {
    entries
        .iter()
        .map(|e| {
            (
                canon(&e.path),
                SlotView {
                    status: e.status.clone(),
                    branch: e.branch.clone(),
                    detached: e.detached,
                    lease_id: e.lease_id.clone(),
                    lease_holder: e.lease_holder.clone(),
                },
            )
        })
        .collect()
}

/// Only the slots that differ, from each side.
fn list_difference(
    bash: &[PoolEntry],
    native: &[PoolEntry],
) -> Option<(BTreeMap<PathBuf, SlotView>, BTreeMap<PathBuf, SlotView>)> {
    let (mut b, mut n) = (view(bash), view(native));
    let same: Vec<PathBuf> = b
        .iter()
        .filter(|(p, v)| n.get(*p) == Some(*v))
        .map(|(p, _)| p.clone())
        .collect();
    for p in same {
        b.remove(&p);
        n.remove(&p);
    }
    (!b.is_empty() || !n.is_empty()).then_some((b, n))
}

#[async_trait]
impl<P: Pool> Pool for ShadowPool<P> {
    async fn acquire(
        &self,
        repo: &Path,
        holder: &str,
        branch: Option<&str>,
        base: Option<&str>,
    ) -> Result<LeaseInfo> {
        let (r, b, s) = (
            repo.to_path_buf(),
            branch.map(str::to_string),
            base.map(str::to_string),
        );
        let plan = self
            .plan(move |n| n.plan_acquire(&r, b.as_deref(), s.as_deref()))
            .await;
        let got = self.bash.acquire(repo, holder, branch, base).await;
        if acquire_agrees(&plan, &got) {
            self.agreed("acquire", repo);
        } else {
            let bash = match &got {
                Ok(info) => json!({ "outcome": "ok", "path": info.path }),
                Err(e) => json!({ "outcome": "refuse", "reason": e.to_string() }),
            };
            self.diverged("acquire", repo, bash, outcome(&plan)).await;
        }
        got
    }

    async fn release(&self, repo: &Path, path: &Path, lease_id: &str) -> Result<Release> {
        let (r, p, id) = (repo.to_path_buf(), path.to_path_buf(), lease_id.to_string());
        let plan = self.plan(move |n| n.plan_release(&r, &p, &id)).await;
        let got = self.bash.release(repo, path, lease_id).await;
        if release_agrees(&plan, &got) {
            self.agreed("release", repo);
        } else {
            let bash = match &got {
                Ok(Release::Returned) => json!({ "outcome": "return" }),
                Ok(Release::NotReturned(why)) => {
                    json!({ "outcome": "not_returned", "reason": why })
                }
                Err(e) => json!({ "outcome": "refuse", "reason": e.to_string() }),
            };
            self.diverged("release", repo, bash, outcome(&plan)).await;
        }
        got
    }

    async fn list(&self, repo: &Path) -> Result<Vec<PoolEntry>> {
        let got = self.bash.list(repo).await;
        let Ok(entries) = &got else {
            return got;
        };
        let r = repo.to_path_buf();
        match self.plan(move |n| n.list_blocking(&r, false)).await {
            Ok(native) => match list_difference(entries, &native) {
                None => self.agreed("list", repo),
                Some((b, n)) => {
                    self.diverged("list", repo, json!(b), json!(n)).await;
                }
            },
            Err(e) => {
                self.diverged(
                    "list",
                    repo,
                    json!({ "slots": entries.len() }),
                    json!({ "error": e.to_string() }),
                )
                .await;
            }
        }
        got
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, status: &str) -> PoolEntry {
        PoolEntry {
            name: "1".into(),
            path: path.into(),
            status: status.into(),
            branch: String::new(),
            detached: true,
            recovery_reason: String::new(),
            lease_id: String::new(),
            lease_holder: String::new(),
        }
    }

    #[test]
    fn list_difference_keeps_only_differing_slots() {
        let b = [entry("/nope/1/r", "available"), entry("/nope/2/r", "dirty")];
        let n = [
            entry("/nope/1/r", "available"),
            entry("/nope/2/r", "available"),
        ];
        let (bd, nd) = list_difference(&b, &n).unwrap();
        assert_eq!(bd.len(), 1);
        assert_eq!(nd[&PathBuf::from("/nope/2/r")].status, "available");
        assert!(list_difference(&b, &b).is_none());
    }

    #[test]
    fn agreement_rules() {
        let info = LeaseInfo {
            path: "/nope/1/r".into(),
            lease_id: "x".into(),
            lease_holder: String::new(),
            base_branch: String::new(),
        };
        let reuse = Ok(AcquirePlan::Reuse {
            path: "/nope/1/r".into(),
        });
        assert!(acquire_agrees(&reuse, &Ok(info.clone())));
        assert!(!acquire_agrees(
            &Ok(AcquirePlan::Create {
                path: "/nope/2/r".into()
            }),
            &Ok(info)
        ));
        assert!(acquire_agrees(
            &Ok(AcquirePlan::Refuse {
                reason: "full".into()
            }),
            &Err(CoreError::Backend("full".into()))
        ));
        assert!(!acquire_agrees(
            &reuse,
            &Err(CoreError::Backend("x".into()))
        ));
        assert!(release_agrees(
            &Ok(ReleasePlan::Return),
            &Ok(Release::Returned)
        ));
        assert!(!release_agrees(
            &Ok(ReleasePlan::Return),
            &Ok(Release::NotReturned("dirty".into()))
        ));
    }
}
