//! Upgrading a Quark home made before 0.1 (format 0, no `format.json`): a
//! workspace firstmate made, with its source clone in `projects/` and a task
//! worktree cut from it, is brought to the current format; the daemon then
//! starts on it and a second start changes nothing.

use std::path::Path;
use std::process::Command;

use quarkd::engine::native_dispatch_engine::source_repo;
use quarkd::home_format::{self, CURRENT};

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
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A home as quarkd left it in early October 2026: a projection store, a
/// Project workspace that is a firstmate secondmate with Quark's files
/// beside firstmate's, and its source repo cloned in `projects/`.
fn legacy_home(home: &Path) -> std::path::PathBuf {
    quarkd::store::Store::open(&home.join("quark.db")).unwrap();
    let ws = home.join("workspaces/prj_legacy");
    // firstmate's engine clone, tracked by its own git.
    write(&ws.join("AGENTS.md"), "firstmate\n");
    write(&ws.join("bin/fm-spawn.sh"), "#!/bin/sh\n");
    write(&ws.join(".claude/skills/fm/SKILL.md"), "skill\n");
    git(&ws, &["init", "-q"]);
    git(&ws, &["add", "AGENTS.md", "bin"]);
    git(&ws, &["commit", "-qm", "fm"]);
    write(&ws.join(".fm-secondmate-home"), "");
    write(&ws.join("data/backlog.md"), "- [ ] x\n");
    // Quark's own files.
    write(&ws.join(".claude/settings.local.json"), "{}\n");
    write(&ws.join(".mcp.json"), "{}\n");
    write(&ws.join("config/crew-dispatch.json"), "{}\n");
    // The source clone, with a worktree a task worked in.
    let clone = ws.join("projects/app");
    std::fs::create_dir_all(&clone).unwrap();
    git(&clone, &["init", "-q"]);
    write(&clone.join("README.md"), "app\n");
    git(&clone, &["add", "."]);
    git(&clone, &["commit", "-qm", "init"]);
    let tree = home.join("worktrees/app-1");
    git(
        &clone,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "task",
            tree.to_str().unwrap(),
        ],
    );
    ws
}

#[cfg(unix)]
#[test]
fn a_pre_0_1_home_upgrades_and_keeps_its_work() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let ws = legacy_home(home);
    assert!(home_format::read(home).unwrap().is_none());

    let m = home_format::migrate(home).unwrap();
    assert_eq!((m.from, m.to), (None, CURRENT));
    assert!(m.adopt_legacy_projects);
    assert_eq!(m.retired, vec![ws.clone()]);
    assert_eq!(m.linked, vec![ws.join("repos/app")]);
    assert_eq!(home_format::read(home).unwrap().unwrap().format, CURRENT);

    // firstmate's files are backed up, Quark's stay.
    for gone in [
        "AGENTS.md",
        "bin",
        ".git",
        "data",
        ".fm-secondmate-home",
        ".claude/skills",
    ] {
        assert!(
            ws.join(gone).symlink_metadata().is_err(),
            "{gone} still there"
        );
    }
    for kept in [
        ".claude/settings.local.json",
        ".mcp.json",
        "config/crew-dispatch.json",
    ] {
        assert!(ws.join(kept).exists(), "{kept} moved");
    }
    assert!(home.join("backups/firstmate").read_dir().unwrap().count() == 1);
    assert!(
        ws.join(".quark/workspace.json").exists(),
        "claimed as native"
    );

    // The engine finds the clone under repos/ and knows it by its own path,
    // so the task worktree cut from it still works.
    assert_eq!(source_repo(&ws, None).unwrap(), ws.join("projects/app"));
    assert_eq!(
        source_repo(&ws, Some("app")).unwrap(),
        ws.join("projects/app")
    );
    let tree = home.join("worktrees/app-1");
    assert!(git(&tree, &["status", "--porcelain=v1", "--branch"]).contains("task"));
    assert!(git(&ws.join("projects/app"), &["worktree", "list"]).contains("app-1"));

    // The store still opens, and a second start does nothing.
    quarkd::store::Store::open(&home.join("quark.db")).unwrap();
    let again = home_format::migrate(home).unwrap();
    assert_eq!(again.from, Some(CURRENT));
    assert!(!again.adopt_legacy_projects && again.retired.is_empty() && again.linked.is_empty());
}

#[test]
fn quarkd_refuses_a_home_from_a_newer_quark() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(home_format::FILE),
        r#"{"format": 999, "written_by": "9.9.9"}"#,
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_quarkd"))
        .arg("--home")
        .arg(dir.path())
        .args(["--engine", "stub", "--listen", "127.0.0.1:0"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("format 999"), "{err}");
    assert!(
        !dir.path().join("quark.db").exists(),
        "opened the store anyway"
    );
}
