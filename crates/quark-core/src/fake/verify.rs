use std::sync::Mutex;

use async_trait::async_trait;

use crate::verify::{Change, MainHealth, MergeGuard, Permit, Verdict, VerifyPipeline};
use crate::{ProjectId, Result, TaskId};

/// A [`VerifyPipeline`] that returns a scripted verdict for any change,
/// stamped with the change's head (or `rebased_head` after a rebase).
#[derive(Debug)]
pub struct FakeVerify {
    pub verdict: Mutex<Verdict>,
    pub rebased_head: Mutex<Option<String>>,
}

impl FakeVerify {
    pub fn new(verdict: Verdict) -> Self {
        Self {
            verdict: Mutex::new(verdict),
            rebased_head: Mutex::new(None),
        }
    }
}

#[async_trait]
impl VerifyPipeline for FakeVerify {
    async fn verify(&self, change: &Change) -> Result<Verdict> {
        let mut v = self.verdict.lock().unwrap().clone();
        v.head = change.head.clone();
        Ok(v)
    }

    async fn rebase_and_reverify(&self, change: &Change) -> Result<Verdict> {
        let mut v = self.verdict.lock().unwrap().clone();
        v.head = self
            .rebased_head
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| change.head.clone());
        Ok(v)
    }
}

/// A [`MergeGuard`] implementing the red-main and stale-head rules over a
/// settable main health.
#[derive(Debug)]
pub struct FakeGuard {
    pub health: Mutex<MainHealth>,
    /// The task allowed to dispatch while main is red.
    pub fix_main: Mutex<Option<TaskId>>,
    /// Tasks whose change is known to turn main green.
    pub fixes_main: Mutex<Vec<TaskId>>,
}

impl Default for FakeGuard {
    fn default() -> Self {
        Self {
            health: Mutex::new(MainHealth::Green),
            fix_main: Mutex::new(None),
            fixes_main: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl MergeGuard for FakeGuard {
    async fn main_health(&self, _project: &ProjectId) -> Result<MainHealth> {
        Ok(self.health.lock().unwrap().clone())
    }

    async fn may_merge(&self, change: &Change, verdict: &Verdict) -> Result<Permit> {
        let deny = |r: &str| Ok(Permit::Deny { reason: r.into() });
        if verdict.head != change.head {
            return deny("verdict is for another head; rebase and re-verify");
        }
        if !verdict.passed() {
            return deny("gates did not pass");
        }
        match &*self.health.lock().unwrap() {
            MainHealth::Green => Ok(Permit::Allow),
            _ if self.fixes_main.lock().unwrap().contains(&change.task) => Ok(Permit::Allow),
            MainHealth::Red { check, commit } => Ok(Permit::Deny {
                reason: format!("main is red: {check} since {commit}"),
            }),
            MainHealth::Unknown { reason } => Ok(Permit::Deny {
                reason: format!("main health unknown: {reason}"),
            }),
        }
    }

    async fn may_dispatch(&self, _project: &ProjectId, task: &TaskId) -> Result<Permit> {
        match &*self.health.lock().unwrap() {
            MainHealth::Red { check, .. } => {
                let mut fix = self.fix_main.lock().unwrap();
                match &*fix {
                    Some(t) if t == task => Ok(Permit::Allow),
                    Some(_) => Ok(Permit::Deny {
                        reason: format!("main is red ({check}); dispatch paused"),
                    }),
                    None => {
                        *fix = Some(task.clone());
                        Ok(Permit::Allow)
                    }
                }
            }
            _ => Ok(Permit::Allow),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::{GateStage, StageResult};

    fn change(task: &str, head: &str) -> Change {
        Change {
            project: "p".into(),
            task: task.into(),
            pull_request: None,
            branch: "b".into(),
            head: head.into(),
        }
    }

    fn green(head: &str) -> Verdict {
        Verdict {
            head: head.into(),
            stages: vec![StageResult {
                stage: GateStage::RepoChecks,
                passed: true,
                summary: String::new(),
            }],
            conflict: None,
        }
    }

    #[tokio::test]
    async fn stale_head_and_red_main() {
        let g = FakeGuard::default();
        let c = change("t1", "abc");
        assert!(!g.may_merge(&c, &green("old")).await.unwrap().allowed());
        assert!(g.may_merge(&c, &green("abc")).await.unwrap().allowed());

        *g.health.lock().unwrap() = MainHealth::Red {
            check: "rust".into(),
            commit: "dead".into(),
        };
        assert!(!g.may_merge(&c, &green("abc")).await.unwrap().allowed());
        g.fixes_main.lock().unwrap().push("t1".into());
        assert!(g.may_merge(&c, &green("abc")).await.unwrap().allowed());

        let p = ProjectId::from("p");
        assert!(g.may_dispatch(&p, &"fix".into()).await.unwrap().allowed());
        assert!(g.may_dispatch(&p, &"fix".into()).await.unwrap().allowed());
        assert!(!g.may_dispatch(&p, &"other".into()).await.unwrap().allowed());
    }
}
