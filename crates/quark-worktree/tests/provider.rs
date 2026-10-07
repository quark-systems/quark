//! The provider against real git repositories, with a git-backed pool that
//! behaves like treehouse (and can be told to misbehave).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use quark_core::event::kinds;
use quark_core::fake::MemoryEventLog;
use quark_core::worktree::{
    Holder, LandedWork, ReturnOutcome, SlotState, WorktreeEvent, WorktreeProvider, WorktreeRequest,
};
use quark_core::{CoreError, HostId, Result};
use quark_worktree::checks;
use quark_worktree::{LeaseInfo, Pool, PoolEntry, Release, TreehouseCli, TreehouseProvider};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A bare origin and a clone of it with one commit on `main`.
struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        git(&root, &["init", "-q", "--bare", "origin.git"]);
        git(&root, &["clone", "-q", "origin.git", "repo"]);
        let repo = root.join("repo");
        std::fs::write(repo.join("README"), "hello\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        git(&repo, &["push", "-q", "origin", "HEAD:main"]);
        git(&repo, &["remote", "set-head", "origin", "main"]);
        Self {
            _tmp: tmp,
            root,
            repo,
        }
    }

    fn request(&self, branch: &str) -> WorktreeRequest {
        WorktreeRequest {
            project: "quark".into(),
            task: "t1".into(),
            repo: self.repo.clone(),
            branch: branch.into(),
            base: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Fault {
    None,
    HandOutPrimary,
    IgnoreBranch,
}

/// Leases `git worktree`s under `<root>/pool/<n>/repo`, keeps their state in
/// memory, and refuses to release a dirty one, like treehouse.
struct GitPool {
    root: PathBuf,
    entries: Mutex<Vec<PoolEntry>>,
    fault: Mutex<Fault>,
    released: Mutex<Vec<String>>,
}

impl GitPool {
    fn new(root: &Path) -> Self {
        Self {
            root: root.join("pool"),
            entries: Mutex::default(),
            fault: Mutex::new(Fault::None),
            released: Mutex::default(),
        }
    }
}

#[async_trait]
impl Pool for GitPool {
    async fn acquire(
        &self,
        repo: &Path,
        holder: &str,
        branch: Option<&str>,
        base: Option<&str>,
    ) -> Result<LeaseInfo> {
        let fault = *self.fault.lock().unwrap();
        let n = self.entries.lock().unwrap().len() + 1;
        let lease_id = format!("L{n}");
        let path = if fault == Fault::HandOutPrimary {
            repo.to_path_buf()
        } else {
            let path = self.root.join(n.to_string()).join("repo");
            let path_s = path.to_string_lossy().into_owned();
            let start = base.unwrap_or("HEAD");
            match branch {
                Some(b) if fault != Fault::IgnoreBranch => {
                    git(repo, &["worktree", "add", "-q", "-b", b, &path_s, start])
                }
                _ => git(repo, &["worktree", "add", "-q", "--detach", &path_s, start]),
            };
            path
        };
        self.entries.lock().unwrap().push(PoolEntry {
            name: n.to_string(),
            path: path.clone(),
            status: "leased".into(),
            branch: branch.unwrap_or_default().into(),
            detached: branch.is_none(),
            recovery_reason: String::new(),
            lease_id: lease_id.clone(),
            lease_holder: holder.into(),
        });
        Ok(LeaseInfo {
            path,
            lease_id,
            lease_holder: holder.into(),
            base_branch: "main".into(),
        })
    }

    async fn release(&self, _repo: &Path, path: &Path, lease_id: &str) -> Result<Release> {
        self.released.lock().unwrap().push(lease_id.to_string());
        let mut entries = self.entries.lock().unwrap();
        let Some(e) = entries
            .iter_mut()
            .find(|e| e.path == path && e.lease_id == lease_id)
        else {
            return Err(CoreError::Backend("lease changed".into()));
        };
        if !git(path, &["status", "--porcelain", "--untracked-files=all"]).is_empty() {
            return Ok(Release::NotReturned("uncommitted changes".into()));
        }
        if e.path.join(".git").is_file() {
            git(path, &["checkout", "-q", "--detach"]);
        }
        e.status = "available".into();
        e.lease_id.clear();
        e.lease_holder.clear();
        Ok(Release::Returned)
    }

    async fn list(&self, _repo: &Path) -> Result<Vec<PoolEntry>> {
        Ok(self.entries.lock().unwrap().clone())
    }
}

fn provider(fx: &Fixture) -> (TreehouseProvider<GitPool>, MemoryEventLog) {
    let log = MemoryEventLog::new();
    let p = TreehouseProvider::new(GitPool::new(&fx.root))
        .with_events(Arc::new(log.clone()), HostId::new("mac"));
    (p, log)
}

fn changes(log: &MemoryEventLog) -> Vec<WorktreeEvent> {
    log.events()
        .iter()
        .inspect(|e| assert_eq!(e.kind.as_str(), kinds::WORKTREE))
        .map(|e| e.decode().unwrap())
        .collect()
}

#[tokio::test]
async fn hands_out_an_isolated_worktree_and_takes_it_back() {
    let fx = Fixture::new();
    let (p, log) = provider(&fx);
    let wt = p.get(&fx.request("feat")).await.unwrap();

    assert_ne!(wt.path, fx.repo);
    assert_eq!(wt.repo, fx.repo);
    assert_eq!(wt.branch, "feat");
    assert_eq!(git(&wt.path, &["symbolic-ref", "--short", "HEAD"]), "feat");
    assert_eq!(wt.holder, Holder::Task { task: "t1".into() });

    let status = p.status().await.unwrap();
    assert_eq!(status.slots.len(), 1);
    assert_eq!(status.slots[0].state, SlotState::InUse);
    assert_eq!(status.slots[0].holder, Some(wt.holder.clone()));

    assert_eq!(
        p.return_worktree(&wt.id).await.unwrap(),
        ReturnOutcome::Returned
    );
    assert_eq!(p.status().await.unwrap().slots[0].state, SlotState::Idle);

    let ev = changes(&log);
    assert_eq!(ev.len(), 2);
    assert_eq!(
        ev[0],
        WorktreeEvent::HandedOut {
            worktree: wt.clone()
        }
    );
    assert_eq!(
        ev[1],
        WorktreeEvent::Returned {
            id: wt.id.clone(),
            outcome: ReturnOutcome::Returned
        }
    );
    assert_eq!(log.events()[0].task, Some("t1".into()));
    assert_eq!(log.events()[1].project, "quark".into());
}

#[tokio::test]
async fn keeps_uncommitted_work() {
    let fx = Fixture::new();
    let (p, log) = provider(&fx);
    let wt = p.get(&fx.request("feat")).await.unwrap();
    std::fs::write(wt.path.join("new.txt"), "x").unwrap();

    let out = p.return_worktree(&wt.id).await.unwrap();
    assert_eq!(
        out,
        ReturnOutcome::Kept {
            work: LandedWork {
                uncommitted: true,
                unpushed_commits: 0
            }
        }
    );
    assert!(wt.path.join("new.txt").exists());
    assert!(p.pool().released.lock().unwrap().is_empty());
    assert!(matches!(
        changes(&log)[1],
        WorktreeEvent::Returned {
            outcome: ReturnOutcome::Kept { .. },
            ..
        }
    ));

    std::fs::remove_file(wt.path.join("new.txt")).unwrap();
    assert_eq!(
        p.return_worktree(&wt.id).await.unwrap(),
        ReturnOutcome::Returned
    );
}

#[tokio::test]
async fn keeps_unpushed_commits_until_they_land() {
    let fx = Fixture::new();
    let (p, _) = provider(&fx);
    let wt = p.get(&fx.request("feat")).await.unwrap();
    std::fs::write(wt.path.join("a.txt"), "a").unwrap();
    git(&wt.path, &["add", "."]);
    git(&wt.path, &["commit", "-q", "-m", "a"]);

    assert_eq!(
        p.return_worktree(&wt.id).await.unwrap(),
        ReturnOutcome::Kept {
            work: LandedWork {
                uncommitted: false,
                unpushed_commits: 1
            }
        }
    );

    git(&wt.path, &["push", "-q", "origin", "feat"]);
    assert_eq!(
        p.return_worktree(&wt.id).await.unwrap(),
        ReturnOutcome::Returned
    );
}

#[tokio::test]
async fn squash_merged_work_counts_as_landed() {
    let fx = Fixture::new();
    let (p, _) = provider(&fx);
    let wt = p.get(&fx.request("feat")).await.unwrap();
    std::fs::write(wt.path.join("a.txt"), "a").unwrap();
    git(&wt.path, &["add", "."]);
    git(&wt.path, &["commit", "-q", "-m", "a1"]);
    std::fs::write(wt.path.join("b.txt"), "b").unwrap();
    git(&wt.path, &["add", "."]);
    git(&wt.path, &["commit", "-q", "-m", "a2"]);

    // The same change lands on main as one different commit.
    std::fs::write(fx.repo.join("a.txt"), "a").unwrap();
    std::fs::write(fx.repo.join("b.txt"), "b").unwrap();
    git(&fx.repo, &["add", "."]);
    git(&fx.repo, &["commit", "-q", "-m", "squash"]);
    git(&fx.repo, &["push", "-q", "origin", "HEAD:main"]);

    let work = checks::landed_work(&wt.path).await.unwrap();
    assert!(work.is_landed(), "{work:?}");
    assert_eq!(
        p.return_worktree(&wt.id).await.unwrap(),
        ReturnOutcome::Returned
    );
}

#[tokio::test]
async fn refuses_a_linked_worktree_as_the_repo() {
    let fx = Fixture::new();
    let (p, _) = provider(&fx);
    let wt = p.get(&fx.request("feat")).await.unwrap();
    let mut req = fx.request("other");
    req.repo = wt.path.clone();
    assert!(matches!(p.get(&req).await, Err(CoreError::Refused(_))));

    req.repo = fx.repo.join("sub");
    std::fs::create_dir(&req.repo).unwrap();
    assert!(matches!(p.get(&req).await, Err(CoreError::Refused(_))));
}

#[tokio::test]
async fn refuses_and_gives_back_the_primary_checkout() {
    let fx = Fixture::new();
    let (p, log) = provider(&fx);
    *p.pool().fault.lock().unwrap() = Fault::HandOutPrimary;
    let err = p.get(&fx.request("feat")).await.unwrap_err();
    assert!(
        matches!(&err, CoreError::Refused(m) if m.contains("primary")),
        "{err}"
    );
    assert_eq!(*p.pool().released.lock().unwrap(), ["L1"]);
    assert!(log.events().is_empty());
}

#[tokio::test]
async fn refuses_a_worktree_on_the_wrong_branch() {
    let fx = Fixture::new();
    let (p, _) = provider(&fx);
    *p.pool().fault.lock().unwrap() = Fault::IgnoreBranch;
    let err = p.get(&fx.request("feat")).await.unwrap_err();
    assert!(
        matches!(&err, CoreError::Refused(m) if m.contains("feat")),
        "{err}"
    );
    assert_eq!(*p.pool().released.lock().unwrap(), ["L1"]);
}

#[tokio::test]
async fn detects_a_tangled_worktree() {
    let fx = Fixture::new();
    let a = fx.root.join("a");
    let b = fx.root.join("b");
    git(
        &fx.repo,
        &["worktree", "add", "-q", "--detach", a.to_str().unwrap()],
    );
    git(
        &fx.repo,
        &["worktree", "add", "-q", "--detach", b.to_str().unwrap()],
    );
    let primary = checks::assert_primary(&fx.repo).await.unwrap();
    checks::assert_isolated(&a, &primary).await.unwrap();

    // Point a's .git at b's git dir: work in a would move b.
    let b_git = std::fs::read_to_string(b.join(".git")).unwrap();
    std::fs::write(a.join(".git"), b_git).unwrap();
    let err = checks::assert_isolated(&a, &primary).await.unwrap_err();
    assert!(
        matches!(&err, CoreError::Refused(m) if m.contains("tangled")),
        "{err}"
    );

    assert!(matches!(
        checks::assert_isolated(&fx.repo, &primary).await,
        Err(CoreError::Refused(_))
    ));
}

#[tokio::test]
async fn finds_a_primary_checkout_on_a_feature_branch() {
    let fx = Fixture::new();
    assert_eq!(checks::primary_tangle(&fx.repo).await.unwrap(), None);
    git(&fx.repo, &["checkout", "-q", "-b", "stray"]);
    assert_eq!(
        checks::primary_tangle(&fx.repo).await.unwrap(),
        Some("stray".into())
    );
    git(&fx.repo, &["checkout", "-q", "--detach"]);
    assert_eq!(checks::primary_tangle(&fx.repo).await.unwrap(), None);
}

#[tokio::test]
async fn leases_survive_as_leased_slots() {
    let fx = Fixture::new();
    let (p, log) = provider(&fx);
    let wt = p.lease(&fx.request(""), "sub-coordinator").await.unwrap();
    assert_eq!(
        wt.holder,
        Holder::Lease {
            owner: "sub-coordinator".into()
        }
    );
    assert!(matches!(changes(&log)[0], WorktreeEvent::Leased { .. }));
    assert_eq!(log.events()[0].task, None);

    // A restarted provider finds the lease through the pool alone.
    let again = TreehouseProvider::new(GitPool {
        root: p.pool().root.clone(),
        entries: Mutex::new(p.pool().entries.lock().unwrap().clone()),
        fault: Mutex::new(Fault::None),
        released: Mutex::default(),
    });
    again.add_repo(&fx.repo);
    let slots = again.status().await.unwrap().slots;
    assert_eq!(slots[0].state, SlotState::Leased);
    assert_eq!(slots[0].holder, Some(wt.holder.clone()));
    assert_eq!(
        again.return_worktree(&wt.id).await.unwrap(),
        ReturnOutcome::Returned
    );

    assert!(matches!(
        p.lease(&fx.request(""), "").await,
        Err(CoreError::Invalid(_))
    ));
}

#[tokio::test]
async fn unknown_ids_are_not_found() {
    let fx = Fixture::new();
    let (p, _) = provider(&fx);
    p.add_repo(&fx.repo);
    assert!(matches!(
        p.return_worktree("nope").await,
        Err(CoreError::NotFound(_))
    ));
}

/// The same lifecycle against a real treehouse. Opt-in: set
/// `QUARK_TREEHOUSE_BIN` to a treehouse 3.1.2 binary.
#[tokio::test]
async fn real_treehouse_lifecycle() {
    let Ok(bin) = std::env::var("QUARK_TREEHOUSE_BIN") else {
        eprintln!("skipped: QUARK_TREEHOUSE_BIN is not set");
        return;
    };
    let fx = Fixture::new();
    let cli = TreehouseCli::new()
        .with_bin(bin)
        .with_root(fx.root.join("treehouse"));
    cli.check_version().await.unwrap();
    let log = MemoryEventLog::new();
    let p = TreehouseProvider::new(cli).with_events(Arc::new(log.clone()), HostId::new("mac"));

    let wt = p.get(&fx.request("feat")).await.unwrap();
    assert_eq!(git(&wt.path, &["symbolic-ref", "--short", "HEAD"]), "feat");
    assert_eq!(p.status().await.unwrap().slots[0].state, SlotState::InUse);

    std::fs::write(wt.path.join("x"), "x").unwrap();
    assert!(matches!(
        p.return_worktree(&wt.id).await.unwrap(),
        ReturnOutcome::Kept { .. }
    ));
    assert!(wt.path.join("x").exists());
    std::fs::remove_file(wt.path.join("x")).unwrap();
    assert_eq!(
        p.return_worktree(&wt.id).await.unwrap(),
        ReturnOutcome::Returned
    );
    assert_eq!(p.status().await.unwrap().slots[0].state, SlotState::Idle);

    let home = p.lease(&fx.request(""), "sub").await.unwrap();
    assert_eq!(
        p.status()
            .await
            .unwrap()
            .slots
            .iter()
            .filter(|s| s.state == SlotState::Leased)
            .count(),
        1
    );
    assert_eq!(
        p.return_worktree(&home.id).await.unwrap(),
        ReturnOutcome::Returned
    );
    assert_eq!(log.events().len(), 4);
}
