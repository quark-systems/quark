//! `quark-ptyd` as its own process: sessions outlive the client that
//! started them, as they must outlive a daemon crash.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{read_exit, read_until, short_dir, spec};
use quark_core::session::{SessionBackend, TermSize};
use quark_core::CoreError;
use quark_sessions::pty::PtyClient;

const PTYD: &str = env!("CARGO_BIN_EXE_quark-ptyd");

/// Stops the `quark-ptyd` serving `socket`, found by its unique socket path.
struct Stop<'a>(&'a Path);

impl Drop for Stop<'_> {
    fn drop(&mut self) {
        let _ = std::process::Command::new("pkill")
            .arg("-f")
            .arg(self.0)
            .status();
    }
}

#[tokio::test]
async fn sessions_outlive_the_client() {
    let dir = short_dir();
    let socket = dir.path().join("run").join("ptyd.sock");
    let _stop = Stop(&socket);

    let first = PtyClient::start(&socket, Path::new(PTYD)).await.unwrap();
    let info = first.create(&spec("w", &["/bin/sh"])).await.unwrap();
    first
        .input(&info.id, b"echo first-$((40+2))\n")
        .await
        .unwrap();
    {
        let mut stream = first.attach(&info.id).await.unwrap();
        let mut screen = vt100::Parser::new(24, 80, 0);
        read_until(&mut stream, &mut screen, "first-42").await;
    }
    // The daemon goes away mid-stream.
    drop(first);

    // A new one finds the supervisor already running and the session in it.
    let second = PtyClient::start(&socket, Path::new(PTYD)).await.unwrap();
    let listed = second.list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, info.id);
    assert_eq!(listed[0].task, Some("t-1".into()));
    assert!(listed[0].alive);

    let mut stream = second.attach(&info.id).await.unwrap();
    let mut screen = vt100::Parser::new(24, 80, 0);
    read_until(&mut stream, &mut screen, "first-42").await;
    second
        .resize(
            &info.id,
            TermSize {
                cols: 100,
                rows: 30,
            },
        )
        .await
        .unwrap();
    let snap = second.snapshot(&info.id).await.unwrap();
    assert_eq!(
        snap.size,
        TermSize {
            cols: 100,
            rows: 30
        }
    );
    second.input(&info.id, b"exit 7\n").await.unwrap();
    assert_eq!(read_exit(&mut stream).await, Some(7));

    second.kill(&info.id).await.unwrap();
    assert!(matches!(
        second.snapshot(&info.id).await,
        Err(CoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn refuses_a_second_supervisor_and_reports_absence() {
    let dir = short_dir();
    let socket = dir.path().join("ptyd.sock");
    let _stop = Stop(&socket);

    assert!(matches!(
        PtyClient::new(&socket).list().await,
        Err(CoreError::Backend(_))
    ));
    PtyClient::start(&socket, Path::new(PTYD)).await.unwrap();
    let out = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(PTYD)
            .arg("--socket")
            .arg(&socket)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("another supervisor"));
    // The first one is still serving.
    assert!(PtyClient::new(&socket).list().await.unwrap().is_empty());
}
