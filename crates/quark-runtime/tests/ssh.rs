//! The SSH runtime against a stand-in `ssh` that runs the remote command
//! line with this machine's `sh`, as an SSH server would hand it to the
//! login shell. That exercises everything Quark controls: the argv, the
//! quoting, stdin, exit codes and transport failures.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use quark_core::host::Health;
use quark_core::session::{SessionBackend, SessionId, SessionSpec, TermSize};
use quark_core::HostId;
use quark_runtime::{connect, fs, Cmd, Exec, Options, RuntimeSpec, SshTarget, TmuxSessions};

const FAKE_SSH: &str = r#"#!/bin/sh
while [ $# -gt 2 ]; do shift; done
[ "$1" = unreachable ] && { echo "ssh: connect to host unreachable: Connection refused" >&2; exit 255; }
exec sh -c "$2"
"#;

fn fake_ssh(dir: &Path) -> PathBuf {
    let p = dir.join("ssh");
    std::fs::write(&p, FAKE_SSH).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn ssh(dir: &Path, destination: &str) -> Arc<dyn Exec> {
    let options = Options {
        ssh_program: fake_ssh(dir),
        connect_timeout: Duration::from_secs(2),
    };
    connect(
        &RuntimeSpec::Ssh {
            target: SshTarget::new(destination),
        },
        HostId::from(destination),
        &options,
    )
    .unwrap()
}

#[tokio::test]
async fn commands_files_and_health_over_ssh() {
    let dir = tempfile::tempdir().unwrap();
    let rt = ssh(dir.path(), "box");
    let out = rt
        .run(
            &Cmd::new(["printf", "%s|%s|%s", "a b", "$HOME", "it's"])
                .env("UNUSED", "x y")
                .cwd(dir.path()),
        )
        .await
        .unwrap();
    assert_eq!(out.stdout_str(), "a b|$HOME|it's");
    let out = rt
        .run(&Cmd::sh("cat; exit 7", [""; 0]).stdin(b"piped".to_vec()))
        .await
        .unwrap();
    assert_eq!((out.code, out.stdout_str().as_str()), (Some(7), "piped"));

    let f = dir.path().join("home/inbox/0001 x.md");
    fs::write(rt.as_ref(), &f, b"line one\n").await.unwrap();
    assert_eq!(std::fs::read(&f).unwrap(), b"line one\n");
    assert_eq!(
        fs::read_from(rt.as_ref(), &f, 5, 100)
            .await
            .unwrap()
            .unwrap(),
        b"one\n"
    );
    assert_eq!(
        fs::list(rt.as_ref(), &dir.path().join("home/inbox"))
            .await
            .unwrap(),
        ["0001 x.md"]
    );
    assert_eq!(rt.health().await.unwrap(), Health::Healthy);
}

#[tokio::test]
async fn a_failed_connection_is_unknown_not_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let rt = ssh(dir.path(), "unreachable");
    let err = rt.run(&Cmd::new(["true"])).await.unwrap_err();
    assert!(err.to_string().contains("outcome is unknown"), "{err}");
    assert!(matches!(
        rt.health().await.unwrap(),
        Health::Unreachable { .. }
    ));
}

async fn wait_for(b: &TmuxSessions, id: &SessionId, what: &str) {
    let mut screen = String::new();
    for _ in 0..50 {
        screen = String::from_utf8_lossy(&b.snapshot(id).await.unwrap().bytes).into_owned();
        if screen.contains(what) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("never saw {what}: {screen}");
}

fn have_tmux() -> bool {
    std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[tokio::test]
async fn tmux_sessions_over_ssh_survive_the_connection() {
    if !have_tmux() {
        eprintln!("tmux not installed; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let rt = ssh(dir.path(), "box");
    let socket = format!("quark-test-{}", std::process::id());
    let tmux = TmuxSessions::with_socket(rt.clone(), &socket);
    let spec = SessionSpec {
        task: None,
        name: "sub-web".into(),
        cwd: dir.path().to_path_buf(),
        argv: vec![
            "sh".into(),
            "-c".into(),
            "echo \"ready $GREETING\"; while IFS= read -r l; do echo \"got:$l\"; [ \"$l\" = bye ] && exit 5; done".into(),
        ],
        env: [("GREETING".to_string(), "hi there".to_string())].into(),
        size: TermSize { cols: 100, rows: 30 },
    };
    let s = tmux.create(&spec).await.unwrap();
    // A second backend over a new "connection" sees the same session.
    let again = TmuxSessions::with_socket(ssh(dir.path(), "box"), &socket);
    wait_for(&again, &s.id, "ready hi there").await;
    again.input(&s.id, b"hello 'q'\r").await.unwrap();
    wait_for(&tmux, &s.id, "got:hello 'q'").await;
    let snap = tmux.snapshot(&s.id).await.unwrap();
    assert_eq!(
        snap.size,
        TermSize {
            cols: 100,
            rows: 30
        }
    );
    let listed = again.list().await.unwrap();
    assert!(listed.iter().any(|i| i.name == "sub-web" && i.alive));

    tmux.input(&s.id, b"bye\r").await.unwrap();
    let mut exited = None;
    for _ in 0..50 {
        let l = tmux.list().await.unwrap();
        if let Some(i) = l.iter().find(|i| i.name == "sub-web" && !i.alive) {
            exited = Some(i.exit_code);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // tmux records the exit status on a best-effort basis; when it has one,
    // it is the process's.
    assert!(matches!(exited, Some(Some(5)) | Some(None)), "{exited:?}");
    tmux.kill(&s.id).await.unwrap();
    tmux.kill(&s.id).await.unwrap();
    assert!(tmux
        .list()
        .await
        .unwrap()
        .iter()
        .all(|i| i.name != "sub-web"));
    let _ = std::process::Command::new("tmux")
        .args(["-L", &socket, "kill-server"])
        .output();
}
