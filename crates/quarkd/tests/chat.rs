//! Coordinator chat delivery through a real tmux terminal. Skipped when tmux
//! is not installed.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use quark_systems::CreateProject;
use quark_transcript::SessionRoots;
use quarkd::chat::{ChatError, CoordinatorInput, Delivery, SessionsInput};
use quarkd::engine::WorkspaceRef;
use quarkd::sessions::{Sessions, WindowSpec};
use quarkd::store::Store;

fn tmux_installed() -> bool {
    std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn claude_slug(p: &Path) -> String {
    p.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// A stand-in coordinator: reads submitted lines, strips the bracketed-paste
/// markers and records each as a Claude Code user message in `log`, the way
/// the real harness records what it is sent.
const FAKE_COORDINATOR: &str = r#"
stty -echo
printf '\033[?2004h'
while IFS= read -r line; do
  text=$(printf '%s' "$line" | sed 's/\x1b\[20[01]~//g')
  printf '{"type":"user","message":{"role":"user","content":"%s"}}\n' "$text" >> "$LOG"
done
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn messages_are_pasted_submitted_and_confirmed_from_the_log() {
    if !tmux_installed() {
        eprintln!("tmux is not installed; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let roots = SessionRoots {
        claude: vec![dir.path().join("claude")],
        codex: vec![],
        pi: vec![],
    };
    let log_dir = dir
        .path()
        .join("claude/projects")
        .join(claude_slug(&workspace));
    std::fs::create_dir_all(&log_dir).unwrap();
    let log = log_dir.join("coordinator.jsonl");
    std::fs::write(&log, "").unwrap();

    let store = Arc::new(Store::open_in_memory().unwrap());
    let project = store
        .create_project(CreateProject {
            name: "p".into(),
            goal: None,
            workspace_path: Some(workspace.to_string_lossy().into()),
            ..Default::default()
        })
        .unwrap();
    let sessions = Sessions::new("tmux", dir.path().join("run"), store.clone()).unwrap();
    let ws = WorkspaceRef {
        project_id: project.id.clone(),
        root: workspace.clone(),
    };
    let input = SessionsInput::new(sessions.clone(), roots.clone())
        .with_confirm_timeout(Duration::from_secs(5));

    // No coordinator window yet: nothing is typed anywhere.
    let err = input.send(&ws, "hello").await.unwrap_err();
    assert!(matches!(err, ChatError::Unavailable(_)), "{err:?}");

    let target = sessions
        .start_window(&WindowSpec {
            name: "fm-coord".into(),
            argv: vec!["sh".into(), "-c".into(), FAKE_COORDINATOR.into()],
            cwd: Some(workspace.clone()),
            env: vec![("LOG".into(), log.to_string_lossy().into())],
        })
        .await
        .unwrap();
    sessions
        .set_coordinator(&project.id, Some(target))
        .await
        .unwrap();
    // Let the stand-in reach its read loop.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let delivered = input
        .send(&ws, "fix #42 and add tests for the parser")
        .await
        .unwrap();
    assert_eq!(delivered, Delivery::Confirmed);
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(
        logged.contains(r#""content":"fix #42 and add tests for the parser""#),
        "{logged}"
    );

    // With no session log to confirm from, a typed message is reported
    // unconfirmed rather than delivered.
    let blind = SessionsInput::new(sessions.clone(), SessionRoots::default())
        .with_confirm_timeout(Duration::from_millis(500));
    assert_eq!(
        blind.send(&ws, "anyone there?").await.unwrap(),
        Delivery::Unconfirmed
    );

    let server = sessions.server().unwrap().clone();
    sessions.detach_all();
    server.kill().await;
}
