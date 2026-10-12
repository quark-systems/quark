//! [`GuardedMerge`]: the native guarded merge, firstmate's `fm-pr-merge.sh`.
//!
//! A merge goes through every rule in `docs/engine/verify.md`, in order:
//!
//! 1. The pull request is rebased onto its base branch's current tip and
//!    the gates run on the rebased head ([`NativePipeline`]). A head that
//!    did not contain the tip is pushed rebased and the merge is refused
//!    until the forge's checks have run on it: a stale head never merges.
//! 2. The gates must pass on exactly the head that merges.
//! 3. The pull request's own checks must be green (or absent).
//! 4. The [`NativeGuard`] re-reads main and decides: fresh head, and while
//!    main is red only a change that passes every failing check, unless a
//!    person overrode the episode.
//! 5. The forge merges only that head; a head that moved since is refused
//!    by the forge itself.
//!
//! Standing approval (firstmate's per-repo yolo posture) is kept here too,
//! as [`kinds::STANDING_APPROVAL`] events.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use quark_core::verify::{Change, MergeGuard, Permit, Verdict, VerifyPipeline};
use quark_core::{EventLog, HostId, NewEvent, ProjectId, Result, Seq, TaskId};
use serde::{Deserialize, Serialize};

use crate::forge::{parse_pull_url, Forge, MergeMethod, PullState};
use crate::guard::{NativeGuard, Target};
use crate::health::{classify, HealthState};
use crate::kinds;
use crate::pipeline::{NativePipeline, PipelineTarget, RepoGates};

/// One merge to attempt.
#[derive(Debug, Clone)]
pub struct LandRequest {
    pub project: ProjectId,
    pub task: TaskId,
    /// The GitHub pull request URL.
    pub pull_request: String,
    pub method: MergeMethod,
    /// A clone of the repo with an `origin` remote; never changed.
    pub checkout: PathBuf,
    /// The repo name the gates config is keyed by.
    pub repo_name: String,
    pub gates: RepoGates,
    /// Where the gates run and leave their logs.
    pub scratch: PathBuf,
}

/// How an attempt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Merged {
        head: String,
    },
    /// The forge already reports it merged; nothing was run.
    AlreadyMerged,
    /// A rule refused it, with the reason a person reads.
    Refused {
        reason: String,
    },
}

/// An attempt's outcome, with the gates' verdict when they ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Landing {
    pub verdict: Option<Verdict>,
    pub outcome: Outcome,
}

impl Landing {
    fn refused(verdict: Option<Verdict>, reason: impl Into<String>) -> Self {
        Self {
            verdict,
            outcome: Outcome::Refused {
                reason: reason.into(),
            },
        }
    }
}

/// Payload of a [`kinds::MERGED`] event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Merged {
    pub pull_request: String,
    pub repo: String,
    pub head: String,
    pub method: MergeMethod,
}

/// Payload of a [`kinds::STANDING_APPROVAL`] event: whether green pull
/// requests for `repo` merge without asking a person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StandingApproval {
    pub repo: String,
    pub on: bool,
}

/// The native guarded merge. One per daemon: its guard keeps red-main
/// episodes, restored from the log on first use.
pub struct GuardedMerge {
    forge: Arc<dyn Forge>,
    log: Arc<dyn EventLog>,
    host: HostId,
    guard: NativeGuard,
    restored: tokio::sync::OnceCell<()>,
    /// The guard's target is per Project, so one Project's merges take
    /// turns from setting it to merging. The gates run outside the turn.
    turns: Mutex<HashMap<ProjectId, Arc<tokio::sync::Mutex<()>>>>,
}

impl GuardedMerge {
    pub fn new(forge: Arc<dyn Forge>, log: Arc<dyn EventLog>, host: HostId) -> Self {
        Self {
            guard: NativeGuard::new(forge.clone(), log.clone(), host.clone()),
            forge,
            log,
            host,
            restored: tokio::sync::OnceCell::new(),
            turns: Mutex::default(),
        }
    }

    pub fn guard(&self) -> &NativeGuard {
        &self.guard
    }

    async fn restore(&self) -> Result<()> {
        self.restored
            .get_or_try_init(|| self.guard.restore())
            .await
            .map(drop)
    }

    fn turn(&self, project: &ProjectId) -> Arc<tokio::sync::Mutex<()>> {
        self.turns
            .lock()
            .unwrap()
            .entry(project.clone())
            .or_default()
            .clone()
    }

    /// Attempt one merge. A rule's refusal is an [`Outcome::Refused`]; an
    /// `Err` is a read or write that failed.
    pub async fn land(&self, req: &LandRequest) -> Result<Landing> {
        self.restore().await?;
        let (repo, number) = parse_pull_url(&req.pull_request)?;
        let pull = self.forge.pull_request(&repo, number).await?;
        match pull.state {
            PullState::Open => {}
            PullState::Merged => {
                return Ok(Landing {
                    verdict: None,
                    outcome: Outcome::AlreadyMerged,
                })
            }
            PullState::Closed => {
                return Ok(Landing::refused(None, "it was closed without merging"))
            }
        }
        let change = Change {
            project: req.project.clone(),
            task: req.task.clone(),
            pull_request: Some(req.pull_request.clone()),
            branch: pull.branch.clone(),
            head: pull.sha.clone(),
        };

        // Rules 1 and 2: rebase onto the current tip and verify that head.
        let pipeline = NativePipeline::new(&req.scratch);
        pipeline.set_target(
            req.project.clone(),
            PipelineTarget {
                checkout: req.checkout.clone(),
                repo_name: req.repo_name.clone(),
                base: pull.base.clone(),
                gates: req.gates.clone(),
                push_rebased: true,
            },
        );
        let verdict = pipeline.rebase_and_reverify(&change).await?;
        if let Some(c) = &verdict.conflict {
            let reason = format!("it does not rebase cleanly onto {}: {c}", pull.base);
            return Ok(Landing::refused(Some(verdict), reason));
        }
        if verdict.head != change.head {
            let reason = format!(
                "{} did not contain {}'s tip, so it was rebased and pushed as {}; {}",
                short(&change.head),
                pull.base,
                short(&verdict.head),
                if verdict.passed() {
                    "the gates passed on it, so merge again once its checks pass"
                } else {
                    "the gates did not pass on it"
                }
            );
            return Ok(Landing::refused(Some(verdict), reason));
        }
        if !verdict.passed() {
            let failed: Vec<&str> = verdict
                .stages
                .iter()
                .filter(|s| !s.passed)
                .map(|s| s.summary.as_str())
                .collect();
            let reason = format!(
                "the gates did not pass on {}: {}",
                short(&verdict.head),
                failed.join("; ")
            );
            return Ok(Landing::refused(Some(verdict), reason));
        }

        // Rule 3: the pull request's own checks.
        let (runs, statuses) = self.forge.checks(&repo, &change.head).await?;
        let own = classify(&change.head, &runs, &statuses);
        let not_green = match own.state {
            HealthState::Red => Some(format!("its checks failed: {}", own.red.join(", "))),
            HealthState::Pending => Some("its checks are still running".to_string()),
            HealthState::Green | HealthState::None => None,
        };
        if let Some(reason) = not_green {
            return Ok(Landing::refused(Some(verdict), reason));
        }

        // Rules 4 and 5: the guard re-reads main, then the forge merges
        // exactly the verified head.
        let turn = self.turn(&req.project);
        let _turn = turn.lock().await;
        self.guard.set_target(
            req.project.clone(),
            Target {
                repo: repo.clone(),
                branch: pull.base.clone(),
            },
        );
        if let Permit::Deny { reason } = self.guard.may_merge(&change, &verdict).await? {
            return Ok(Landing::refused(Some(verdict), reason));
        }
        self.forge
            .merge(&repo, number, &change.head, req.method)
            .await?;
        let merged = Merged {
            pull_request: req.pull_request.clone(),
            repo,
            head: change.head.clone(),
            method: req.method,
        };
        let event = NewEvent::typed(
            self.host.clone(),
            req.project.clone(),
            Some(req.task.clone()),
            kinds::MERGED,
            &merged,
        )?;
        if let Err(e) = self.log.append(event).await {
            // The merge happened; losing its record must not report it failed.
            tracing::warn!(error = %e, "could not record a merge");
        }
        Ok(Landing {
            verdict: Some(verdict),
            outcome: Outcome::Merged { head: change.head },
        })
    }

    /// Record standing approval for each of `repos` in `project`.
    pub async fn set_standing_approval(
        &self,
        project: &ProjectId,
        repos: &[String],
        on: bool,
    ) -> Result<()> {
        for repo in repos {
            let event = NewEvent::typed(
                self.host.clone(),
                project.clone(),
                None,
                kinds::STANDING_APPROVAL,
                &StandingApproval {
                    repo: repo.clone(),
                    on,
                },
            )?;
            self.log.append(event).await?;
        }
        Ok(())
    }

    /// Each repo of `project` with standing approval recorded, and whether
    /// it is on, read back from the log.
    pub async fn standing_approvals(&self, project: &ProjectId) -> Result<BTreeMap<String, bool>> {
        let mut out = BTreeMap::new();
        let mut after = Seq::ZERO;
        loop {
            let batch = self.log.read(after, 500).await?;
            let Some(last) = batch.last() else { break };
            after = last.seq;
            for e in batch
                .iter()
                .filter(|e| e.kind.as_str() == kinds::STANDING_APPROVAL && &e.project == project)
            {
                let a: StandingApproval = e.decode()?;
                out.insert(a.repo, a.on);
            }
        }
        Ok(out)
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use super::*;
    use crate::forge::{FakeForge, PullHead};
    use crate::health::CheckRun;
    use crate::pipeline::CheckGate;
    use quark_core::fake::MemoryEventLog;

    const URL: &str = "https://github.com/acme/app/pull/1";

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@x")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@x")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn commit(dir: &Path, file: &str) -> String {
        std::fs::write(dir.join(file), file).unwrap();
        git(dir, &["add", file]);
        git(dir, &["commit", "-qm", file]);
        git(dir, &["rev-parse", "HEAD"])
    }

    /// A bare origin with `main`, a clone of it, and branch `b` one commit
    /// ahead, pushed. The forge reports both green and the pull open.
    struct Repo {
        dir: tempfile::TempDir,
        origin: PathBuf,
        checkout: PathBuf,
        forge: Arc<FakeForge>,
        log: Arc<MemoryEventLog>,
        merge: GuardedMerge,
    }

    impl Repo {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let origin = dir.path().join("origin.git");
            let checkout = dir.path().join("app");
            git(
                dir.path(),
                &["init", "-q", "--bare", "-b", "main", "origin.git"],
            );
            git(dir.path(), &["clone", "-q", "origin.git", "app"]);
            git(&checkout, &["checkout", "-q", "-b", "main"]);
            commit(&checkout, "README");
            git(&checkout, &["push", "-q", "origin", "main"]);
            git(&checkout, &["checkout", "-q", "-b", "b"]);
            commit(&checkout, "feature");
            git(&checkout, &["push", "-q", "origin", "b"]);
            git(&checkout, &["checkout", "-q", "main"]);
            let forge = Arc::new(FakeForge::new());
            let log = Arc::new(MemoryEventLog::new());
            let merge = GuardedMerge::new(forge.clone(), log.clone(), "h".into());
            let r = Self {
                dir,
                origin,
                checkout,
                forge,
                log,
                merge,
            };
            r.sync();
            r
        }

        fn origin_sha(&self, branch: &str) -> String {
            git(&self.origin, &["rev-parse", branch])
        }

        /// Point the forge at origin as it is now, all green.
        fn sync(&self) {
            let (main, head) = (self.origin_sha("main"), self.origin_sha("b"));
            self.forge.set_tip("acme/app", "main", &main);
            self.forge.set_checks(&main, vec![ci("success")], vec![]);
            self.forge.set_checks(&head, vec![ci("success")], vec![]);
            self.forge.set_behind(&main, &head, 0);
            self.forge.set_pull(
                "acme/app",
                1,
                PullHead {
                    sha: head,
                    branch: "b".into(),
                    base: "main".into(),
                    state: PullState::Open,
                },
            );
        }

        fn request(&self, check: &str) -> LandRequest {
            LandRequest {
                project: "p".into(),
                task: "t1".into(),
                pull_request: URL.into(),
                method: MergeMethod::Squash,
                checkout: self.checkout.clone(),
                repo_name: "app".into(),
                gates: RepoGates {
                    checks: vec![CheckGate {
                        name: "test".into(),
                        run: check.into(),
                        timeout_s: Some(30),
                    }],
                    ..RepoGates::default()
                },
                scratch: self.dir.path().join("scratch"),
            }
        }

        async fn land(&self, check: &str) -> Landing {
            self.merge.land(&self.request(check)).await.unwrap()
        }
    }

    fn ci(conclusion: &str) -> CheckRun {
        CheckRun::new(1, "ci", "completed", Some(conclusion))
    }

    fn refusal(l: &Landing) -> &str {
        match &l.outcome {
            Outcome::Refused { reason } => reason,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn merges_a_fresh_verified_green_head() {
        let r = Repo::new();
        let head = r.origin_sha("b");
        let l = r.land("test -f feature").await;
        assert_eq!(l.outcome, Outcome::Merged { head: head.clone() });
        assert!(l.verdict.unwrap().passed());
        assert_eq!(
            r.forge.merges(),
            vec![("acme/app".into(), 1, head, MergeMethod::Squash)]
        );
        let events = r.log.read(Seq::ZERO, 100).await.unwrap();
        let merged: Merged = events
            .iter()
            .find(|e| e.kind.as_str() == kinds::MERGED)
            .unwrap()
            .decode()
            .unwrap();
        assert_eq!(merged.pull_request, URL);
        // The gates logged into the scratch directory.
        let logs = crate::pipeline::logs_dir(&r.dir.path().join("scratch"));
        assert!(logs
            .join(crate::pipeline::check_log(&merged.head, "test"))
            .is_file());

        // Merged already: nothing runs again.
        let again = r.land("false").await;
        assert_eq!(
            (again.outcome, again.verdict),
            (Outcome::AlreadyMerged, None)
        );
    }

    #[tokio::test]
    async fn refuses_failed_gates_and_unfinished_checks() {
        let r = Repo::new();
        let l = r.land("exit 4").await;
        assert!(refusal(&l).contains("test: failed (exit 4)"), "{l:?}");
        assert!(!l.verdict.unwrap().passed());

        let head = r.origin_sha("b");
        r.forge.set_checks(
            &head,
            vec![CheckRun::new(1, "ci", "in_progress", None)],
            vec![],
        );
        assert!(refusal(&r.land("true").await).contains("still running"));
        r.forge.set_checks(&head, vec![ci("failure")], vec![]);
        assert!(refusal(&r.land("true").await).contains("its checks failed: ci"));
        assert!(r.forge.merges().is_empty());
    }

    #[tokio::test]
    async fn a_stale_head_is_rebased_pushed_and_refused_until_rechecked() {
        let r = Repo::new();
        let old = r.origin_sha("b");
        // Main moves on.
        commit(&r.checkout, "other");
        git(&r.checkout, &["push", "-q", "origin", "main"]);
        let main = r.origin_sha("main");
        r.forge.set_tip("acme/app", "main", &main);
        r.forge.set_checks(&main, vec![ci("success")], vec![]);
        r.forge.set_behind(&main, &old, 1);

        let l = r.land("test -f feature && test -f other").await;
        let rebased = r.origin_sha("b");
        assert_ne!(rebased, old, "the rebased head was pushed");
        assert_eq!(git(&r.origin, &["merge-base", &main, &rebased]), main);
        let reason = refusal(&l);
        assert!(reason.contains("rebased and pushed"), "{reason}");
        assert!(reason.contains("gates passed"), "{reason}");
        assert_eq!(l.verdict.unwrap().head, rebased);
        assert!(r.forge.merges().is_empty());

        // The forge sees the rebased head and its checks pass: it merges.
        r.sync();
        let l = r.land("test -f feature && test -f other").await;
        assert_eq!(l.outcome, Outcome::Merged { head: rebased });
    }

    #[tokio::test]
    async fn while_main_is_red_only_a_change_that_fixes_it_merges() {
        let r = Repo::new();
        let (main, head) = (r.origin_sha("main"), r.origin_sha("b"));
        r.forge.set_checks(&main, vec![ci("failure")], vec![]);
        // Its own checks are absent: not red, but it does not fix main.
        r.forge.set_checks(&head, vec![], vec![]);
        let l = r.land("true").await;
        assert!(refusal(&l).starts_with("main is red"), "{l:?}");
        assert!(r.merge.guard().episode(&"p".into()).is_some());

        r.forge.set_checks(&head, vec![ci("success")], vec![]);
        let l = r.land("true").await;
        assert_eq!(l.outcome, Outcome::Merged { head });
    }

    #[tokio::test]
    async fn refuses_closed_pulls_and_records_standing_approval() {
        let r = Repo::new();
        let mut pull = r.forge.pull_request("acme/app", 1).await.unwrap();
        pull.state = PullState::Closed;
        r.forge.set_pull("acme/app", 1, pull);
        assert!(refusal(&r.land("true").await).contains("closed"));

        let p = ProjectId::from("p");
        let repos = vec!["app".to_string(), "lib".to_string()];
        r.merge
            .set_standing_approval(&p, &repos, true)
            .await
            .unwrap();
        r.merge
            .set_standing_approval(&p, &repos[1..], false)
            .await
            .unwrap();
        let approvals = r.merge.standing_approvals(&p).await.unwrap();
        assert_eq!(
            approvals.into_iter().collect::<Vec<_>>(),
            vec![("app".into(), true), ("lib".into(), false)]
        );
        assert!(r
            .merge
            .standing_approvals(&"other".into())
            .await
            .unwrap()
            .is_empty());
    }
}
