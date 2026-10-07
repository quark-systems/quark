use std::time::Duration;

use quark_core::session::Output;

use super::*;

pub(crate) fn spec(argv: &[&str]) -> SessionSpec {
    SessionSpec {
        task: Some("t-1".into()),
        name: "w".into(),
        cwd: std::env::temp_dir(),
        argv: argv.iter().map(|s| s.to_string()).collect(),
        env: [("QUARK_TEST".to_string(), "hello".to_string())].into(),
        size: TermSize { cols: 40, rows: 10 },
    }
}

/// Reads the stream into a screen model until `want` shows up on it.
pub(crate) async fn read_until(
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

#[tokio::test]
async fn runs_a_shell_and_streams_its_screen() {
    let sup = PtySupervisor::new();
    let info = sup.create(&spec(&["/bin/sh"])).await.unwrap();
    assert!(info.alive);
    assert_eq!(info.task, Some("t-1".into()));
    let mut stream = sup.attach(&info.id).await.unwrap();
    let mut screen = vt100::Parser::new(10, 40, 0);
    sup.input(&info.id, b"echo \"$QUARK_TEST $TERM\"\n")
        .await
        .unwrap();
    read_until(&mut stream, &mut screen, "hello xterm-256color").await;

    // A snapshot replayed into a fresh emulator shows the same screen as
    // the stream, once the stream has caught up with it.
    let snap = sup.snapshot(&info.id).await.unwrap();
    assert_eq!(snap.size, TermSize { cols: 40, rows: 10 });
    let mut fresh = vt100::Parser::new(10, 40, 0);
    fresh.process(&snap.bytes);
    let want = fresh.screen().contents();
    read_until(&mut stream, &mut screen, &want).await;
    assert_eq!(fresh.screen().contents(), screen.screen().contents());
    assert_eq!(
        fresh.screen().cursor_position(),
        screen.screen().cursor_position()
    );

    // A late viewer starts from a repaint of the current screen.
    let mut late = sup.attach(&info.id).await.unwrap();
    let mut late_screen = vt100::Parser::new(10, 40, 0);
    read_until(&mut late, &mut late_screen, "hello xterm-256color").await;

    sup.resize(&info.id, TermSize { cols: 60, rows: 20 })
        .await
        .unwrap();
    sup.input(&info.id, b"stty size\n").await.unwrap();
    read_until(&mut stream, &mut screen, "20 60").await;

    sup.input(&info.id, b"exit 3\n").await.unwrap();
    let code = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match stream.next().await {
                Some(Output::Exited { code }) => return code,
                Some(_) => {}
                None => panic!("stream ended without an exit"),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(code, Some(3));
    assert!(stream.next().await.is_none());
    let listed = sup.list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert!(!listed[0].alive);
    assert_eq!(listed[0].exit_code, Some(3));
    assert!(matches!(
        sup.input(&info.id, b"x").await,
        Err(CoreError::Refused(_))
    ));
    sup.kill(&info.id).await.unwrap();
    assert!(sup.list().await.unwrap().is_empty());
}

#[tokio::test]
async fn kill_ends_the_program() {
    let sup = PtySupervisor::new();
    let info = sup.create(&spec(&["sleep", "600"])).await.unwrap();
    let mut stream = sup.attach(&info.id).await.unwrap();
    sup.kill(&info.id).await.unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match stream.next().await {
                Some(Output::Exited { code }) => return code,
                Some(_) => {}
                None => panic!("stream ended without an exit"),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(ended, Some(128 + 1), "SIGHUP");
    assert!(matches!(
        sup.snapshot(&info.id).await,
        Err(CoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn refuses_bad_specs() {
    let sup = PtySupervisor::new();
    let mut s = spec(&["/bin/sh"]);
    s.size = TermSize { cols: 1, rows: 10 };
    assert!(matches!(sup.create(&s).await, Err(CoreError::Invalid(_))));
    let s = spec(&["/definitely/not/a/program"]);
    assert!(matches!(sup.create(&s).await, Err(CoreError::Backend(_))));
    assert!(matches!(
        sup.attach(&SessionId("nope".into())).await,
        Err(CoreError::NotFound(_))
    ));
}
