//! The provider over the native pool alone, against real git repositories
//! (no treehouse needed; `native_parity.rs` compares the two).

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use quark_core::worktree::{Holder, ReturnOutcome, SlotState, WorktreeProvider, WorktreeRequest};
use quark_worktree::native::state::{self, RECOVERED_HOLDER};
use quark_worktree::{AcquirePlan, NativePool, Pool, TreehouseProvider};

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
        p.return_worktree(&one.id).await.unwrap(),
        ReturnOutcome::Kept { .. }
    ));
    git(&one.path, &["add", "a"]);
    git(&one.path, &["commit", "-q", "-m", "a"]);
    git(&one.path, &["push", "-q", "origin", "HEAD:main"]);
    git(&one.path, &["push", "-q", "origin", "HEAD:feat-1"]);
    git(&fx.repo, &["fetch", "-q", "origin"]);
    assert_eq!(
        p.return_worktree(&one.id).await.unwrap(),
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
