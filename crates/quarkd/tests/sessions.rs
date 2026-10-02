//! Terminal sessions against a real tmux server. Skipped when tmux is not
//! installed.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use quark_systems::{
    CreateProject, Event, EventType, TerminalChunkKind, TerminalOutput, TerminalRole,
};
use quarkd::sessions::{SessionError, Sessions, TaskTarget, WindowSpec, RETAIN_BYTES};
use quarkd::store::Store;
use tokio::sync::broadcast;

fn tmux_installed() -> bool {
    std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn output(e: &Event) -> Option<(TerminalOutput, Vec<u8>)> {
    if e.event_type != EventType::WorkerOutput {
        return None;
    }
    let out: TerminalOutput = serde_json::from_value(e.payload.clone()).unwrap();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&out.data_b64)
        .unwrap();
    Some((out, bytes))
}

/// Waits for an output event of `terminal` matching `pred`, collecting the
/// terminal's bytes since the last snapshot.
async fn wait_for(
    rx: &mut broadcast::Receiver<Event>,
    terminal: &str,
    what: &str,
    pred: impl Fn(&TerminalOutput, &[u8]) -> bool,
) -> (TerminalOutput, Vec<u8>) {
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let event = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "timed out waiting for {what} on {terminal}; saw {:?}",
                    String::from_utf8_lossy(&seen)
                )
            })
            .expect("event bus open");
        let Some((out, bytes)) = output(&event) else {
            continue;
        };
        if out.terminal_id != terminal {
            continue;
        }
        if out.kind == TerminalChunkKind::Snapshot {
            seen.clear();
        }
        seen.extend_from_slice(&bytes);
        if pred(&out, &seen) {
            return (out, seen);
        }
    }
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
}

fn shell(name: &str) -> WindowSpec {
    WindowSpec {
        name: name.into(),
        argv: vec!["bash".into(), "--norc".into(), "--noprofile".into()],
        cwd: None,
        env: vec![("PS1".into(), "$ ".into())],
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streams_maps_types_and_reattaches() {
    if !tmux_installed() {
        eprintln!("tmux is not installed; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let project = store
        .create_project(CreateProject {
            name: "p".into(),
            goal: None,
            workspace_path: None,
            ..Default::default()
        })
        .unwrap();
    let mut rx = store.subscribe();
    let other = store
        .create_project(CreateProject {
            name: "other".into(),
            goal: None,
            workspace_path: None,
            ..Default::default()
        })
        .unwrap();
    let sessions = Sessions::new("tmux", dir.path().join("run"), store.clone()).unwrap();
    let server = sessions.server().unwrap().clone();
    assert_eq!(
        sessions.tmux_env().unwrap(),
        format!("{},0,0", server.socket().display())
    );

    // The coordinator window, then worker windows as firstmate would name
    // them, one of them another Project's.
    sessions.ensure_server().await.unwrap();
    let coordinator = sessions.start_window(&shell("fm-coord")).await.unwrap();
    assert_eq!(coordinator, "quark:fm-coord");
    sessions
        .set_coordinator(&project.id, Some(coordinator.clone()))
        .await
        .unwrap();
    sessions.start_window(&shell("fm-fix-login")).await.unwrap();
    sessions.start_window(&shell("fm-elsewhere")).await.unwrap();
    sessions
        .sync(
            &other.id,
            vec![TaskTarget {
                task_id: "tsk_elsewhere".into(),
                target: "quark:fm-elsewhere".into(),
            }],
        )
        .await
        .unwrap();
    let task = "tsk_fixlogin";
    sessions
        .sync(
            &project.id,
            vec![TaskTarget {
                task_id: task.into(),
                target: "quark:fm-fix-login".into(),
            }],
        )
        .await
        .unwrap();

    // Each terminal starts with a snapshot.
    let (first, _) = wait_for(&mut rx, task, "first snapshot", |o, _| {
        o.kind == TerminalChunkKind::Snapshot
    })
    .await;
    assert_eq!(first.role, TerminalRole::Worker);
    assert_eq!(first.task_id.as_deref(), Some(task));
    assert_eq!((first.cols, first.rows), (Some(120), Some(36)));

    let terminals = sessions.list(&project.id);
    let roles: Vec<_> = terminals
        .iter()
        .map(|t| (t.id.as_str(), t.role, t.task_id.as_deref()))
        .collect();
    assert_eq!(
        roles,
        [
            (project.id.as_str(), TerminalRole::Coordinator, None),
            (task, TerminalRole::Worker, Some(task)),
        ]
    );
    let others: Vec<_> = sessions.list(&other.id).into_iter().map(|t| t.id).collect();
    assert_eq!(others, ["tsk_elsewhere"]);

    // Input is typed and its echo streams back as output.
    sessions
        .input(task, b"echo hello-$((6*7))\r", Some(1))
        .await
        .unwrap();
    wait_for(&mut rx, task, "echo", |_, b| contains(b, "hello-42")).await;
    assert!(matches!(
        sessions.input(task, b"x", Some(1)).await,
        Err(SessionError::StaleInput { got: 1, last: 1 })
    ));

    // Resize reaches the program.
    let t = sessions.resize(task, 100, 30).await.unwrap();
    assert_eq!((t.cols, t.rows), (100, 30));
    sessions
        .input(task, b"echo size-$(tput cols)x$(tput lines)\r", None)
        .await
        .unwrap();
    wait_for(&mut rx, task, "resized", |_, b| contains(b, "size-100x30")).await;

    // An explicit snapshot holds the current screen.
    let snap = sessions.snapshot(task).await.unwrap();
    let (out, bytes) = output(&snap).unwrap();
    assert_eq!(out.kind, TerminalChunkKind::Snapshot);
    assert_eq!((out.cols, out.rows), (Some(100), Some(30)));
    assert!(contains(&bytes, "hello-42"));

    // A pane tmux pauses (as it does when the daemon falls behind) is
    // resynchronized with a snapshot and keeps streaming.
    let sock = server.socket().to_path_buf();
    let clients = std::process::Command::new("tmux")
        .arg("-S")
        .arg(&sock)
        .args(["list-clients", "-F", "#{client_name}"])
        .output()
        .unwrap();
    let pane = std::process::Command::new("tmux")
        .arg("-S")
        .arg(&sock)
        .args([
            "display-message",
            "-p",
            "-t",
            "quark:fm-fix-login",
            "#{pane_id}",
        ])
        .output()
        .unwrap();
    let pane = String::from_utf8_lossy(&pane.stdout).trim().to_string();
    for client in String::from_utf8_lossy(&clients.stdout).lines() {
        let status = std::process::Command::new("tmux")
            .arg("-S")
            .arg(&sock)
            .args(["refresh-client", "-t", client, "-A"])
            .arg(format!("{pane}:pause"))
            .status()
            .unwrap();
        assert!(status.success());
    }
    wait_for(&mut rx, task, "resync snapshot", |o, _| {
        o.kind == TerminalChunkKind::Snapshot
    })
    .await;
    sessions.input(task, b"echo resumed\r", None).await.unwrap();
    wait_for(&mut rx, task, "output after pause", |_, b| {
        contains(b, "resumed")
    })
    .await;

    // Coordinator input works the same way.
    sessions
        .input(&project.id, b"echo coord-ok\r", None)
        .await
        .unwrap();
    wait_for(&mut rx, &project.id, "coordinator echo", |_, b| {
        contains(b, "coord-ok")
    })
    .await;

    // A daemon restart: detach everything, start again, sync again. The
    // panes kept running and come back with a snapshot of where they were.
    sessions.detach_all();
    drop(sessions);
    let sessions = Sessions::new("tmux", dir.path().join("run"), store.clone()).unwrap();
    sessions
        .set_coordinator(&project.id, Some(coordinator))
        .await
        .unwrap();
    sessions
        .sync(
            &project.id,
            vec![TaskTarget {
                task_id: task.into(),
                target: "quark:fm-fix-login".into(),
            }],
        )
        .await
        .unwrap();
    wait_for(&mut rx, task, "reattach snapshot", |o, b| {
        o.kind == TerminalChunkKind::Snapshot && contains(b, "size-100x30")
    })
    .await;
    sessions.input(task, b"echo again\r", None).await.unwrap();
    wait_for(&mut rx, task, "echo after reattach", |_, b| {
        contains(b, "again")
    })
    .await;

    // When the program exits its window closes and the terminal goes away.
    sessions.input(task, b"exit\r", None).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while sessions.list(&project.id).iter().any(|t| t.id == task) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "terminal stayed listed"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(matches!(
        sessions.input(task, b"x", None).await,
        Err(SessionError::NotFound)
    ));

    sessions.detach_all();
    server.kill().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn floods_are_recorded_and_pruned_to_a_snapshot() {
    if !tmux_installed() {
        eprintln!("tmux is not installed; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let project = store
        .create_project(CreateProject {
            name: "flood".into(),
            goal: None,
            workspace_path: None,
            ..Default::default()
        })
        .unwrap();
    let mut rx = store.subscribe();
    let sessions = Sessions::new("tmux", dir.path().join("run"), store.clone()).unwrap();
    let server = sessions.server().unwrap().clone();
    let coordinator = sessions.start_window(&shell("fm-coord")).await.unwrap();
    sessions
        .set_coordinator(&project.id, Some(coordinator))
        .await
        .unwrap();
    let id = project.id.clone();
    wait_for(&mut rx, &id, "first snapshot", |o, _| {
        o.kind == TerminalChunkKind::Snapshot
    })
    .await;

    // About 7 MB of numbered lines, then a marker.
    sessions
        .input(&id, b"seq 1 1000000; echo flood-$((1+1))-done\r", None)
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut last_line = Vec::new();
    loop {
        let event = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .expect("flood finished")
            .unwrap();
        if let Some((out, bytes)) = output(&event) {
            if out.terminal_id == id {
                last_line.extend_from_slice(&bytes);
                if contains(&last_line, "flood-2-done") {
                    break;
                }
                let keep = last_line.len().saturating_sub(64);
                last_line.drain(..keep);
            }
        }
    }

    // Wait for the writer's last prune, then check what replay would give.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let events = store.events_after(0, 100_000).unwrap();
    let kept: Vec<_> = events
        .iter()
        .filter_map(output)
        .filter(|(o, _)| o.terminal_id == id)
        .collect();
    let bytes: usize = kept.iter().map(|(_, b)| b.len()).sum();
    assert!(
        bytes as u64 <= RETAIN_BYTES + 512 * 1024,
        "kept {bytes} bytes of output"
    );
    assert_eq!(
        kept.first().map(|(o, _)| o.kind),
        Some(TerminalChunkKind::Snapshot),
        "replay starts from a snapshot"
    );

    sessions.detach_all();
    server.kill().await;
}

/// Provisioning starts the coordinator through the engine in Quark's shared
/// server and maps its window as the Project's coordinator terminal, which a
/// restarted daemon maps again from the store.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provisioning_starts_the_coordinator_in_the_shared_server() {
    use std::os::unix::fs::PermissionsExt;

    use quark_engine::runner::MemoryCallLog;
    use quark_systems::{AgentConfig, ProjectStatus, RepoSource};
    use quarkd::engine::firstmate::FirstmateEngine;
    use quarkd::engine::EngineAdapter;
    use quarkd::provision::{self, Layout};

    if !tmux_installed() {
        eprintln!("tmux is not installed; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("engine/bin");
    std::fs::create_dir_all(&bin).unwrap();
    // fm-spawn opens its window in whatever server $TMUX names, as firstmate
    // does, so the window only lands in Quark's server if TMUX reached it.
    for (name, body) in [
        (
            "fm-project-add.sh",
            "echo \"project=$1 path=$FM_HOME/projects/$1 mode=$4 yolo=off result=added\"",
        ),
        ("fm-home-seed.sh", "mkdir -p \"$2\"; echo \"home=$2\""),
        (
            "fm-spawn.sh",
            "tmux new-window -d -t quark: -n \"$1\" 'sleep 600' || exit 1\n\
             echo \"spawned $1 harness=$4 kind=secondmate window=quark:$1 worktree=$2\"",
        ),
    ] {
        let p = bin.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new("tmux", dir.path().join("run"), store.clone()).unwrap();
    let engine: Arc<dyn EngineAdapter> = Arc::new(
        FirstmateEngine::new(
            dir.path().join("engine"),
            Arc::new(MemoryCallLog::default()),
        )
        .with_tmux(sessions.tmux_env().ok()),
    );
    let input = provision::normalize(CreateProject {
        name: "p".into(),
        repos: vec![RepoSource {
            url: "https://github.com/quark-systems/quark.git".into(),
            name: None,
        }],
        agent_config: Some(AgentConfig {
            harness: "claude-code".into(),
            model: None,
            effort: None,
        }),
        ..Default::default()
    })
    .unwrap();
    let project = store.create_project(input).unwrap();
    let layout = Layout::new(dir.path().join("home"));
    provision::provision(
        store.clone(),
        engine.clone(),
        sessions.clone(),
        layout,
        project.id.clone(),
    )
    .await;

    let p = store.get_project(&project.id).unwrap();
    assert_eq!(p.status, ProjectStatus::Ready, "{:?}", p.status_detail);
    let target = format!("quark:{}", project.id);
    assert_eq!(
        store.coordinator_terminals().unwrap(),
        vec![(project.id.clone(), target.clone())]
    );
    let terminals = sessions.list(&project.id);
    assert_eq!(terminals.len(), 1, "{terminals:?}");
    assert_eq!(terminals[0].id, project.id);
    assert_eq!(terminals[0].role, TerminalRole::Coordinator);

    // A restarted daemon has no mapping until the projector restores it.
    sessions.detach_all();
    let restarted = Sessions::new("tmux", dir.path().join("run"), store.clone()).unwrap();
    assert!(restarted.list(&project.id).is_empty());
    let projector =
        quarkd::projector::Projector::new(store.clone(), engine).with_sessions(restarted.clone());
    let run = tokio::spawn(projector.run(Duration::from_secs(3600)));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while restarted.list(&project.id).is_empty() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "coordinator not restored"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    run.abort();
    assert_eq!(
        restarted.list(&project.id)[0].role,
        TerminalRole::Coordinator
    );
    restarted.server().unwrap().kill().await;
}
