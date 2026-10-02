//! FirstmateEngine against a fake engine that prints a snapshot captured
//! from the real firstmate engine.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use quark_engine::runner::MemoryCallLog;
use quark_systems::CreateProject;
use quark_systems::{TaskKind, TaskState};
use quarkd::config::StoreCallLog;
use quarkd::engine::firstmate::FirstmateEngine;
use quarkd::engine::{EngineAdapter, EngineError, TaskControl, WorkspaceRef};
use quarkd::store::Store;

struct Fake {
    home: PathBuf,
    engine_root: PathBuf,
}

fn fake_engine(dir: &Path) -> Fake {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../quark-engine/tests/fixtures/fleet-snapshot.v1.json");
    let bin = dir.join("engine/bin");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(dir.join("home/state")).unwrap();
    let script = bin.join("fm-fleet-snapshot.sh");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\n[ \"$1\" = --json ] || exit 2\ncat '{}'\n",
            fixture.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    Fake {
        home: dir.join("home"),
        engine_root: dir.join("engine"),
    }
}

fn engine(dir: &Path) -> (FirstmateEngine, WorkspaceRef) {
    let ws = fake_engine(dir);
    (
        FirstmateEngine::new(&ws.engine_root, Arc::new(MemoryCallLog::default())),
        WorkspaceRef {
            project_id: "p1".into(),
            root: ws.home.clone(),
        },
    )
}

#[tokio::test]
async fn snapshot_maps_to_neutral_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let (e, ws) = engine(dir.path());
    assert_eq!(e.name(), "firstmate");
    let snap = e.snapshot(&ws).await.unwrap();
    let got: Vec<_> = snap
        .tasks
        .iter()
        .map(|t| (t.id.as_str(), t.kind, t.state))
        .collect();
    assert_eq!(
        got,
        [
            ("external-wait", Some(TaskKind::Ship), TaskState::Paused),
            ("scout-x", Some(TaskKind::Scout), TaskState::Done),
            ("ship-task", Some(TaskKind::Ship), TaskState::Running),
            ("live-gate", Some(TaskKind::Ship), TaskState::Queued),
            ("dead-gate", Some(TaskKind::Scout), TaskState::Queued),
        ],
        "secondmates and captain-kind rows are not tasks"
    );
    let ship = &snap.tasks[2];
    assert_eq!(ship.title, "Ship the thing");
    assert_eq!(ship.harness.as_deref(), Some("claude"));
    assert_eq!(
        ship.pull_request_url.as_deref(),
        Some("https://github.com/kunchenguid/firstmate/pull/9")
    );
}

#[tokio::test]
async fn holds_are_live_holds_and_open_decisions() {
    let dir = tempfile::tempdir().unwrap();
    let (e, ws) = engine(dir.path());
    let holds = e.holds(&ws).await.unwrap();
    let got: Vec<_> = holds
        .iter()
        .map(|h| (h.id.as_str(), h.question.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("pick-db", "Choose the projection store: SQLite or DuckDB"),
            ("mate:race", "pick subscribe order"),
        ]
    );
}

#[tokio::test]
async fn status_tail_resumes_from_offset() {
    let dir = tempfile::tempdir().unwrap();
    let (e, ws) = engine(dir.path());
    fs::write(ws.root.join("state/t.status"), "working: a\ndone: b\n").unwrap();
    let first = e.status_tail(&ws, "t", 0).await.unwrap();
    assert_eq!(first.lines, ["working: a", "done: b"]);
    let again = e.status_tail(&ws, "t", first.next_offset).await.unwrap();
    assert!(again.lines.is_empty());
    assert_eq!(again.next_offset, first.next_offset);
    assert!(matches!(
        e.status_tail(&ws, "../x", 0).await,
        Err(EngineError::TaskNotFound(_))
    ));
}

#[tokio::test]
async fn missing_workspace_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let (e, mut ws) = engine(dir.path());
    ws.root = dir.path().join("nope");
    assert!(matches!(
        e.snapshot(&ws).await,
        Err(EngineError::WorkspaceNotFound(_))
    ));
}

/// Fake write scripts that append their argv, one argument per line, to
/// `<home>/calls.log`. `fm-control.sh` fails for task `stuck`.
fn add_write_scripts(engine_root: &Path) {
    for (name, extra) in [
        ("fm-send.sh", ""),
        (
            "fm-control.sh",
            "[ \"$1\" = stuck ] && { echo 'agent did not stop' >&2; exit 1; }\n",
        ),
    ] {
        let p = engine_root.join("bin").join(name);
        fs::write(
            &p,
            format!(
                "#!/bin/sh\n{extra}{{ echo '--- {name}'; for a in \"$@\"; do echo \"$a\"; done; }} >> \"$FM_HOME/calls.log\"\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[tokio::test]
async fn writes_run_allowlisted_scripts_and_are_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_engine(dir.path());
    add_write_scripts(&fake.engine_root);
    let store = Arc::new(Store::open_in_memory().unwrap());
    let project = store
        .create_project(CreateProject {
            name: "Quark".into(),
            goal: None,
            workspace_path: Some(fake.home.to_string_lossy().into_owned()),
        })
        .unwrap();
    let e = FirstmateEngine::new(
        &fake.engine_root,
        Arc::new(StoreCallLog {
            store: store.clone(),
        }),
    );
    let ws = WorkspaceRef {
        project_id: project.id.clone(),
        root: fake.home.clone(),
    };

    e.send_message(&ws, "ship-task", "line one\n- line two")
        .await
        .unwrap();
    e.control(&ws, "ship-task", &TaskControl::Cancel)
        .await
        .unwrap();
    e.control(
        &ws,
        "ship-task",
        &TaskControl::Relaunch {
            harness: Some("codex".into()),
            model: None,
            effort: Some("high".into()),
            note: "--resume from the red test".into(),
        },
    )
    .await
    .unwrap();

    let log = fs::read_to_string(fake.home.join("calls.log")).unwrap();
    assert_eq!(
        log,
        "--- fm-send.sh\nship-task\nline one\n- line two\n\
         --- fm-control.sh\nship-task\nexit\n\
         --- fm-control.sh\nship-task\nrelaunch\n--harness=codex\n--effort=high\n\
         --note=--resume from the red test\n"
    );

    // Refused before anything runs: no script call, no record.
    let err = e.send_message(&ws, "ship-task", "/quit").await.unwrap_err();
    assert!(matches!(err, EngineError::Invalid(_)), "{err:?}");
    let err = e
        .control(&ws, "--help", &TaskControl::Cancel)
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::TaskNotFound(_)), "{err:?}");

    let err = e
        .control(&ws, "stuck", &TaskControl::Cancel)
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::Command(ref m) if m.contains("agent did not stop")));

    let calls = store.recent_script_calls(10).unwrap();
    let got: Vec<_> = calls
        .iter()
        .rev()
        .map(|c| (c.script.as_str(), c.args[1].as_str(), c.ok, c.exit_code))
        .collect();
    assert_eq!(
        got,
        [
            ("fm-send.sh", "line one\n- line two", true, Some(0)),
            ("fm-control.sh", "exit", true, Some(0)),
            ("fm-control.sh", "relaunch", true, Some(0)),
            ("fm-control.sh", "exit", false, Some(1)),
        ]
    );
    assert!(calls.iter().all(|c| c.kind == "write"));
    assert!(calls
        .iter()
        .all(|c| c.project_id.as_deref() == Some(project.id.as_str())));
    assert_eq!(calls[0].detail.as_deref(), Some("agent did not stop\n"));
}
