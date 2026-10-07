//! The SSH runtime against a stand-in `ssh` that runs the remote command
//! line with this machine's `sh`, as an SSH server would hand it to the
//! login shell. That exercises everything Quark controls: the argv, the
//! quoting, stdin, exit codes and transport failures.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
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

/// The stand-in, written once for the whole test binary. Writing it again
/// while another test forks would leave the write handle open in that child
/// until it execs, and starting the script then fails with "Text file busy".
fn fake_ssh() -> PathBuf {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    let dir = DIR.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ssh");
        std::fs::write(&p, FAKE_SSH).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        dir
    });
    dir.path().join("ssh")
}

fn ssh(destination: &str) -> Arc<dyn Exec> {
    let options = Options {
        ssh_program: fake_ssh(),
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
    let rt = ssh("box");
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
    let rt = ssh("unreachable");
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
    let rt = ssh("box");
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
    let again = TmuxSessions::with_socket(ssh("box"), &socket);
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
        // tmux marks the pane dead when its output closes and fills in the
        // exit status once it reaps the child, so wait for both.
        if let Some(i) = l
            .iter()
            .find(|i| i.name == "sub-web" && !i.alive && i.exit_code.is_some())
        {
            exited = Some(i.exit_code);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(exited, Some(Some(5)));
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
