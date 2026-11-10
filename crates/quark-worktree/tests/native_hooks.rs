//! The user's `post_create` and `pre_destroy` hooks on the native pool. Its
//! own test binary because hooks come from `$HOME/.config/treehouse`, and
//! `HOME` is process-wide.

#![cfg(unix)]

use std::path::Path;
use std::process::Command;

use quark_worktree::{NativePool, Pool};

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[tokio::test]
async fn user_hooks_run_and_repo_hooks_do_not() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let home = base.join("home");
    std::fs::create_dir_all(home.join(".config/treehouse")).unwrap();
    let log = base.join("hooks.log");
    std::fs::write(
        home.join(".config/treehouse/config.toml"),
        format!(
            "[hooks]\npost_create = [\"echo created $(basename $PWD) >> {0}\", \"exit 3\", \"echo after-failure >> {0}\"]\npre_destroy = [\"echo destroying >> {0}\"]\n",
            log.display()
        ),
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    git(&base, &["init", "-q", "--bare", "origin.git"]);
    git(&base, &["clone", "-q", "origin.git", "repo"]);
    let repo = base.join("repo");
    std::fs::write(repo.join("README"), "x\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    git(&repo, &["push", "-q", "origin", "HEAD:main"]);
    git(&repo, &["remote", "set-head", "origin", "main"]);

    let pool = NativePool::new().with_root(base.join("pools"));
    let a = pool.acquire(&repo, "a", None, None).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        "created repo\nafter-failure\n",
        "hooks run in the slot, in order, and a failing one does not stop the rest"
    );
    pool.release(&repo, &a.path, &a.lease_id).await.unwrap();
    // A reused slot runs post_create again, as treehouse does.
    let b = pool.acquire(&repo, "b", None, None).await.unwrap();
    assert_eq!(b.path, a.path);
    assert_eq!(
        std::fs::read_to_string(&log)
            .unwrap()
            .matches("created")
            .count(),
        2
    );
    pool.release(&repo, &b.path, &b.lease_id).await.unwrap();
    pool.destroy(&repo, &b.path).await.unwrap();
    assert!(std::fs::read_to_string(&log)
        .unwrap()
        .ends_with("destroying\n"));

    // Hooks in the repository's own treehouse.toml are ignored for safety.
    std::fs::write(
        repo.join("treehouse.toml"),
        format!(
            "[hooks]\npost_create = [\"echo repo-hook >> {}\"]\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::write(repo.join(".git/info/exclude"), "treehouse.toml\n").unwrap();
    pool.acquire(&repo, "c", None, None).await.unwrap();
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(!text.contains("repo-hook"), "{text}");
    assert_eq!(text.matches("created").count(), 3, "user hooks still run");
}
