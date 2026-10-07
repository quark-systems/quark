//! The native pool against the real treehouse 3.1.2 binary, on one pool:
//! each reads what the other wrote, takes over what the other leased, and
//! the shadow sees no disagreement across a whole lifecycle.
//!
//! Needs treehouse: `TREEHOUSE_BIN`, else `treehouse` on `PATH`, at 3.1.x.
//! Without it every test here passes vacuously and says so; CI installs it.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use quark_core::fake::MemoryEventLog;
use quark_core::slice::Divergence;
use quark_core::worktree::{Holder, ReturnOutcome, SlotState, WorktreeProvider, WorktreeRequest};
use quark_core::{event::kinds, HostId};
use quark_worktree::native::state::RECOVERED_HOLDER;
use quark_worktree::{
    AcquirePlan, NativePool, Pool, Release, ReleasePlan, ShadowPool, TreehouseCli,
    TreehouseProvider,
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

/// treehouse 3.1.x, or `None` (and a note) when it is not installed.
fn treehouse_bin() -> Option<PathBuf> {
    let bin = std::env::var_os("TREEHOUSE_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("treehouse"));
    let out = Command::new(&bin).arg("--version").output().ok()?;
    let v = String::from_utf8_lossy(&out.stdout);
    let v = v.trim().trim_start_matches("treehouse version ");
    if out.status.success() && v.trim_start_matches('v').starts_with("3.1.") {
        return Some(bin);
    }
    eprintln!("treehouse 3.1.x not found ({}); skipping", bin.display());
    None
}

/// A bare origin, a clone with one commit on `main`, and a pool root.
struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    root: PathBuf,
    bin: PathBuf,
}

impl Fixture {
    fn new() -> Option<Self> {
        let bin = treehouse_bin()?;
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
        let root = base.join("pools");
        Some(Self {
            _tmp: tmp,
            repo,
            root,
            bin,
        })
    }

    fn treehouse(&self) -> TreehouseCli {
        TreehouseCli::new()
            .with_bin(&self.bin)
            .with_root(&self.root)
    }

    fn native(&self) -> NativePool {
        NativePool::new().with_root(&self.root)
    }

    fn max_trees(&self, n: usize) {
        std::fs::write(
            self.repo.join("treehouse.toml"),
            format!("max_trees = {n}\n"),
        )
        .unwrap();
        // Untracked config would make the clone dirty for nothing; ignore it.
        std::fs::write(self.repo.join(".git/info/exclude"), "treehouse.toml\n").unwrap();
    }
}

const HOLDER: &str = "quark:task:p:t1";

#[tokio::test]
async fn both_find_the_same_pool() {
    let Some(fx) = Fixture::new() else { return };
    let info = fx
        .treehouse()
        .acquire(&fx.repo, HOLDER, None, None)
        .await
        .unwrap();
    let pool = fx.native().pool_dir(&fx.repo).unwrap();
    assert_eq!(
        info.path.parent().unwrap().parent().unwrap(),
        pool,
        "native resolves treehouse's pool directory"
    );
    assert_eq!(info.path.file_name().unwrap(), "repo");
}

#[tokio::test]
async fn native_reads_what_treehouse_wrote() {
    let Some(fx) = Fixture::new() else { return };
    let th = fx.treehouse();
    let leased = th
        .acquire(&fx.repo, HOLDER, Some("feat"), None)
        .await
        .unwrap();
    let spare = th.acquire(&fx.repo, "other", None, None).await.unwrap();
    assert_eq!(
        th.release(&fx.repo, &spare.path, &spare.lease_id)
            .await
            .unwrap(),
        Release::Returned
    );

    let theirs = th.list(&fx.repo).await.unwrap();
    let mine = fx.native().list_blocking(&fx.repo, false).unwrap();
    assert_eq!(mine.len(), 2);
    for (t, n) in theirs.iter().zip(&mine) {
        assert_eq!(t.path, n.path);
        assert_eq!(t.status, n.status, "{}", t.path.display());
        assert_eq!(t.branch, n.branch);
        assert_eq!(t.detached, n.detached);
        assert_eq!(t.lease_id, n.lease_id);
        assert_eq!(t.lease_holder, n.lease_holder);
    }
    let held = mine.iter().find(|e| e.path == leased.path).unwrap();
    assert_eq!(held.status, "leased");
    assert_eq!(held.lease_holder, HOLDER, "the digest authenticated");
    assert_ne!(held.lease_holder, RECOVERED_HOLDER);
    assert_eq!(held.branch, "feat");
}

#[tokio::test]
async fn treehouse_reads_and_releases_what_native_wrote() {
    let Some(fx) = Fixture::new() else { return };
    let native = fx.native();
    let a = native
        .acquire(&fx.repo, HOLDER, Some("feat"), None)
        .await
        .unwrap();
    let b = native.acquire(&fx.repo, "home", None, None).await.unwrap();
    assert_ne!(a.path, b.path);
    assert_eq!(a.base_branch, "main");
    assert_eq!(git(&a.path, &["symbolic-ref", "--short", "HEAD"]), "feat");

    let th = fx.treehouse();
    let theirs = th.list(&fx.repo).await.unwrap();
    assert_eq!(theirs.len(), 2);
    let ta = theirs.iter().find(|e| e.path == a.path).unwrap();
    assert_eq!(ta.status, "leased");
    assert_eq!(ta.lease_id, a.lease_id);
    assert_eq!(
        ta.lease_holder, HOLDER,
        "treehouse authenticated the native digest instead of quarantining"
    );

    // Treehouse releases a native lease, then reuses that slot.
    assert_eq!(
        th.release(&fx.repo, &a.path, &a.lease_id).await.unwrap(),
        Release::Returned
    );
    let again = th.acquire(&fx.repo, "next", None, None).await.unwrap();
    assert_eq!(
        again.path, a.path,
        "treehouse reuses the slot native created"
    );

    // Native releases a treehouse lease, then reuses it.
    assert_eq!(
        native
            .release(&fx.repo, &again.path, &again.lease_id)
            .await
            .unwrap(),
        Release::Returned
    );
    let mine = native.acquire(&fx.repo, "mine", None, None).await.unwrap();
    assert_eq!(mine.path, a.path);
    let listed = th.list(&fx.repo).await.unwrap();
    assert!(listed
        .iter()
        .all(|e| e.status == "leased" && e.lease_holder != RECOVERED_HOLDER));
}

#[tokio::test]
async fn both_keep_dirty_worktrees_and_refuse_stale_leases() {
    let Some(fx) = Fixture::new() else { return };
    let native = fx.native();
    let th = fx.treehouse();
    let a = th.acquire(&fx.repo, HOLDER, None, None).await.unwrap();
    std::fs::write(a.path.join("scratch"), "work\n").unwrap();

    assert!(matches!(
        native.plan_release(&fx.repo, &a.path, &a.lease_id).unwrap(),
        ReleasePlan::NotReturned { .. }
    ));
    assert!(matches!(
        native
            .release(&fx.repo, &a.path, &a.lease_id)
            .await
            .unwrap(),
        Release::NotReturned(_)
    ));
    assert!(matches!(
        th.release(&fx.repo, &a.path, &a.lease_id).await.unwrap(),
        Release::NotReturned(_)
    ));
    assert!(a.path.join("scratch").exists(), "nothing was discarded");

    assert!(matches!(
        native
            .plan_release(&fx.repo, &a.path, "not-the-lease")
            .unwrap(),
        ReleasePlan::Refuse { .. }
    ));
    assert!(native
        .release(&fx.repo, &a.path, "not-the-lease")
        .await
        .is_err());
    assert!(th
        .release(&fx.repo, &a.path, "not-the-lease")
        .await
        .is_err());
}

#[tokio::test]
async fn plans_match_what_treehouse_does() {
    let Some(fx) = Fixture::new() else { return };
    fx.max_trees(2);
    let native = fx.native();
    let th = fx.treehouse();

    let plan = native.plan_acquire(&fx.repo, None, None).unwrap();
    let a = th.acquire(&fx.repo, HOLDER, None, None).await.unwrap();
    assert_eq!(
        plan,
        AcquirePlan::Create {
            path: a.path.clone()
        }
    );

    let plan = native.plan_acquire(&fx.repo, Some("feat"), None).unwrap();
    let b = th
        .acquire(&fx.repo, HOLDER, Some("feat"), None)
        .await
        .unwrap();
    assert_eq!(
        plan,
        AcquirePlan::Create {
            path: b.path.clone()
        }
    );

    // Full: both refuse.
    assert!(matches!(
        native.plan_acquire(&fx.repo, None, None).unwrap(),
        AcquirePlan::Refuse { .. }
    ));
    assert!(th.acquire(&fx.repo, HOLDER, None, None).await.is_err());
    assert!(native.acquire(&fx.repo, HOLDER, None, None).await.is_err());

    // A slot holding a commit nobody has is never reused...
    std::fs::write(b.path.join("f"), "x\n").unwrap();
    git(&b.path, &["add", "f"]);
    git(&b.path, &["commit", "-q", "-m", "unlanded"]);
    assert_eq!(
        th.release(&fx.repo, &a.path, &a.lease_id).await.unwrap(),
        Release::Returned
    );
    assert_eq!(
        native.plan_acquire(&fx.repo, None, None).unwrap(),
        AcquirePlan::Reuse {
            path: a.path.clone()
        }
    );
    let again = th.acquire(&fx.repo, HOLDER, None, None).await.unwrap();
    assert_eq!(again.path, a.path);

    // ...and a branch that already exists is refused by both.
    assert!(matches!(
        native.plan_acquire(&fx.repo, Some("feat"), None).unwrap(),
        AcquirePlan::Refuse { .. }
    ));
    assert!(th
        .acquire(&fx.repo, HOLDER, Some("feat"), None)
        .await
        .is_err());

    // An unknown base is refused by both.
    assert!(matches!(
        native.plan_acquire(&fx.repo, None, Some("nope")).unwrap(),
        AcquirePlan::Refuse { .. }
    ));
}

/// A worktree in the pool that the state file does not list is quarantined
/// on read, then freed under the lock once proven safe, by both.
#[tokio::test]
async fn both_recover_unrecorded_worktrees() {
    let Some(fx) = Fixture::new() else { return };
    let th = fx.treehouse();
    let a = th.acquire(&fx.repo, HOLDER, None, None).await.unwrap();
    let pool = a.path.parent().unwrap().parent().unwrap().to_path_buf();
    // A worktree directory the state file does not list.
    git(
        &fx.repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            &pool.join("9/repo").to_string_lossy(),
        ],
    );
    let stray_path = pool.join("9/repo");
    let peek = fx.native().list_blocking(&fx.repo, false).unwrap();
    let stray = peek.iter().find(|e| e.path == stray_path).unwrap();
    assert_eq!(stray.status, "leased");
    assert_eq!(stray.lease_holder, RECOVERED_HOLDER);

    // Native frees it (clean, idle, HEAD on origin)...
    let mine = fx.native().list(&fx.repo).await.unwrap();
    let stray = mine.iter().find(|e| e.path == stray_path).unwrap();
    assert_eq!(stray.status, "available");
    // ...and treehouse agrees with the state native wrote.
    let theirs = th.list(&fx.repo).await.unwrap();
    let t = theirs.iter().find(|e| e.path == stray_path).unwrap();
    assert_eq!(t.status, "available");

    // One with an unpushed commit stays quarantined, with the reason.
    let other = pool.join("8/repo");
    git(
        &fx.repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            &other.to_string_lossy(),
        ],
    );
    std::fs::write(other.join("f"), "x\n").unwrap();
    git(&other, &["add", "f"]);
    git(&other, &["commit", "-q", "-m", "unpushed"]);
    let mine = fx.native().list(&fx.repo).await.unwrap();
    let kept = mine.iter().find(|e| e.path == other).unwrap();
    assert_eq!(kept.lease_holder, RECOVERED_HOLDER);
    assert!(kept.recovery_reason.contains("HEAD is not contained"));
}

/// A whole provider lifecycle through the shadow: treehouse acts, the
/// native pool is asked every time, and nothing diverges.
#[tokio::test]
async fn shadow_lifecycle_has_no_divergence() {
    let Some(fx) = Fixture::new() else { return };
    let log = MemoryEventLog::new();
    let shadow = ShadowPool::new(fx.treehouse(), fx.native())
        .with_events(Arc::new(log.clone()), HostId::new("mac"));
    let provider = TreehouseProvider::new(shadow);
    let request = |task: &str, branch: &str| WorktreeRequest {
        project: "quark".into(),
        task: task.into(),
        repo: fx.repo.clone(),
        branch: branch.into(),
        base: None,
    };

    let one = provider.get(&request("t1", "feat-1")).await.unwrap();
    let two = provider.get(&request("t2", "feat-2")).await.unwrap();
    let home = provider.lease(&request("t3", ""), "home").await.unwrap();
    provider.status().await.unwrap();

    // Land t1's branch, then return it; t2 keeps uncommitted work.
    std::fs::write(one.path.join("a"), "a\n").unwrap();
    git(&one.path, &["add", "a"]);
    git(&one.path, &["commit", "-q", "-m", "a"]);
    git(&one.path, &["push", "-q", "origin", "HEAD:main"]);
    git(&one.path, &["push", "-q", "origin", "HEAD:feat-1"]);
    git(&fx.repo, &["fetch", "-q", "origin"]);
    assert_eq!(
        provider.return_worktree(&one.id).await.unwrap(),
        ReturnOutcome::Returned
    );
    std::fs::write(two.path.join("wip"), "x\n").unwrap();
    assert!(matches!(
        provider.return_worktree(&two.id).await.unwrap(),
        ReturnOutcome::Kept { .. }
    ));

    // The returned slot is reused for the next task.
    let four = provider.get(&request("t4", "feat-4")).await.unwrap();
    assert_eq!(four.path, one.path);
    assert_eq!(
        home.holder,
        Holder::Lease {
            owner: "home".into()
        }
    );

    let status = provider.status().await.unwrap();
    assert_eq!(status.slots.len(), 3);
    assert!(status
        .slots
        .iter()
        .all(|s| s.state != SlotState::Quarantined));

    let divergences: Vec<Divergence> = log
        .events()
        .iter()
        .filter(|e| e.kind.as_str() == kinds::SHADOW_DIVERGENCE)
        .map(|e| e.decode().unwrap())
        .collect();
    assert!(divergences.is_empty(), "{divergences:#?}");
}

/// The shadow records a disagreement once, and again only when it changes.
#[tokio::test]
async fn shadow_records_divergence_once() {
    let Some(fx) = Fixture::new() else { return };
    let log = MemoryEventLog::new();
    // The native side looks at a different root, so it sees an empty pool.
    let shadow = ShadowPool::new(
        fx.treehouse(),
        NativePool::new().with_root(fx.root.join("elsewhere")),
    )
    .with_events(Arc::new(log.clone()), HostId::new("mac"));
    shadow.acquire(&fx.repo, HOLDER, None, None).await.unwrap();
    shadow.list(&fx.repo).await.unwrap();
    shadow.list(&fx.repo).await.unwrap();
    let kinds_seen: Vec<String> = log
        .events()
        .iter()
        .filter(|e| e.kind.as_str() == kinds::SHADOW_DIVERGENCE)
        .map(|e| e.decode::<Divergence>().unwrap().operation)
        .collect();
    assert_eq!(kinds_seen, ["acquire", "list"]);
}
