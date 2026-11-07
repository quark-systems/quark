//! The provider over the native pool alone, against real git repositories
//! (no treehouse needed; `native_parity.rs` compares the two).

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use quark_core::worktree::{Holder, ReturnOutcome, SlotState, WorktreeProvider, WorktreeRequest};
use quark_worktree::native::state::{self, RECOVERED_HOLDER};
use quark_worktree::{
    AcquirePlan, DestroyOptions, LeaseInfo, NativePool, Pool, Release, TreehouseProvider,
};

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

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        git(&base, &["init", "-q", "--bare", "origin.git"]);
        git(&base, &["clone", "-q", "origin.git", "repo"]);
        let repo = base.join("repo");
        std::fs::write(repo.join("README"), "hello\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        git(&repo, &["push", "-q", "origin", "HEAD:main"]);
        git(&repo, &["remote", "set-head", "origin", "main"]);
        Self {
            _tmp: tmp,
            repo,
            root: base.join("pools"),
        }
    }

    fn pool(&self) -> NativePool {
        NativePool::new().with_root(&self.root)
    }

    fn request(&self, task: &str, branch: &str) -> WorktreeRequest {
        WorktreeRequest {
            project: "quark".into(),
            task: task.into(),
            repo: self.repo.clone(),
            branch: branch.into(),
            base: None,
        }
    }
}

#[tokio::test]
async fn lifecycle() {
    let fx = Fixture::new();
    let p = TreehouseProvider::new(fx.pool());

    let one = p.get(&fx.request("t1", "feat-1")).await.unwrap();
    assert_eq!(
        git(&one.path, &["symbolic-ref", "--short", "HEAD"]),
        "feat-1"
    );
    assert_eq!(one.path.file_name().unwrap(), "repo");
    let home = p.lease(&fx.request("t2", ""), "home").await.unwrap();
    assert_ne!(home.path, one.path);
    assert_eq!(
        home.holder,
        Holder::Lease {
            owner: "home".into()
        }
    );

    // Unlanded work is kept; landed work is returned and the slot reused.
    std::fs::write(one.path.join("a"), "a\n").unwrap();
    assert!(matches!(
        p.return_worktree(&one).await.unwrap(),
        ReturnOutcome::Kept { .. }
    ));
    git(&one.path, &["add", "a"]);
    git(&one.path, &["commit", "-q", "-m", "a"]);
    git(&one.path, &["push", "-q", "origin", "HEAD:main"]);
    git(&one.path, &["push", "-q", "origin", "HEAD:feat-1"]);
    git(&fx.repo, &["fetch", "-q", "origin"]);
    assert_eq!(
        p.return_worktree(&one).await.unwrap(),
        ReturnOutcome::Returned
    );
    let status = p.status().await.unwrap();
    let freed = status.slots.iter().find(|s| s.path == one.path).unwrap();
    assert_eq!(freed.state, SlotState::Idle);

    let three = p.get(&fx.request("t3", "feat-3")).await.unwrap();
    assert_eq!(three.path, one.path, "the returned slot is reused");
    assert!(three.path.join("a").exists(), "reset to the new main");

    // The state on disk is treehouse's format, signed.
    let pool = fx.pool().pool_dir(&fx.repo).unwrap();
    let s = state::read(&pool).unwrap();
    assert_eq!(s.version, state::STATE_VERSION);
    assert!(s.worktrees.iter().all(|w| w.leased
        && w.lease_holder != RECOVERED_HOLDER
        && !w.seed_inventory_digest.is_empty()));
    assert!(pool.join("treehouse-state.key").exists());
}

#[tokio::test]
async fn full_pools_and_bad_requests_are_refused() {
    let fx = Fixture::new();
    std::fs::write(fx.repo.join("treehouse.toml"), "max_trees = 1\n").unwrap();
    std::fs::write(fx.repo.join(".git/info/exclude"), "treehouse.toml\n").unwrap();
    let pool = fx.pool();
    pool.acquire(&fx.repo, "a", None, None).await.unwrap();
    assert!(matches!(
        pool.plan_acquire(&fx.repo, None, None).unwrap(),
        AcquirePlan::Refuse { .. }
    ));
    assert!(pool.acquire(&fx.repo, "b", None, None).await.is_err());
    assert!(pool
        .acquire(&fx.repo, "b", Some("bad..name"), None)
        .await
        .is_err());
    assert!(pool
        .acquire(&fx.repo, "b", None, Some("nope"))
        .await
        .is_err());
}

#[tokio::test]
async fn unsupported_settings_are_refused_not_ignored() {
    let fx = Fixture::new();
    std::fs::write(
        fx.repo.join("treehouse.toml"),
        "worktree_path = \"{pool}/{slot}/x\"\n",
    )
    .unwrap();
    let err = fx
        .pool()
        .acquire(&fx.repo, "a", None, None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("worktree_path"), "{err}");

    let fx = Fixture::new();
    std::fs::write(fx.repo.join(".worktreeinclude"), ".env\n").unwrap();
    git(&fx.repo, &["add", ".worktreeinclude"]);
    git(&fx.repo, &["commit", "-q", "-m", "include"]);
    git(&fx.repo, &["push", "-q", "origin", "HEAD:main"]);
    let err = fx
        .pool()
        .acquire(&fx.repo, "a", None, None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains(".worktreeinclude"), "{err}");
}

#[tokio::test]
async fn discarding_drops_uncommitted_work_and_keeps_commits_on_the_branch() {
    let fx = Fixture::new();
    let p = TreehouseProvider::new(fx.pool());
    let wt = p.get(&fx.request("t1", "feat-1")).await.unwrap();
    std::fs::write(wt.path.join("kept"), "kept\n").unwrap();
    git(&wt.path, &["add", "kept"]);
    git(&wt.path, &["commit", "-q", "-m", "kept"]);
    let commit = git(&wt.path, &["rev-parse", "HEAD"]);
    std::fs::write(wt.path.join("scratch"), "untracked\n").unwrap();
    std::fs::write(wt.path.join("kept"), "edited\n").unwrap();
    assert!(matches!(
        p.return_worktree(&wt).await.unwrap(),
        ReturnOutcome::Kept { .. }
    ));

    assert_eq!(
        p.discard_worktree(&wt).await.unwrap(),
        ReturnOutcome::Returned
    );
    let status = p.status().await.unwrap();
    let freed = status.slots.iter().find(|s| s.path == wt.path).unwrap();
    assert_eq!(freed.state, SlotState::Idle);
    assert!(!wt.path.join("scratch").exists());
    assert_eq!(git(&fx.repo, &["rev-parse", "feat-1"]), commit);
}

/// Three slots of `fx.repo`: one leased, two returned (idle, landed).
async fn three_slots(pool: &NativePool, fx: &Fixture) -> (LeaseInfo, LeaseInfo, LeaseInfo) {
    let mut l = slots(pool, fx, 3).await;
    let (c, b, a) = (l.pop().unwrap(), l.pop().unwrap(), l.pop().unwrap());
    (a, b, c)
}

/// `n` slots of `fx.repo`, all but the first returned.
async fn slots(pool: &NativePool, fx: &Fixture, n: usize) -> Vec<LeaseInfo> {
    let mut out = Vec::new();
    for i in 0..n {
        out.push(
            pool.acquire(&fx.repo, &format!("h{i}"), None, None)
                .await
                .unwrap(),
        );
    }
    for l in &out[1..] {
        assert_eq!(
            pool.release(&fx.repo, &l.path, &l.lease_id).await.unwrap(),
            Release::Returned
        );
    }
    out
}

fn listed(pool: &NativePool, fx: &Fixture) -> Vec<PathBuf> {
    pool.list_blocking(&fx.repo, false)
        .unwrap()
        .into_iter()
        .map(|e| e.path)
        .collect()
}

#[tokio::test]
async fn prune_removes_only_stale_slots() {
    let fx = Fixture::new();
    let pool = fx.pool();
    let mut l = slots(&pool, &fx, 4).await;
    let (dirty, other, landed, leased) = (
        l.pop().unwrap(),
        l.pop().unwrap(),
        l.pop().unwrap(),
        l.pop().unwrap(),
    );
    // An idle slot with a commit that never landed, and an idle dirty one.
    let unmerged = other.path.clone();
    git(&unmerged, &["commit", "-q", "--allow-empty", "-m", "local"]);
    std::fs::write(dirty.path.join("scratch"), "x\n").unwrap();

    let plan = pool.prune_blocking(&fx.repo, true).unwrap();
    assert_eq!(
        plan.removed.iter().map(|r| &r.path).collect::<Vec<_>>(),
        [&landed.path]
    );
    assert!(plan.removed[0].bytes > 0);
    assert!(landed.path.exists(), "a dry run removes nothing");

    let done = pool.prune(&fx.repo).await.unwrap();
    assert_eq!(done.removed.len(), 1);
    assert!(!landed.path.exists());
    assert!(!landed.path.parent().unwrap().exists(), "slot dir goes too");
    let reasons: Vec<&str> = done.kept.iter().map(|k| k.reason.as_str()).collect();
    assert!(
        reasons.iter().any(|r| r.starts_with("HEAD not merged")),
        "{reasons:?}"
    );
    assert!(reasons.contains(&"uncommitted changes"), "{reasons:?}");
    assert!(
        !done.kept.iter().any(|k| k.path == leased.path),
        "busy is not reported"
    );

    let left = listed(&pool, &fx);
    assert!(left.contains(&leased.path) && left.contains(&unmerged) && left.contains(&dirty.path));
    assert!(!left.contains(&landed.path));
    assert!(!git(&fx.repo, &["worktree", "list"]).contains(landed.path.to_str().unwrap()));

    // The pool keeps working: the next slot is created fresh.
    let next = pool.acquire(&fx.repo, "e", None, None).await.unwrap();
    assert!(next.path.exists());
}

#[tokio::test]
async fn destroy_removes_a_disposable_slot_and_needs_opt_ins_for_the_rest() {
    let fx = Fixture::new();
    let pool = fx.pool();
    let (leased, landed, other) = three_slots(&pool, &fx).await;

    let kept = pool.destroy(&fx.repo, &leased.path).await.unwrap();
    assert!(kept.removed.is_empty());
    assert!(
        kept.kept[0].reason.contains("leased: held by h0")
            && kept.kept[0].reason.contains("--include-leased"),
        "{:?}",
        kept.kept
    );
    assert!(leased.path.exists());

    let gone = pool.destroy(&fx.repo, &landed.path).await.unwrap();
    assert_eq!(gone.removed.len(), 1);
    assert!(!landed.path.exists());

    std::fs::write(other.path.join("scratch"), "x\n").unwrap();
    let dirty = pool.destroy(&fx.repo, &other.path).await.unwrap();
    assert!(
        dirty.kept[0].reason.starts_with("dirty"),
        "{:?}",
        dirty.kept
    );
    let forced = pool
        .destroy_blocking(
            &fx.repo,
            &other.path,
            DestroyOptions {
                include_unlanded: true,
                ..Default::default()
            },
            false,
        )
        .unwrap();
    assert_eq!(forced.removed.len(), 1);
    assert!(!other.path.exists());

    let left = listed(&pool, &fx);
    assert_eq!(left, std::slice::from_ref(&leased.path));
    assert!(pool
        .destroy(&fx.repo, &landed.path)
        .await
        .unwrap_err()
        .to_string()
        .contains("not managed"));
}

#[tokio::test]
async fn retiring_a_clone_leaves_other_clones_slots() {
    let fx = Fixture::new();
    let base = fx.repo.parent().unwrap().to_path_buf();
    git(&base, &["clone", "-q", "origin.git", "second/repo"]);
    let second = base.join("second/repo");
    git(&second, &["remote", "set-head", "origin", "main"]);
    let pool = fx.pool();
    assert_eq!(
        pool.pool_dir(&fx.repo).unwrap(),
        pool.pool_dir(&second).unwrap()
    );

    let p = TreehouseProvider::new(pool.clone());
    let mine = p.get(&fx.request("t1", "")).await.unwrap();
    assert_eq!(
        p.return_worktree(&mine).await.unwrap(),
        ReturnOutcome::Returned
    );
    let theirs = pool.acquire(&second, "x", None, None).await.unwrap();
    pool.release(&second, &theirs.path, &theirs.lease_id)
        .await
        .unwrap();

    let out = p.retire_clone(&fx.repo).await.unwrap();
    assert_eq!(out.removed.len(), 1, "{out:?}");
    assert!(!mine.path.exists());
    assert!(theirs.path.exists());
    assert!(p.repos().is_empty());
}
