#![allow(dead_code)]

use std::time::Duration;

use quark_core::session::{Output, OutputStream, SessionSpec, TermSize};

pub fn spec(name: &str, argv: &[&str]) -> SessionSpec {
    SessionSpec {
        task: Some("t-1".into()),
        name: name.into(),
        cwd: std::env::temp_dir(),
        argv: argv.iter().map(|s| s.to_string()).collect(),
        env: Default::default(),
        size: TermSize { cols: 80, rows: 24 },
    }
}

/// A short temp dir: Unix socket paths are limited to about 100 bytes.
pub fn short_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("qs")
        .tempdir_in("/tmp")
        .unwrap()
}

/// Reads `stream` into `screen` until `want` is on it.
pub async fn read_until(
    stream: &mut Box<dyn OutputStream>,
    screen: &mut vt100::Parser,
    want: &str,
) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !screen.screen().contents().contains(want) {
            match stream.next().await {
                Some(Output::Bytes { data }) => screen.process(&data),
                other => panic!("stream ended before {want:?}: {other:?}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "timed out waiting for {want:?}: {:?}",
            screen.screen().contents()
        )
    });
}

/// Reads `stream` until the session ends; returns its exit code.
pub async fn read_exit(stream: &mut Box<dyn OutputStream>) -> Option<i32> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match stream.next().await {
                Some(Output::Exited { code }) => return code,
                Some(_) => {}
                None => panic!("stream ended without an exit"),
            }
        }
    })
    .await
    .expect("timed out waiting for the session to end")
}
