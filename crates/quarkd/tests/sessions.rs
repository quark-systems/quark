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

fn shell(name: &str, role: Option<&str>) -> WindowSpec {
    WindowSpec {
        name: name.into(),
        role: role.map(str::to_string),
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
        })
        .unwrap();
    let mut rx = store.subscribe();
    let sessions = Sessions::new("tmux", dir.path().join("run"), store.clone());
    let server = sessions.server(&project.id).unwrap();

    // The coordinator window, then a worker window as firstmate would name it.
    sessions
        .start_window(&project.id, &shell("coordinator", Some("coordinator")))
        .await
        .unwrap();
    sessions
        .start_window(&project.id, &shell("fm-fix-login", None))
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
    let sessions = Sessions::new("tmux", dir.path().join("run"), store.clone());
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
        })
        .unwrap();
    let mut rx = store.subscribe();
    let sessions = Sessions::new("tmux", dir.path().join("run"), store.clone());
    let server = sessions.server(&project.id).unwrap();
    sessions
        .start_window(&project.id, &shell("coordinator", Some("coordinator")))
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
