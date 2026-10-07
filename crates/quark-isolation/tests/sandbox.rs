//! Runs real sandboxes. Each test skips, saying so, on a host without a
//! usable one (bubblewrap on Linux, Seatbelt on macOS).
//!
//! Scratch dirs live under the target dir, not `/tmp`, because temp dirs
//! are always writable inside the sandbox.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use quark_core::isolation::{Policy, ProcessSpec};
use quark_core::{CoreError, Isolation, IsolationMode};
use quark_isolation::{git_worktree_policy, HostIsolation, Sandbox, ISOLATION_ENV};

async fn host() -> Option<HostIsolation> {
    let iso = HostIsolation::detect().await;
    if *iso.sandbox() == Sandbox::None {
        eprintln!("skipping: no usable sandbox on this host");
        return None;
    }
    assert!(iso.modes().contains(&IsolationMode::Sandbox));
    Some(iso)
}

fn scratch() -> tempfile::TempDir {
    tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap()
}

fn sh(cwd: &Path, policy: Policy, script: &str) -> ProcessSpec {
    ProcessSpec {
        argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
        cwd: cwd.into(),
        env: BTreeMap::new(),
        mode: IsolationMode::Sandbox,
        policy,
    }
}

/// Run the wrapped spec; true when it exits 0.
async fn run(iso: &HostIsolation, spec: &ProcessSpec) -> bool {
    let (argv, env) = iso.wrap(spec).await.unwrap();
    let out = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(&spec.cwd)
        .envs(env)
        .output()
        .unwrap();
    if !out.status.success() {
        eprintln!("stderr: {}", String::from_utf8_lossy(&out.stderr));
    }
    out.status.success()
}

#[tokio::test]
async fn writes_only_where_the_policy_allows() {
    let Some(iso) = host().await else { return };
    let (wt, extra, outside) = (scratch(), scratch(), scratch());
    let policy = Policy {
        writable: vec![extra.path().into()],
        ..Policy::default()
    };
    let ok = format!("touch a && touch {}/b", extra.path().display());
    assert!(run(&iso, &sh(wt.path(), policy.clone(), &ok)).await);
    assert!(wt.path().join("a").exists() && extra.path().join("b").exists());

    let denied = format!("touch {}/c", outside.path().display());
    assert!(!run(&iso, &sh(wt.path(), policy.clone(), &denied)).await);
    assert!(!outside.path().join("c").exists());

    // Reads work everywhere.
    std::fs::write(outside.path().join("r"), "x").unwrap();
    let read = format!("cat {}/r", outside.path().display());
    assert!(run(&iso, &sh(wt.path(), policy, &read)).await);
}

#[tokio::test]
async fn readable_carve_out_wins_inside_a_writable_tree() {
    let Some(iso) = host().await else { return };
    let wt = scratch();
    let pinned = wt.path().join("pinned");
    std::fs::create_dir(&pinned).unwrap();
    std::fs::write(pinned.join("f"), "orig").unwrap();
    let policy = Policy {
        readable: vec![pinned.clone()],
        ..Policy::default()
    };
    assert!(!run(&iso, &sh(wt.path(), policy.clone(), "echo x > pinned/f")).await);
    assert!(!run(&iso, &sh(wt.path(), policy.clone(), "touch pinned/new")).await);
    assert_eq!(std::fs::read_to_string(pinned.join("f")).unwrap(), "orig");
    assert!(run(&iso, &sh(wt.path(), policy, "cat pinned/f && touch other")).await);
}

#[tokio::test]
async fn sets_the_isolation_env_var() {
    let Some(iso) = host().await else { return };
    let wt = scratch();
    let script = format!("test -n \"${ISOLATION_ENV}\"");
    assert!(run(&iso, &sh(wt.path(), Policy::default(), &script)).await);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn network_off_leaves_only_loopback() {
    let Some(iso) = host().await else { return };
    let wt = scratch();
    let only_lo = "test \"$(tail -n +3 /proc/net/dev | cut -d: -f1 | tr -d ' ')\" = lo";
    assert!(run(&iso, &sh(wt.path(), Policy::default(), only_lo)).await);
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

#[tokio::test]
async fn worker_commits_in_a_linked_worktree_but_cannot_retarget_git() {
    let Some(iso) = host().await else { return };
    let root = scratch();
    let repo: PathBuf = root.path().join("repo");
    let wt = root.path().join("wt");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=q",
            "-c",
            "user.email=q@q",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    );
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "task", wt.to_str().unwrap()],
    );

    let policy = git_worktree_policy(&wt).unwrap();
    let commit = "echo hi > f && git add f && \
        GIT_CONFIG_GLOBAL=/dev/null git -c user.name=q -c user.email=q@q commit -q -m work";
    assert!(run(&iso, &sh(&wt, policy.clone(), commit)).await);
    let log = Command::new("git")
        .args(["-C", repo.to_str().unwrap(), "log", "--format=%s", "task"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&log.stdout).starts_with("work"));

    assert!(!run(&iso, &sh(&wt, policy.clone(), "echo 'gitdir: /x' > .git")).await);
    assert!(!run(&iso, &sh(&wt, policy.clone(), "rm -f .git")).await);
    let config = format!("echo '[core]' >> {}/.git/config", repo.display());
    assert!(!run(&iso, &sh(&wt, policy, &config)).await);
}

#[tokio::test]
async fn native_passes_through_and_container_is_unsupported() {
    let iso = HostIsolation::new(Sandbox::None);
    assert_eq!(iso.modes(), vec![IsolationMode::Native]);
    let mut spec = sh(Path::new("/"), Policy::default(), "true");
    spec.mode = IsolationMode::Native;
    spec.env.insert("A".into(), "1".into());
    let (argv, env) = iso.wrap(&spec).await.unwrap();
    assert_eq!((argv, env), (spec.argv.clone(), spec.env.clone()));

    spec.mode = IsolationMode::Sandbox;
    assert!(matches!(
        iso.wrap(&spec).await,
        Err(CoreError::Unsupported(_))
    ));
    spec.mode = IsolationMode::Container;
    assert!(matches!(
        iso.wrap(&spec).await,
        Err(CoreError::Unsupported(_))
    ));
    spec.argv.clear();
    assert!(matches!(iso.wrap(&spec).await, Err(CoreError::Invalid(_))));
}
