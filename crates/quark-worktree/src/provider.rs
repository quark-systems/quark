//! [`TreehouseProvider`]: the [`WorktreeProvider`] over a [`Pool`].

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use quark_core::event::kinds;
use quark_core::worktree::{
    Holder, ProviderStatus, ReturnOutcome, Slot, SlotState, Worktree, WorktreeEvent,
    WorktreeProvider, WorktreeRequest,
};
use quark_core::{CoreError, EventLog, HostId, NewEvent, ProjectId, Result, TaskId};

use crate::checks;
use crate::treehouse::{LeaseInfo, Pool, PoolEntry, Release, TreehouseCli};

const TASK_PREFIX: &str = "quark:task:";
const LEASE_PREFIX: &str = "quark:lease:";

/// A [`WorktreeProvider`] that leases worktrees from a [`Pool`] (treehouse by
/// default) and wraps every call in the checks of [`crate::checks`].
///
/// - `get` and `lease` refuse a `repo` that is not a primary checkout, then
///   assert what the pool handed out is an isolated, untangled worktree on
///   the requested branch; anything else is given straight back and refused.
/// - `return_worktree` runs the landed-work check first and reports
///   [`ReturnOutcome::Kept`] without asking the pool when work would be lost.
/// - Each handout, lease and return is appended to the event log as
///   `worktree.change` when one is attached.
///
/// Worktree ids are the pool's per-acquisition lease ids, and the holder
/// (with its Project) is recorded as the lease holder, so the provider keeps
/// no state of its own beyond the set of repos whose pools it reports.
pub struct TreehouseProvider<P = TreehouseCli> {
    pool: P,
    repos: Mutex<BTreeSet<PathBuf>>,
    events: Option<(Arc<dyn EventLog>, HostId)>,
}

impl TreehouseProvider<TreehouseCli> {
    /// Over `treehouse` from `PATH`.
    pub fn treehouse() -> Self {
        Self::new(TreehouseCli::new())
    }
}

impl<P: Pool> TreehouseProvider<P> {
    pub fn new(pool: P) -> Self {
        Self {
            pool,
            repos: Mutex::default(),
            events: None,
        }
    }

    /// Mirror every change into `log`, stamped with `host`.
    pub fn with_events(mut self, log: Arc<dyn EventLog>, host: HostId) -> Self {
        self.events = Some((log, host));
        self
    }

    /// Report and search this repo's pool. Repos are added on their first
    /// `get` or `lease` too; a restarted daemon re-adds its Projects' repos.
    pub fn add_repo(&self, repo: impl Into<PathBuf>) {
        let repo = repo.into();
        let repo = std::fs::canonicalize(&repo).unwrap_or(repo);
        self.repos.lock().unwrap().insert(repo);
    }

    pub fn repos(&self) -> Vec<PathBuf> {
        self.repos.lock().unwrap().iter().cloned().collect()
    }

    pub fn pool(&self) -> &P {
        &self.pool
    }

    async fn hand_out(&self, request: &WorktreeRequest, holder: Holder) -> Result<Worktree> {
        let primary = checks::assert_primary(&request.repo).await?;
        let branch = (!request.branch.is_empty()).then_some(request.branch.as_str());
        let label = holder_label(&request.project, &holder);
        let info = self
            .pool
            .acquire(&primary.top, &label, branch, request.base.as_deref())
            .await?;

        if let Err(e) = self.verify(&info, &primary, branch).await {
            self.give_back(&primary.top, &info).await;
            return Err(e);
        }
        self.add_repo(&primary.top);

        let wt = Worktree {
            id: info.lease_id.clone(),
            path: std::fs::canonicalize(&info.path).unwrap_or(info.path.clone()),
            repo: primary.top.clone(),
            branch: request.branch.clone(),
            holder: holder.clone(),
        };
        let change = match holder {
            Holder::Task { .. } => WorktreeEvent::HandedOut {
                worktree: wt.clone(),
            },
            Holder::Lease { .. } => WorktreeEvent::Leased {
                worktree: wt.clone(),
            },
        };
        if let Err(e) = self
            .record(&request.project, task_of(&wt.holder), &change)
            .await
        {
            self.give_back(&primary.top, &info).await;
            return Err(e);
        }
        Ok(wt)
    }

    async fn verify(
        &self,
        info: &LeaseInfo,
        primary: &checks::Checkout,
        branch: Option<&str>,
    ) -> Result<()> {
        let wt = checks::assert_isolated(&info.path, primary).await?;
        if let Some(want) = branch {
            let have =
                crate::git::try_read(&wt.top, ["symbolic-ref", "--quiet", "--short", "HEAD"])
                    .await?
                    .unwrap_or_default();
            if have != want {
                return Err(CoreError::Refused(format!(
                    "{} is on {:?}, not the requested branch {want:?}",
                    info.path.display(),
                    if have.is_empty() {
                        "a detached HEAD"
                    } else {
                        &have
                    }
                )));
            }
        }
        Ok(())
    }

    /// Release a lease we just took and must not hand out. A fresh worktree
    /// holds no work, and the release still refuses to discard any.
    async fn give_back(&self, repo: &Path, info: &LeaseInfo) {
        let _ = self.pool.release(repo, &info.path, &info.lease_id).await;
    }

    async fn record(
        &self,
        project: &ProjectId,
        task: Option<TaskId>,
        change: &WorktreeEvent,
    ) -> Result<()> {
        let Some((log, host)) = &self.events else {
            return Ok(());
        };
        let event = NewEvent::typed(host.clone(), project.clone(), task, kinds::WORKTREE, change)?;
        log.append(event).await.map(|_| ())
    }

    async fn find(&self, id: &str) -> Result<(PathBuf, PoolEntry)> {
        for repo in self.repos() {
            if let Some(entry) = self
                .pool
                .list(&repo)
                .await?
                .into_iter()
                .find(|e| e.lease_id == id)
            {
                return Ok((repo, entry));
            }
        }
        Err(CoreError::NotFound(format!("worktree {id}")))
    }
}

#[async_trait]
impl<P: Pool> WorktreeProvider for TreehouseProvider<P> {
    async fn get(&self, request: &WorktreeRequest) -> Result<Worktree> {
        self.hand_out(
            request,
            Holder::Task {
                task: request.task.clone(),
            },
        )
        .await
    }

    async fn return_worktree(&self, id: &str) -> Result<ReturnOutcome> {
        let (repo, entry) = self.find(id).await?;
        let (project, holder) = parse_holder(&entry.lease_holder).unwrap_or_else(|| {
            (
                ProjectId::engine(),
                Holder::Lease {
                    owner: entry.lease_holder.clone(),
                },
            )
        });
        let task = task_of(&holder);

        let work = checks::landed_work(&entry.path).await?;
        let outcome = if !work.is_landed() {
            ReturnOutcome::Kept { work }
        } else {
            match self.pool.release(&repo, &entry.path, id).await? {
                Release::Returned => ReturnOutcome::Returned,
                Release::NotReturned(why) => {
                    // Work appeared between our check and the pool's.
                    let work = checks::landed_work(&entry.path).await?;
                    if work.is_landed() {
                        return Err(CoreError::Backend(format!(
                            "pool kept worktree {id}: {why}"
                        )));
                    }
                    ReturnOutcome::Kept { work }
                }
            }
        };
        self.record(
            &project,
            task,
            &WorktreeEvent::Returned {
                id: id.to_string(),
                outcome: outcome.clone(),
            },
        )
        .await?;
        Ok(outcome)
    }

    async fn lease(&self, request: &WorktreeRequest, owner: &str) -> Result<Worktree> {
        if owner.is_empty() {
            return Err(CoreError::Invalid("a lease needs an owner".into()));
        }
        self.hand_out(
            request,
            Holder::Lease {
                owner: owner.to_string(),
            },
        )
        .await
    }

    async fn status(&self) -> Result<ProviderStatus> {
        let mut slots = Vec::new();
        for repo in self.repos() {
            for entry in self.pool.list(&repo).await? {
                slots.push(slot(&repo, entry));
            }
        }
        Ok(ProviderStatus { slots })
    }
}

fn task_of(holder: &Holder) -> Option<TaskId> {
    match holder {
        Holder::Task { task } => Some(task.clone()),
        Holder::Lease { .. } => None,
    }
}

fn slot(repo: &Path, entry: PoolEntry) -> Slot {
    let holder = match parse_holder(&entry.lease_holder) {
        Some((_, h)) => Some(h),
        None if !entry.lease_holder.is_empty() => Some(Holder::Lease {
            owner: entry.lease_holder.clone(),
        }),
        None => None,
    };
    let state = if !entry.recovery_reason.is_empty() {
        SlotState::Quarantined
    } else {
        match entry.status.as_str() {
            "available" => SlotState::Idle,
            "in-use" | "in use" | "you're here" => SlotState::InUse,
            "dirty" => SlotState::Dirty,
            "leased" if matches!(holder, Some(Holder::Task { .. })) => SlotState::InUse,
            "leased" => SlotState::Leased,
            // damaged, unverified, and anything this version does not know.
            _ => SlotState::Quarantined,
        }
    };
    Slot {
        path: entry.path,
        repo: repo.to_path_buf(),
        state,
        holder,
    }
}

/// The lease holder recorded with the pool: `quark:task:<project>:<task>` or
/// `quark:lease:<project>:<owner>`, with `%` and `:` escaped in each part.
pub fn holder_label(project: &ProjectId, holder: &Holder) -> String {
    match holder {
        Holder::Task { task } => format!(
            "{TASK_PREFIX}{}:{}",
            escape(project.as_str()),
            escape(task.as_str())
        ),
        Holder::Lease { owner } => {
            format!(
                "{LEASE_PREFIX}{}:{}",
                escape(project.as_str()),
                escape(owner)
            )
        }
    }
}

/// The Project and holder a [`holder_label`] names; `None` for a lease
/// someone else took.
pub fn parse_holder(label: &str) -> Option<(ProjectId, Holder)> {
    let (rest, is_task) = if let Some(rest) = label.strip_prefix(TASK_PREFIX) {
        (rest, true)
    } else {
        (label.strip_prefix(LEASE_PREFIX)?, false)
    };
    let (project, name) = rest.split_once(':')?;
    let project = ProjectId::new(unescape(project)?);
    let name = unescape(name)?;
    let holder = if is_task {
        Holder::Task {
            task: TaskId::new(name),
        }
    } else {
        Holder::Lease { owner: name }
    };
    Some((project, holder))
}

fn escape(s: &str) -> String {
    s.replace('%', "%25").replace(':', "%3A")
}

fn unescape(s: &str) -> Option<String> {
    if s.contains(':') {
        return None;
    }
    Some(s.replace("%3A", ":").replace("%25", "%"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holder_labels_round_trip() {
        let p = ProjectId::new("quark:mvp%1");
        for h in [
            Holder::Task { task: "t:1".into() },
            Holder::Lease {
                owner: "sub-coordinator".into(),
            },
        ] {
            let label = holder_label(&p, &h);
            assert_eq!(parse_holder(&label), Some((p.clone(), h)));
        }
        assert_eq!(
            holder_label(&"p".into(), &Holder::Task { task: "t".into() }),
            "quark:task:p:t"
        );
        assert_eq!(parse_holder("firstmate-home"), None);
        assert_eq!(parse_holder("quark:task:no-separator"), None);
    }

    fn entry(status: &str, holder: &str, recovery: &str) -> PoolEntry {
        PoolEntry {
            name: "1".into(),
            path: "/p/1/r".into(),
            status: status.into(),
            branch: String::new(),
            detached: false,
            recovery_reason: recovery.into(),
            lease_id: String::new(),
            lease_holder: holder.into(),
        }
    }

    #[test]
    fn slot_states() {
        let r = Path::new("/r");
        let state = |s, h, rr| slot(r, entry(s, h, rr)).state;
        assert_eq!(state("available", "", ""), SlotState::Idle);
        assert_eq!(state("in-use", "", ""), SlotState::InUse);
        assert_eq!(state("you're here", "", ""), SlotState::InUse);
        assert_eq!(state("dirty", "", ""), SlotState::Dirty);
        assert_eq!(state("leased", "quark:task:p:t", ""), SlotState::InUse);
        assert_eq!(state("leased", "quark:lease:p:home", ""), SlotState::Leased);
        assert_eq!(state("leased", "firstmate", ""), SlotState::Leased);
        assert_eq!(state("leased", "x", "state lost"), SlotState::Quarantined);
        assert_eq!(state("damaged", "", ""), SlotState::Quarantined);
        assert_eq!(state("unverified", "", ""), SlotState::Quarantined);
        assert_eq!(
            slot(r, entry("leased", "firstmate", "")).holder,
            Some(Holder::Lease {
                owner: "firstmate".into()
            })
        );
    }
}
