//! [`NativeGuard`]: the native [`MergeGuard`], with its red-main episodes kept
//! in the event log.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use quark_core::verify::{Change, MainHealth, MergeGuard, Permit, Verdict};
use quark_core::{CoreError, EventLog, HostId, NewEvent, ProjectId, Result, Seq, TaskId};
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::forge::Forge;
use crate::health::{classify, green_checks};
use crate::kinds;
use crate::rules::{
    dispatch_decision, merge_decision, observe, Decision, DispatchFacts, Episode, Freshness,
    MainStatus, MergeFacts, Observation,
};

/// Which repository and branch a Project merges into.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    /// `owner/repo` on GitHub.
    pub repo: String,
    /// The default branch, usually `main`.
    pub branch: String,
}

/// Payload of a [`kinds::RED_MAIN`] event: an episode's state after a change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpisodeChange {
    pub repo: String,
    pub branch: String,
    /// `None` once the episode closed.
    pub episode: Option<Episode>,
    /// `opened`, `updated`, `closed`, `decision`, `fix_main`.
    pub change: String,
}

/// Payload of a [`kinds::OVERRIDE`] event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Override {
    pub decision: String,
}

/// Payload of a [`kinds::DECISION`] event: one native guard answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuardDecision {
    /// `merge` or `dispatch`.
    pub op: String,
    pub repo: String,
    pub branch: String,
    pub head: Option<String>,
    pub pull_request: Option<String>,
    pub main: MainStatus,
    pub decision: Decision,
}

/// The native merge and dispatch guard.
///
/// Episodes are appended to the event log as [`kinds::RED_MAIN`] events
/// before a decision that depends on them returns, and [`NativeGuard::restore`]
/// rebuilds them by replay, so a daemon restart keeps an open episode, its
/// decision and its fix-main task.
///
/// Filing the episode's decision for a person is the caller's job: watch for
/// an `opened` change, file it, and report the id with
/// [`NativeGuard::decision_filed`]; the answer comes back through
/// [`NativeGuard::answer`].
pub struct NativeGuard {
    forge: Arc<dyn Forge>,
    log: Arc<dyn EventLog>,
    host: HostId,
    targets: Mutex<HashMap<ProjectId, Target>>,
    episodes: Mutex<HashMap<(String, String), Episode>>,
    answered: Mutex<HashSet<String>>,
    fix_main: Mutex<HashSet<TaskId>>,
    live: Mutex<HashSet<TaskId>>,
    /// Serializes read-advance-record so two callers never open one episode
    /// twice.
    turn: tokio::sync::Mutex<()>,
}

fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

impl NativeGuard {
    pub fn new(forge: Arc<dyn Forge>, log: Arc<dyn EventLog>, host: HostId) -> Self {
        Self {
            forge,
            log,
            host,
            targets: Mutex::default(),
            episodes: Mutex::default(),
            answered: Mutex::default(),
            fix_main: Mutex::default(),
            live: Mutex::default(),
            turn: tokio::sync::Mutex::new(()),
        }
    }

    /// Rebuild episodes and answered decisions from the log.
    pub async fn restore(&self) -> Result<()> {
        let mut after = Seq::ZERO;
        loop {
            let batch = self.log.read(after, 500).await?;
            let Some(last) = batch.last() else { break };
            after = last.seq;
            for e in &batch {
                match e.kind.as_str() {
                    kinds::RED_MAIN => {
                        let c: EpisodeChange = e.decode()?;
                        let mut eps = self.episodes.lock().unwrap();
                        match c.episode {
                            Some(ep) => {
                                eps.insert((c.repo, c.branch), ep);
                            }
                            None => {
                                eps.remove(&(c.repo, c.branch));
                            }
                        }
                    }
                    kinds::OVERRIDE => {
                        let o: Override = e.decode()?;
                        self.answered.lock().unwrap().insert(o.decision);
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    pub fn set_target(&self, project: ProjectId, target: Target) {
        self.targets.lock().unwrap().insert(project, target);
    }

    fn target(&self, project: &ProjectId) -> Result<Target> {
        self.targets
            .lock()
            .unwrap()
            .get(project)
            .cloned()
            .ok_or_else(|| CoreError::NotFound(format!("merge target for project {project}")))
    }

    /// The open episode for `project`, if any.
    pub fn episode(&self, project: &ProjectId) -> Option<Episode> {
        let t = self.target(project).ok()?;
        self.episodes
            .lock()
            .unwrap()
            .get(&(t.repo, t.branch))
            .cloned()
    }

    /// Record the decision filed for the open episode.
    pub async fn decision_filed(&self, project: &ProjectId, decision: &str) -> Result<()> {
        let _turn = self.turn.lock().await;
        let t = self.target(project)?;
        let Some(mut ep) = self.episode(project) else {
            return Err(CoreError::Invalid(format!(
                "{} has no red-main episode",
                t.repo
            )));
        };
        ep.decision = Some(decision.to_string());
        self.save(project, &t, Some(ep), "decision").await
    }

    /// A person answered `decision`: the guardrail is overridden until main
    /// is green again.
    pub async fn answer(&self, decision: &str) -> Result<()> {
        let event = NewEvent::typed(
            self.host.clone(),
            ProjectId::engine(),
            None,
            kinds::OVERRIDE,
            &Override {
                decision: decision.to_string(),
            },
        )?;
        self.log.append(event).await?;
        self.answered.lock().unwrap().insert(decision.to_string());
        Ok(())
    }

    /// Mark `task` as dispatched with the fix-main flag.
    pub fn declare_fix_main(&self, task: TaskId) {
        self.fix_main.lock().unwrap().insert(task);
    }

    /// `task` finished; a fix-main slot it held is free again.
    pub fn task_ended(&self, task: &TaskId) {
        self.live.lock().unwrap().remove(task);
        self.fix_main.lock().unwrap().remove(task);
    }

    async fn save(
        &self,
        project: &ProjectId,
        t: &Target,
        episode: Option<Episode>,
        change: &str,
    ) -> Result<()> {
        let payload = EpisodeChange {
            repo: t.repo.clone(),
            branch: t.branch.clone(),
            episode: episode.clone(),
            change: change.into(),
        };
        let event = NewEvent::typed(
            self.host.clone(),
            project.clone(),
            None,
            kinds::RED_MAIN,
            &payload,
        )?;
        self.log.append(event).await?;
        let key = (t.repo.clone(), t.branch.clone());
        let mut eps = self.episodes.lock().unwrap();
        match episode {
            Some(e) => {
                eps.insert(key, e);
            }
            None => {
                eps.remove(&key);
            }
        }
        Ok(())
    }

    /// Read main and advance its episode. Call with `turn` held.
    async fn observe_locked(
        &self,
        project: &ProjectId,
        t: &Target,
    ) -> Result<(Observation, Option<String>)> {
        let read = async {
            let tip = self.forge.branch_tip(&t.repo, &t.branch).await?;
            let (runs, statuses) = self.forge.checks(&t.repo, &tip).await?;
            Ok::<_, CoreError>(classify(&tip, &runs, &statuses))
        }
        .await;
        let read = match read {
            Ok(r) => Some(r),
            Err(e) => {
                tracing::warn!(repo = %t.repo, error = %e, "main health unreadable");
                None
            }
        };
        let prev = self
            .episodes
            .lock()
            .unwrap()
            .get(&(t.repo.clone(), t.branch.clone()))
            .cloned();
        let answered = self.answered.lock().unwrap().clone();
        let o = observe(prev.clone(), read.as_ref(), &now(), |d| {
            answered.contains(d)
        });
        if o.episode != prev {
            let change = if o.opened {
                "opened"
            } else if o.closed {
                "closed"
            } else {
                "updated"
            };
            self.save(project, t, o.episode.clone(), change).await?;
        }
        Ok((o, read.map(|r| r.tip)))
    }

    async fn record(&self, project: &ProjectId, task: Option<&TaskId>, d: &GuardDecision) {
        let event = NewEvent::typed(
            self.host.clone(),
            project.clone(),
            task.cloned(),
            kinds::DECISION,
            d,
        );
        let result = match event {
            Ok(e) => self.log.append(e).await.map(|_| ()),
            Err(e) => Err(e),
        };
        if let Err(e) = result {
            tracing::warn!(error = %e, "could not record a guard decision");
        }
    }
}

#[async_trait]
impl MergeGuard for NativeGuard {
    async fn main_health(&self, project: &ProjectId) -> Result<MainHealth> {
        let t = self.target(project)?;
        let _turn = self.turn.lock().await;
        let (o, _) = self.observe_locked(project, &t).await?;
        Ok(match (o.status, o.episode) {
            (MainStatus::Clear, _) => MainHealth::Green,
            (MainStatus::Unknown, _) => MainHealth::Unknown {
                reason: format!("could not read {} on {}", t.branch, t.repo),
            },
            (_, Some(e)) => MainHealth::Red {
                check: e.checks.join(", "),
                commit: e.commit,
            },
            (_, None) => MainHealth::Unknown {
                reason: "red without an episode".into(),
            },
        })
    }

    async fn may_merge(&self, change: &Change, verdict: &Verdict) -> Result<Permit> {
        if verdict.head != change.head {
            return Ok(Permit::Deny {
                reason: format!(
                    "the verdict is for {}, not {}; rebase and re-verify",
                    verdict.head, change.head
                ),
            });
        }
        if !verdict.passed() {
            return Ok(Permit::Deny {
                reason: verdict
                    .conflict
                    .as_ref()
                    .map(|c| format!("rebase conflict: {c}"))
                    .unwrap_or_else(|| "the gates did not pass".into()),
            });
        }
        let t = self.target(&change.project)?;
        let _turn = self.turn.lock().await;
        let (o, tip) = self.observe_locked(&change.project, &t).await?;
        let freshness = match &tip {
            Some(tip) => {
                Freshness::from_behind(self.forge.behind_by(&t.repo, tip, &change.head).await.ok())
            }
            None => Freshness::Unreadable,
        };
        let mut green = BTreeSet::new();
        if o.status == MainStatus::Red {
            if let Ok((runs, statuses)) = self.forge.checks(&t.repo, &change.head).await {
                green = green_checks(&runs, &statuses);
            }
        }
        let facts = MergeFacts {
            head: change.head.clone(),
            freshness,
            // The verdict passed on this head, which is the gate evidence.
            gates: None,
            main: o.status,
            failing: o.episode.map(|e| e.checks).unwrap_or_default(),
            green,
        };
        let d = merge_decision(&facts);
        self.record(
            &change.project,
            Some(&change.task),
            &GuardDecision {
                op: "merge".into(),
                repo: t.repo,
                branch: t.branch,
                head: Some(change.head.clone()),
                pull_request: change.pull_request.clone(),
                main: o.status,
                decision: d.clone(),
            },
        )
        .await;
        Ok(d.permit())
    }

    async fn may_dispatch(&self, project: &ProjectId, task: &TaskId) -> Result<Permit> {
        let t = self.target(project)?;
        let _turn = self.turn.lock().await;
        let (o, _) = self.observe_locked(project, &t).await?;
        let prior = o.episode.as_ref().and_then(|e| e.fix_task.clone());
        let prior_live = prior
            .as_ref()
            .is_some_and(|p| self.live.lock().unwrap().contains(&TaskId::new(p.clone())));
        let facts = DispatchFacts {
            task: task.to_string(),
            main: o.status,
            fix_main: self.fix_main.lock().unwrap().contains(task),
            prior_fix: prior,
            prior_fix_live: prior_live,
        };
        let d = dispatch_decision(&facts);
        if d.reason == crate::rules::Reason::FixMainClaimed {
            if let Some(mut ep) = o.episode.clone() {
                ep.fix_task = Some(task.to_string());
                self.save(project, &t, Some(ep), "fix_main").await?;
            }
        }
        if d.allow {
            self.live.lock().unwrap().insert(task.clone());
        }
        self.record(
            project,
            Some(task),
            &GuardDecision {
                op: "dispatch".into(),
                repo: t.repo,
                branch: t.branch,
                head: None,
                pull_request: None,
                main: o.status,
                decision: d.clone(),
            },
        )
        .await;
        Ok(d.permit())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::FakeForge;
    use crate::health::CheckRun;
    use quark_core::fake::MemoryEventLog;
    use quark_core::verify::{GateStage, StageResult};

    const RED: &str = "1111111111111111111111111111111111111111";
    const HEAD: &str = "2222222222222222222222222222222222222222";
    const FIXED: &str = "3333333333333333333333333333333333333333";

    fn setup() -> (Arc<FakeForge>, Arc<MemoryEventLog>, NativeGuard, ProjectId) {
        let forge = Arc::new(FakeForge::new());
        let log = Arc::new(MemoryEventLog::new());
        let guard = NativeGuard::new(forge.clone(), log.clone(), "h".into());
        let p = ProjectId::from("p");
        guard.set_target(
            p.clone(),
            Target {
                repo: "acme/app".into(),
                branch: "main".into(),
            },
        );
        (forge, log, guard, p)
    }

    fn change(p: &ProjectId, task: &str) -> Change {
        Change {
            project: p.clone(),
            task: task.into(),
            pull_request: Some("https://github.com/acme/app/pull/1".into()),
            branch: "b".into(),
            head: HEAD.into(),
        }
    }

    fn passed(head: &str) -> Verdict {
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
    async fn guards_merges_and_dispatch_through_a_red_episode() {
        let (forge, log, guard, p) = setup();
        forge.set_tip("acme/app", "main", RED);
        forge.set_checks(
            RED,
            vec![CheckRun::new(1, "ci", "completed", Some("success"))],
            vec![],
        );
        forge.set_behind(RED, HEAD, 0);
        let c = change(&p, "t1");

        assert!(guard.may_merge(&c, &passed(HEAD)).await.unwrap().allowed());
        assert!(!guard.may_merge(&c, &passed("old")).await.unwrap().allowed());
        forge.set_behind(RED, HEAD, 2);
        let deny = guard.may_merge(&c, &passed(HEAD)).await.unwrap();
        assert!(matches!(deny, Permit::Deny { reason } if reason.contains("stale head")));
        forge.set_behind(RED, HEAD, 0);

        // Main turns red: unrelated merges and dispatch stop.
        forge.set_checks(
            RED,
            vec![CheckRun::new(2, "ci", "completed", Some("failure"))],
            vec![],
        );
        assert_eq!(
            guard.main_health(&p).await.unwrap(),
            MainHealth::Red {
                check: "ci".into(),
                commit: RED.into()
            }
        );
        forge.set_checks(HEAD, vec![], vec![]);
        assert!(!guard.may_merge(&c, &passed(HEAD)).await.unwrap().allowed());
        assert!(!guard.may_dispatch(&p, &"w".into()).await.unwrap().allowed());
        // One fix-main task at a time.
        guard.declare_fix_main("fix-a".into());
        guard.declare_fix_main("fix-b".into());
        assert!(guard
            .may_dispatch(&p, &"fix-a".into())
            .await
            .unwrap()
            .allowed());
        assert!(!guard
            .may_dispatch(&p, &"fix-b".into())
            .await
            .unwrap()
            .allowed());
        assert_eq!(
            guard.episode(&p).unwrap().fix_task.as_deref(),
            Some("fix-a")
        );
        guard.task_ended(&"fix-a".into());
        assert!(guard
            .may_dispatch(&p, &"fix-b".into())
            .await
            .unwrap()
            .allowed());

        // A change that passes the failing check may merge.
        forge.set_checks(
            HEAD,
            vec![CheckRun::new(3, "ci", "completed", Some("success"))],
            vec![],
        );
        assert!(guard.may_merge(&c, &passed(HEAD)).await.unwrap().allowed());

        // A restarted guard keeps the episode, and the person's answer overrides it.
        guard.decision_filed(&p, "d1").await.unwrap();
        let again = NativeGuard::new(forge.clone(), log.clone(), "h".into());
        again.set_target(
            p.clone(),
            Target {
                repo: "acme/app".into(),
                branch: "main".into(),
            },
        );
        again.restore().await.unwrap();
        let ep = again.episode(&p).unwrap();
        assert_eq!(
            (ep.decision.as_deref(), ep.fix_task.as_deref()),
            (Some("d1"), Some("fix-b"))
        );
        forge.set_checks(HEAD, vec![], vec![]);
        assert!(!again.may_merge(&c, &passed(HEAD)).await.unwrap().allowed());
        again.answer("d1").await.unwrap();
        assert!(again.may_merge(&c, &passed(HEAD)).await.unwrap().allowed());

        // Green closes the episode.
        forge.set_tip("acme/app", "main", FIXED);
        forge.set_checks(
            FIXED,
            vec![CheckRun::new(4, "ci", "completed", Some("success"))],
            vec![],
        );
        assert_eq!(again.main_health(&p).await.unwrap(), MainHealth::Green);
        assert!(again.episode(&p).is_none());

        let kinds: Vec<String> = log
            .read(Seq::ZERO, 1000)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.kind.0)
            .collect();
        assert!(kinds.iter().any(|k| k == kinds::DECISION));
        assert!(kinds.iter().any(|k| k == kinds::RED_MAIN));
    }

    #[tokio::test]
    async fn unreadable_main_refuses_merges_but_not_dispatch() {
        let (_forge, _log, guard, p) = setup();
        let c = change(&p, "t1");
        assert!(!guard.may_merge(&c, &passed(HEAD)).await.unwrap().allowed());
        assert!(guard.may_dispatch(&p, &"w".into()).await.unwrap().allowed());
        assert!(matches!(
            guard.main_health(&p).await.unwrap(),
            MainHealth::Unknown { .. }
        ));
    }
}
