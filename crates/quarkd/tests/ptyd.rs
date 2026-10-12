//! `quarkd ptyd`: quarkd runs the native engine's PTY supervisor itself, so
//! `cargo run -p quarkd -- --engine native` needs no second binary.

use std::path::Path;

use quark_core::session::{SessionBackend, SessionSpec, TermSize};
use quark_sessions::pty::PtyClient;

const QUARKD: &str = env!("CARGO_BIN_EXE_quarkd");

/// Stops the supervisor serving `socket`, found by its unique socket path.
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
async fn quarkd_serves_the_pty_supervisor() {
    // A short path: a unix socket path has a small length limit.
    let dir = tempfile::Builder::new()
        .prefix("qp")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = dir.path().join("run").join("ptyd.sock");
    let _stop = Stop(&socket);

    let client = PtyClient::start_with(&socket, Path::new(QUARKD), &["ptyd"])
        .await
        .unwrap();
    assert!(client.list().await.unwrap().is_empty());
    let info = client
        .create(&SessionSpec {
            task: None,
            name: "w".into(),
            cwd: dir.path().to_path_buf(),
            argv: vec!["/bin/sh".into()],
            env: Default::default(),
            size: TermSize { cols: 80, rows: 24 },
        })
        .await
        .unwrap();
    let listed = client.list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, info.id);
    assert!(listed[0].alive);
}
