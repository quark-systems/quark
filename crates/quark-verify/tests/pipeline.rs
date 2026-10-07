//! The native gate runner against real git repositories.

use std::path::Path;
use std::process::Command;

use quark_core::verify::{Change, GateStage, VerifyPipeline};
use quark_core::ProjectId;
use quark_verify::{CheckGate, HoldoutGate, NativePipeline, PipelineTarget, RepoGates};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(dir: &Path, file: &str, body: &str, msg: &str) -> String {
    std::fs::write(dir.join(file), body).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", msg]);
    git(dir, &["rev-parse", "HEAD"])
}

struct Fixture {
    _tmp: tempfile::TempDir,
    work: std::path::PathBuf,
    checkout: std::path::PathBuf,
    scratch: std::path::PathBuf,
}

/// An origin with `main` (README) and a `feature` branch (feature.txt)
/// forked from it, then a new commit on main (main.txt). `checkout` is a
/// clone the pipeline verifies from; `work` pushes to origin.
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin.git");
    let work = tmp.path().join("work");
    git(
        tmp.path(),
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            origin.to_str().unwrap(),
        ],
    );
    git(
        tmp.path(),
        &[
            "clone",
            "-q",
            origin.to_str().unwrap(),
            work.to_str().unwrap(),
        ],
    );
    git(&work, &["checkout", "-q", "-b", "main"]);
    commit(&work, "README", "hello\n", "init");
    git(&work, &["push", "-q", "origin", "main"]);
    git(&work, &["checkout", "-q", "-b", "feature"]);
    commit(&work, "feature.txt", "feature\n", "feature");
    git(&work, &["push", "-q", "origin", "feature"]);
    git(&work, &["checkout", "-q", "main"]);
    commit(&work, "main.txt", "main\n", "main moves");
    git(&work, &["push", "-q", "origin", "main"]);
    let checkout = tmp.path().join("checkout");
    git(
        tmp.path(),
        &[
            "clone",
            "-q",
            origin.to_str().unwrap(),
            checkout.to_str().unwrap(),
        ],
    );
    let scratch = tmp.path().join("scratch");
    Fixture {
        work,
        checkout,
        scratch,
        _tmp: tmp,
    }
}

fn target(f: &Fixture, gates: RepoGates) -> PipelineTarget {
    PipelineTarget {
        checkout: f.checkout.clone(),
        repo_name: "app".into(),
        base: "main".into(),
        gates,
        push_rebased: false,
    }
}

fn check(name: &str, run: &str) -> CheckGate {
    CheckGate {
        name: name.into(),
        run: run.into(),
        timeout_s: Some(30),
    }
}

fn change(f: &Fixture) -> Change {
    Change {
        project: "p".into(),
        task: "t1".into(),
        pull_request: None,
        branch: "feature".into(),
        head: git(&f.work, &["rev-parse", "feature"]),
    }
}

#[tokio::test]
async fn verifies_the_head_and_the_rebased_head() {
    let f = fixture();
    let p = NativePipeline::new(&f.scratch);
    p.set_target(
        ProjectId::from("p"),
        target(
            &f,
            RepoGates {
                checks: vec![
                    check("feature", "test -f feature.txt"),
                    check("main", "test -f main.txt"),
                ],
                ..Default::default()
            },
        ),
    );
    let c = change(&f);

    // On its own head the branch lacks main's new file.
    let v = p.verify(&c).await.unwrap();
    assert_eq!(v.head, c.head);
    assert!(!v.passed());
    assert_eq!(v.stages[0].stage, GateStage::RepoChecks);
    assert_eq!(
        v.stages[0].summary,
        "feature: passed; main: failed (exit 1)"
    );

    // Rebased onto main it passes, on a new head that contains main's tip.
    let v = p.rebase_and_reverify(&c).await.unwrap();
    assert!(v.passed(), "{v:?}");
    assert_ne!(v.head, c.head);
    let tip = git(&f.work, &["rev-parse", "main"]);
    git(&f.checkout, &["fetch", "-q", "origin"]);
    git(&f.checkout, &["merge-base", "--is-ancestor", &tip, &v.head]);

    // Scratch worktrees are cleaned up and the checkout is untouched.
    assert_eq!(git(&f.checkout, &["worktree", "list"]).lines().count(), 1);
    assert_eq!(git(&f.checkout, &["status", "--porcelain"]), "");
}

#[tokio::test]
async fn a_conflicting_rebase_is_reported() {
    let f = fixture();
    git(&f.work, &["checkout", "-q", "main"]);
    commit(&f.work, "feature.txt", "main's version\n", "conflict");
    git(&f.work, &["push", "-q", "origin", "main"]);
    let p = NativePipeline::new(&f.scratch);
    p.set_target(ProjectId::from("p"), target(&f, RepoGates::default()));
    let v = p.rebase_and_reverify(&change(&f)).await.unwrap();
    assert!(!v.passed());
    assert!(v
        .conflict
        .as_deref()
        .unwrap()
        .contains("could not rebase onto origin/main"));
    assert_eq!(git(&f.checkout, &["worktree", "list"]).lines().count(), 1);
}

#[tokio::test]
async fn no_gates_pass_with_a_note_and_holdout_hides_its_output() {
    let f = fixture();
    let p = NativePipeline::new(&f.scratch);
    p.set_target(ProjectId::from("p"), target(&f, RepoGates::default()));
    let v = p.verify(&change(&f)).await.unwrap();
    assert!(v.passed());
    assert_eq!(v.stages[0].summary, "no gates are configured for app");

    // A holdout repo with two categories, one failing.
    let holdout = f.scratch.parent().unwrap().join("holdout");
    std::fs::create_dir_all(&holdout).unwrap();
    git(&holdout, &["init", "-q", "-b", "main"]);
    for (cat, body) in [
        ("api", "test -f \"$GATE_TARGET/feature.txt\""),
        ("ui", "echo secret-test-name; exit 1"),
    ] {
        let dir = holdout.join("holdout/app").join(cat);
        std::fs::create_dir_all(&dir).unwrap();
        let run = dir.join("run");
        std::fs::write(&run, format!("#!/usr/bin/env bash\n{body}\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    git(&holdout, &["add", "."]);
    git(
        &holdout,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@e",
            "commit",
            "-q",
            "-m",
            "tests",
        ],
    );
    p.set_target(
        ProjectId::from("p"),
        target(
            &f,
            RepoGates {
                holdout: Some(HoldoutGate {
                    repo: holdout.to_str().unwrap().into(),
                    git_ref: Some("main".into()),
                    path: None,
                    timeout_s: Some(30),
                }),
                ..Default::default()
            },
        ),
    );
    let v = p.verify(&change(&f)).await.unwrap();
    assert!(!v.passed());
    assert_eq!(v.stages[0].stage, GateStage::Holdout);
    assert_eq!(v.stages[0].summary, "api: passed; ui: failed");
}
