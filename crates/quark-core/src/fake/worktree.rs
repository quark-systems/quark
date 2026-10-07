use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::worktree::{
    Holder, LandedWork, ProviderStatus, ReturnOutcome, Slot, SlotState, Worktree, WorktreeProvider,
    WorktreeRequest,
};
use crate::{CoreError, Result};

/// A [`WorktreeProvider`] that hands out paths under `root` without making
/// them, and keeps the isolation and landed-work rules.
#[derive(Debug)]
pub struct FakeWorktrees {
    root: PathBuf,
    held: Mutex<BTreeMap<String, Worktree>>,
    /// Unlanded work per worktree id; set by tests.
    work: Mutex<BTreeMap<String, LandedWork>>,
    next: Mutex<u32>,
}

impl FakeWorktrees {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            held: Mutex::default(),
            work: Mutex::default(),
            next: Mutex::new(1),
        }
    }

    /// Pretend `id` has unlanded work, so returning it keeps it.
    pub fn set_work(&self, id: &str, work: LandedWork) {
        self.work.lock().unwrap().insert(id.to_string(), work);
    }

    fn hand_out(&self, request: &WorktreeRequest, holder: Holder) -> Result<Worktree> {
        let mut next = self.next.lock().unwrap();
        let id = format!("wt-{}", *next);
        *next += 1;
        let path = self.root.join(&id);
        if path == request.repo {
            return Err(CoreError::Refused(
                "worktree is the primary checkout".into(),
            ));
        }
        let wt = Worktree {
            id: id.clone(),
            path,
            repo: request.repo.clone(),
            branch: request.branch.clone(),
            holder,
        };
        self.held.lock().unwrap().insert(id, wt.clone());
        Ok(wt)
    }
}

#[async_trait]
impl WorktreeProvider for FakeWorktrees {
    async fn get(&self, request: &WorktreeRequest) -> Result<Worktree> {
        self.hand_out(
            request,
            Holder::Task {
                task: request.task.clone(),
            },
        )
    }

    async fn return_worktree(&self, id: &str) -> Result<ReturnOutcome> {
        if !self.held.lock().unwrap().contains_key(id) {
            return Err(CoreError::NotFound(format!("worktree {id}")));
        }
        if let Some(work) = self.work.lock().unwrap().get(id).filter(|w| !w.is_landed()) {
            return Ok(ReturnOutcome::Kept { work: work.clone() });
        }
        self.held.lock().unwrap().remove(id);
        Ok(ReturnOutcome::Returned)
    }

    async fn lease(&self, request: &WorktreeRequest, owner: &str) -> Result<Worktree> {
        self.hand_out(
            request,
            Holder::Lease {
                owner: owner.to_string(),
            },
        )
    }

    async fn status(&self) -> Result<ProviderStatus> {
        let slots = self
            .held
            .lock()
            .unwrap()
            .values()
            .map(|w| Slot {
                path: w.path.clone(),
                repo: w.repo.clone(),
                state: match w.holder {
                    Holder::Task { .. } => SlotState::InUse,
                    Holder::Lease { .. } => SlotState::Leased,
                },
                holder: Some(w.holder.clone()),
            })
            .collect();
        Ok(ProviderStatus { slots })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> WorktreeRequest {
        WorktreeRequest {
            project: "p".into(),
            task: "t".into(),
            repo: "/repo".into(),
            branch: "b".into(),
            base: None,
        }
    }

    #[tokio::test]
    async fn keeps_unlanded_work() {
        let wt = FakeWorktrees::new("/pool");
        let w = wt.get(&req()).await.unwrap();
        wt.set_work(
            &w.id,
            LandedWork {
                uncommitted: true,
                unpushed_commits: 0,
            },
        );
        assert!(matches!(
            wt.return_worktree(&w.id).await.unwrap(),
            ReturnOutcome::Kept { .. }
        ));
        assert_eq!(wt.status().await.unwrap().slots.len(), 1);
        wt.set_work(
            &w.id,
            LandedWork {
                uncommitted: false,
                unpushed_commits: 0,
            },
        );
        assert_eq!(
            wt.return_worktree(&w.id).await.unwrap(),
            ReturnOutcome::Returned
        );
    }

    #[tokio::test]
    async fn refuses_primary_checkout() {
        let wt = FakeWorktrees::new("/pool");
        let mut r = req();
        r.repo = "/pool/wt-1".into();
        assert!(matches!(wt.get(&r).await, Err(CoreError::Refused(_))));
    }
}
