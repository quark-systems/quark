//! [`TmuxBackend`] against a real private tmux server. Skipped when tmux is
//! not installed.

mod common;

use common::{read_exit, read_until, short_dir, spec};
use quark_core::session::{SessionBackend, TermSize};
use quark_core::CoreError;
use quark_sessions::tmux::server::Server;
use quark_sessions::tmux::TmuxBackend;

fn tmux_installed() -> bool {
    std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[tokio::test]
async fn drives_sessions_and_finds_them_after_a_restart() {
    if !tmux_installed() {
        eprintln!("tmux is not installed; skipping");
        return;
    }
    let dir = short_dir();
    let server = Server::new("tmux", dir.path().join("tmux").join("quark")).unwrap();
    let tmux = TmuxBackend::new(server.clone());

    let info = tmux.create(&spec("worker-1", &["/bin/sh"])).await.unwrap();
    assert!(info.id.0.starts_with('%'));
    let mut stream = tmux.attach(&info.id).await.unwrap();
    let mut screen = vt100::Parser::new(24, 80, 0);
    tmux.input(&info.id, b"echo tmux-$((40+2))\n")
        .await
        .unwrap();
    read_until(&mut stream, &mut screen, "tmux-42").await;

    let snap = tmux.snapshot(&info.id).await.unwrap();
    assert_eq!(snap.size, TermSize { cols: 80, rows: 24 });
    let mut fresh = vt100::Parser::new(24, 80, 0);
    fresh.process(&snap.bytes);
    assert!(fresh.screen().contents().contains("tmux-42"));

    tmux.resize(
        &info.id,
        TermSize {
            cols: 100,
            rows: 30,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        tmux.snapshot(&info.id).await.unwrap().size,
        TermSize {
            cols: 100,
            rows: 30
        }
    );

    // A daemon restart: a fresh backend on the same server finds the session
    // and the task it runs for.
    tmux.detach_all();
    let again = TmuxBackend::new(server.clone());
    let found = again
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.id == info.id)
        .expect("session survives");
    assert_eq!(found.name, "worker-1");
    assert_eq!(found.task, Some("t-1".into()));

    let mut stream = again.attach(&info.id).await.unwrap();
    let mut screen = vt100::Parser::new(30, 100, 0);
    read_until(&mut stream, &mut screen, "tmux-42").await;
    again.kill(&info.id).await.unwrap();
    assert_eq!(read_exit(&mut stream).await, None);
    assert!(matches!(
        again.snapshot(&info.id).await,
        Err(CoreError::NotFound(_))
    ));
    server.kill().await;
}
