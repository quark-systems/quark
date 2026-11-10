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

impl Fixture {
    /// Commit `files` and push them to origin's main.
    fn commit(&self, files: &[(&str, &str)]) {
        for (name, body) in files {
            let p = self.repo.join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
            git(&self.repo, &["add", "-f", name]);
        }
        git(&self.repo, &["commit", "-q", "-m", "files"]);
        git(&self.repo, &["push", "-q", "origin", "HEAD:main"]);
    }

    /// Local pool settings, kept out of `git status`.
    fn settings(&self, toml: &str) {
        std::fs::write(self.repo.join("treehouse.toml"), toml).unwrap();
        std::fs::write(self.repo.join(".git/info/exclude"), "treehouse.toml\n").unwrap();
    }

    fn entry(&self, path: &Path) -> state::Entry {
        let pool = self.pool().pool_dir(&self.repo).unwrap();
        state::read(&pool)
            .unwrap()
            .worktrees
            .into_iter()
            .find(|w| w.path == path)
            .unwrap()
    }
}

#[tokio::test]
async fn jj_is_still_refused_not_ignored() {
    let fx = Fixture::new();
    std::fs::create_dir(fx.repo.join(".jj")).unwrap();
    std::fs::write(fx.repo.join(".git/info/exclude"), ".jj\ntreehouse.toml\n").unwrap();
    std::fs::write(fx.repo.join("treehouse.toml"), "vcs = \"jj\"\n").unwrap();
    let err = fx
        .pool()
        .acquire(&fx.repo, "a", None, None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("jj workspaces"), "{err}");

    let fx = Fixture::new();
    fx.settings("apfs_sharing = \"on\"\n");
    let err = fx
        .pool()
        .acquire(&fx.repo, "a", None, None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("invalid apfs_sharing"), "{err}");
}

#[tokio::test]
async fn worktreeinclude_seeds_ignored_files_and_cleans_exactly_those() {
    let fx = Fixture::new();
    fx.commit(&[
        (".gitignore", "*.seed\n.env\nbuild/\n"),
        (
            ".worktreeinclude",
            ".env\nconfig/*.seed\n!config/skip.seed\n",
        ),
        ("config/tracked.seed", "tracked\n"),
    ]);
    std::fs::write(fx.repo.join(".env"), "SECRET=1\n").unwrap();
    std::fs::create_dir_all(fx.repo.join("config")).unwrap();
    std::fs::write(fx.repo.join("config/local.seed"), "local\n").unwrap();
    std::fs::write(fx.repo.join("config/skip.seed"), "skip\n").unwrap();
    std::fs::write(fx.repo.join("other.seed"), "not selected\n").unwrap();
    std::fs::write(fx.repo.join("notes.txt"), "untracked, not ignored\n").unwrap();
    std::fs::set_permissions(
        fx.repo.join(".env"),
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();

    let pool = fx.pool();
    let a = pool.acquire(&fx.repo, "a", None, None).await.unwrap();
    let read = |p: &Path, name: &str| std::fs::read_to_string(p.join(name)).ok();
    assert_eq!(read(&a.path, ".env").as_deref(), Some("SECRET=1\n"));
    assert_eq!(
        read(&a.path, "config/local.seed").as_deref(),
        Some("local\n")
    );
    assert_eq!(
        read(&a.path, "config/skip.seed"),
        None,
        "negation is honoured"
    );
    assert_eq!(read(&a.path, "other.seed"), None, "only selected files");
    assert_eq!(read(&a.path, "notes.txt"), None, "only ignored files");
    assert_eq!(
        read(&a.path, "config/tracked.seed").as_deref(),
        Some("tracked\n")
    );
    let mode = std::fs::metadata(a.path.join(".env"))
        .unwrap()
        .permissions();
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
        0o600
    );
    let mut seeded = fx.entry(&a.path).seeded_paths;
    seeded.sort();
    assert_eq!(seeded, [".env", "config/local.seed"]);
    assert!(fx.entry(&a.path).seed_inventory_known);

    // The task leaves its own ignored output; a release removes only the
    // seeded files (git clean leaves other ignored files, as treehouse does).
    std::fs::create_dir_all(a.path.join("build")).unwrap();
    std::fs::write(a.path.join("build/out"), "artifact\n").unwrap();
    assert_eq!(
        pool.release(&fx.repo, &a.path, &a.lease_id).await.unwrap(),
        Release::Returned
    );
    assert_eq!(read(&a.path, ".env"), None);
    assert_eq!(read(&a.path, "config/local.seed"), None);
    assert_eq!(read(&a.path, "build/out").as_deref(), Some("artifact\n"));
    assert!(fx.entry(&a.path).seeded_paths.is_empty());

    // The seeded slot is reused, with fresh copies.
    std::fs::write(fx.repo.join(".env"), "SECRET=2\n").unwrap();
    let b = pool.acquire(&fx.repo, "b", None, None).await.unwrap();
    assert_eq!(b.path, a.path, "a seeded slot is reusable");
    assert_eq!(read(&b.path, ".env").as_deref(), Some("SECRET=2\n"));

    // Reusing a slot whose seeds are still there (a branch creation that
    // failed and freed it) replaces them instead of failing on them.
    let pool_dir = pool.pool_dir(&fx.repo).unwrap();
    let mut s = state::read(&pool_dir).unwrap();
    let wt = s.worktrees.iter_mut().find(|w| w.path == b.path).unwrap();
    wt.clear_lease();
    state::write(&pool_dir, s).unwrap();
    std::fs::write(fx.repo.join(".env"), "SECRET=3\n").unwrap();
    let c = pool
        .acquire(&fx.repo, "c", Some("feat"), None)
        .await
        .unwrap();
    assert_eq!(c.path, b.path);
    assert_eq!(read(&c.path, ".env").as_deref(), Some("SECRET=3\n"));
}

#[tokio::test]
async fn seeding_never_replaces_tracked_paths_or_trusts_an_uncommitted_manifest() {
    let fx = Fixture::new();
    fx.commit(&[(".gitignore", ".env\n")]);
    std::fs::write(fx.repo.join(".env"), "x\n").unwrap();
    // An uncommitted .worktreeinclude is not read.
    std::fs::write(fx.repo.join(".worktreeinclude"), ".env\n").unwrap();
    let pool = fx.pool();
    let a = pool.acquire(&fx.repo, "a", None, None).await.unwrap();
    assert!(!a.path.join(".env").exists());
    assert!(!a.path.join(".worktreeinclude").exists());
}

#[tokio::test]
async fn unique_leaf_and_worktree_path_place_new_slots() {
    let fx = Fixture::new();
    fx.settings("unique_leaf = true\n");
    let pool = fx.pool();
    let a = pool.acquire(&fx.repo, "a", None, None).await.unwrap();
    assert_eq!(a.path.file_name().unwrap(), "repo-1");
    assert_eq!(a.path.parent().unwrap().file_name().unwrap(), "1");

    let fx = Fixture::new();
    fx.settings("worktree_path = \"{repo_parent}/{repo}-wt-{slot}\"\n");
    let base = fx.repo.parent().unwrap().to_path_buf();
    // An existing directory is never adopted: slot 1's path is taken.
    std::fs::create_dir(base.join("repo-wt-1")).unwrap();
    let pool = fx.pool();
    let a = pool.acquire(&fx.repo, "a", None, None).await.unwrap();
    assert_eq!(a.path, base.join("repo-wt-2"));
    assert_eq!(fx.entry(&a.path).name, "2");
    let b = pool.acquire(&fx.repo, "b", None, None).await.unwrap();
    assert_eq!(b.path, base.join("repo-wt-3"));
    // Releases and reuse work on templated paths.
    pool.release(&fx.repo, &a.path, &a.lease_id).await.unwrap();
    let c = pool.acquire(&fx.repo, "c", None, None).await.unwrap();
    assert_eq!(c.path, a.path);
    pool.release(&fx.repo, &c.path, &c.lease_id).await.unwrap();
    // Destroy removes the worktree only, never the directory holding it.
    let out = pool.destroy(&fx.repo, &c.path).await.unwrap();
    assert_eq!(out.removed.len(), 1, "{out:?}");
    assert!(!c.path.exists());
    assert!(base.join("repo-wt-1").exists());
    assert!(fx.repo.exists());

    // A template that escapes into the repository is refused up front.
    fx.settings("worktree_path = \"{repo_parent}/{repo}/wt-{slot}\"\n");
    let err = pool.acquire(&fx.repo, "d", None, None).await.unwrap_err();
    assert!(err.to_string().contains("inside the repository"), "{err}");
}

#[tokio::test]
async fn recovered_slot_untracked_files_move_to_a_backup() {
    let fx = Fixture::new();
    let pool = fx.pool();
    let a = pool.acquire(&fx.repo, "a", None, None).await.unwrap();
    let pool_dir = pool.pool_dir(&fx.repo).unwrap();
    std::fs::write(a.path.join("notes.txt"), "keep me\n").unwrap();
    // The state is lost: the slot comes back quarantined as recovered.
    std::fs::remove_file(pool_dir.join("treehouse-state.json")).unwrap();
    let listed = pool.list(&fx.repo).await.unwrap();
    let e = listed.iter().find(|e| e.path == a.path).unwrap();
    // Listing freed it after moving the file into the retained backup.
    assert_eq!(e.status, "available", "{e:?}");
    let backup = pool_dir.parent().unwrap().join(format!(
        "treehouse-recovered-backup-{}-1",
        pool_dir.file_name().unwrap().to_string_lossy()
    ));
    assert_eq!(
        std::fs::read_to_string(backup.join("notes.txt")).unwrap(),
        "keep me\n"
    );
    assert!(!a.path.join("notes.txt").exists());
    assert_eq!(e.recovery_backup, backup.to_string_lossy());
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
