//! The tmux control-mode protocol and one control client.
//!
//! A control client (`tmux -C attach -t <session>`) reads commands from stdin,
//! one per line, and writes two kinds of lines to stdout:
//!
//! - reply blocks: `%begin <time> <num> <flags>`, the command's output lines,
//!   then `%end` or `%error` with the same number. A flags value with bit 0
//!   set means the command came from this client; replies arrive in the
//!   order commands were sent.
//! - notifications outside blocks: `%output %<pane> <escaped bytes>`,
//!   `%extended-output %<pane> <age> ... : <escaped bytes>`, `%pause`,
//!   window and session changes, `%exit`.
//!
//! The reader runs every line through one task, so a command reply is handled
//! in order with the output around it. Screen snapshots rely on that: output
//! that arrives before a `capture-pane` reply is already part of the capture.

use std::collections::VecDeque;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::oneshot;

/// How often an idle control client pokes the server; see [`keepalive`].
const KEEPALIVE: std::time::Duration = std::time::Duration::from_secs(1);

/// One parsed control-mode line outside a reply block.
#[derive(Debug, PartialEq, Eq)]
pub enum Notification {
    Begin {
        num: u64,
        own: bool,
    },
    End {
        num: u64,
        error: bool,
    },
    Output {
        pane: String,
        data: Vec<u8>,
    },
    Pause {
        pane: String,
    },
    /// Windows, panes or sessions came or went, or were renamed or resized.
    Layout,
    Exit,
    Other,
}

pub fn parse(line: &[u8]) -> Notification {
    let (word, rest) = split_word(line);
    match word {
        b"%begin" | b"%end" | b"%error" => {
            let mut fields = rest.split(|&b| b == b' ');
            let _time = fields.next();
            let num = fields
                .next()
                .and_then(|n| std::str::from_utf8(n).ok()?.parse().ok())
                .unwrap_or(0);
            let flags: u32 = fields
                .next()
                .and_then(|n| std::str::from_utf8(n).ok()?.parse().ok())
                .unwrap_or(0);
            if word == b"%begin" {
                Notification::Begin {
                    num,
                    own: flags & 1 == 1,
                }
            } else {
                Notification::End {
                    num,
                    error: word == b"%error",
                }
            }
        }
        b"%output" => {
            let (pane, data) = split_word(rest);
            Notification::Output {
                pane: String::from_utf8_lossy(pane).into_owned(),
                data: decode_octal(data),
            }
        }
        b"%extended-output" => {
            // %extended-output %<pane> <age> [reserved ...] : <data>
            let (pane, rest) = split_word(rest);
            let data = rest
                .windows(3)
                .position(|w| w == b" : ")
                .map(|i| &rest[i + 3..])
                .or_else(|| rest.strip_prefix(b": "))
                .unwrap_or_default();
            Notification::Output {
                pane: String::from_utf8_lossy(pane).into_owned(),
                data: decode_octal(data),
            }
        }
        b"%pause" => Notification::Pause {
            pane: String::from_utf8_lossy(rest).trim().to_string(),
        },
        b"%window-add"
        | b"%window-close"
        | b"%window-renamed"
        | b"%unlinked-window-add"
        | b"%unlinked-window-close"
        | b"%unlinked-window-renamed"
        | b"%sessions-changed"
        | b"%session-renamed"
        | b"%layout-change"
        | b"%session-window-changed" => Notification::Layout,
        b"%exit" => Notification::Exit,
        _ => Notification::Other,
    }
}

fn split_word(line: &[u8]) -> (&[u8], &[u8]) {
    match line.iter().position(|&b| b == b' ') {
        Some(i) => (&line[..i], &line[i + 1..]),
        None => (line, &[]),
    }
}

/// Decodes control-mode escaping: `\ooo` octal escapes become bytes, every
/// other byte passes through.
pub fn decode_octal(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'\\'
            && i + 3 < input.len()
            && input[i + 1..i + 4]
                .iter()
                .all(|b| (b'0'..=b'7').contains(b))
        {
            let v = (input[i + 1] - b'0') as u16 * 64
                + (input[i + 2] - b'0') as u16 * 8
                + (input[i + 3] - b'0') as u16;
            out.push(v as u8);
            i += 4;
        } else {
            out.push(input[i]);
            i += 1;
        }
    }
    out
}

/// A finished command reply.
#[derive(Debug, Clone)]
pub struct Reply {
    pub ok: bool,
    pub lines: Vec<Vec<u8>>,
}

impl Reply {
    pub fn error_text(&self) -> String {
        self.lines
            .iter()
            .map(|l| String::from_utf8_lossy(l))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// What to do with the reply to a command, in the order commands were sent.
pub enum Waiter<T> {
    /// Hand the reply to an async caller.
    Caller(oneshot::Sender<Reply>),
    /// Hand the reply to the reader's handler, in stream order.
    Reader(T),
    Discard,
}

/// Receives everything a control client reads, on the reader task.
pub trait Handler: Send + 'static {
    /// Context attached to [`Waiter::Reader`] commands.
    type Tag: Send + 'static;
    fn output(&mut self, pane: &str, data: Vec<u8>);
    fn pause(&mut self, client: &ControlClient<Self::Tag>, pane: &str);
    fn reply(&mut self, tag: Self::Tag, reply: Reply);
    fn layout(&mut self);
    /// The client exited or its stdout closed.
    fn closed(&mut self);
}

/// The write side of one control client. Cheap to clone.
pub struct ControlClient<T> {
    inner: Arc<Inner<T>>,
}

impl<T> Clone for ControlClient<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

struct Inner<T> {
    stdin: tokio::sync::Mutex<ChildStdin>,
    pending: Mutex<VecDeque<Waiter<T>>>,
    child: Mutex<Option<Child>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error("tmux control client is gone")]
    Closed,
    #[error("tmux: {0}")]
    Tmux(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl<T: Send + 'static> ControlClient<T> {
    /// Attaches a control client to `session` on the server at `socket`, and
    /// starts the reader task that feeds `handler`.
    ///
    /// The client sets `ignore-size`, so it never changes window sizes, and
    /// `pause-after`, so a pane whose output this client falls behind on is
    /// paused (and resynchronized by the handler) instead of buffering
    /// without bound inside tmux.
    pub fn attach<H>(
        tmux: &Path,
        socket: &Path,
        session_id: &str,
        pause_after_secs: u32,
        handler: H,
    ) -> std::io::Result<Self>
    where
        H: Handler<Tag = T>,
    {
        let mut child = Command::new(tmux)
            .arg("-S")
            .arg(socket)
            .args(["-C", "attach-session", "-f"])
            .arg(format!("ignore-size,pause-after={pause_after_secs}"))
            .args(["-t", session_id])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let client = ControlClient {
            inner: Arc::new(Inner {
                stdin: tokio::sync::Mutex::new(stdin),
                pending: Mutex::new(VecDeque::new()),
                child: Mutex::new(Some(child)),
            }),
        };
        tokio::spawn(read_loop(stdout, client.clone(), handler));
        tokio::spawn(keepalive(Arc::downgrade(&client.inner)));
        Ok(client)
    }

    /// Sends one command line. Its reply goes to `waiter`.
    pub async fn send(&self, command: &str, waiter: Waiter<T>) -> Result<(), ControlError> {
        debug_assert!(!command.contains('\n'));
        let mut stdin = self.inner.stdin.lock().await;
        // Queue the waiter before writing so the reply can never arrive first.
        self.inner.pending.lock().unwrap().push_back(waiter);
        let mut line = Vec::with_capacity(command.len() + 1);
        line.extend_from_slice(command.as_bytes());
        line.push(b'\n');
        if let Err(e) = async {
            stdin.write_all(&line).await?;
            stdin.flush().await
        }
        .await
        {
            self.inner.pending.lock().unwrap().pop_back();
            return Err(if e.kind() == std::io::ErrorKind::BrokenPipe {
                ControlError::Closed
            } else {
                e.into()
            });
        }
        Ok(())
    }

    /// Sends a command and waits for its reply; a `%error` reply is an error.
    pub async fn run(&self, command: &str) -> Result<Reply, ControlError> {
        let (tx, rx) = oneshot::channel();
        self.send(command, Waiter::Caller(tx)).await?;
        let reply = rx.await.map_err(|_| ControlError::Closed)?;
        if reply.ok {
            Ok(reply)
        } else {
            Err(ControlError::Tmux(reply.error_text()))
        }
    }

    pub fn is_closed(&self) -> bool {
        match self.inner.child.lock().unwrap().as_mut() {
            Some(child) => !matches!(child.try_wait(), Ok(None)),
            None => true,
        }
    }

    /// Detaches by killing the client process. The tmux server and its
    /// panes keep running.
    pub fn detach(&self) {
        if let Some(mut child) = self.inner.child.lock().unwrap().take() {
            let _ = child.start_kill();
        }
    }
}

/// Sends a no-op command every [`KEEPALIVE`] while the client lives.
///
/// tmux 3.4 checks how far behind a control client is only when it has
/// something else to write to it. A pane that falls behind at the end of a
/// burst of output is then paused without a `%pause` notification until the
/// client's next command, and its last output never arrives. Regular
/// commands make tmux report the pause, so the handler can resync.
async fn keepalive<T: Send + 'static>(inner: std::sync::Weak<Inner<T>>) {
    let mut tick = tokio::time::interval(KEEPALIVE);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await;
    loop {
        tick.tick().await;
        let Some(inner) = inner.upgrade() else { return };
        let client = ControlClient { inner };
        if client.is_closed()
            || client
                .send("display-message -p ''", Waiter::Discard)
                .await
                .is_err()
        {
            return;
        }
    }
}

async fn read_loop<H: Handler>(stdout: ChildStdout, client: ControlClient<H::Tag>, mut handler: H) {
    let mut reader = BufReader::with_capacity(256 * 1024, stdout);
    let mut line = Vec::new();
    // The reply block being collected, if it answers one of our commands.
    let mut block: Option<(u64, Vec<Vec<u8>>)> = None;
    // A block from another source (the attach itself), skipped whole.
    let mut foreign: Option<u64> = None;
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if let Some((num, lines)) = block.as_mut() {
            match parse(&line) {
                Notification::End { num: n, error } if n == *num => {
                    let lines = std::mem::take(lines);
                    block = None;
                    let waiter = client.inner.pending.lock().unwrap().pop_front();
                    let reply = Reply { ok: !error, lines };
                    match waiter {
                        Some(Waiter::Caller(tx)) => {
                            let _ = tx.send(reply);
                        }
                        Some(Waiter::Reader(tag)) => handler.reply(tag, reply),
                        Some(Waiter::Discard) | None => {
                            if !reply.ok {
                                tracing::debug!(error = %reply.error_text(), "tmux command failed");
                            }
                        }
                    }
                }
                _ => lines.push(line.clone()),
            }
            continue;
        }
        if let Some(num) = foreign {
            if matches!(parse(&line), Notification::End { num: n, .. } if n == num) {
                foreign = None;
            }
            continue;
        }
        match parse(&line) {
            Notification::Begin { num, own: true } => block = Some((num, Vec::new())),
            Notification::Begin { num, own: false } => foreign = Some(num),
            Notification::Output { pane, data } => handler.output(&pane, data),
            Notification::Pause { pane } => handler.pause(&client, &pane),
            Notification::Layout => handler.layout(),
            Notification::Exit => break,
            Notification::End { .. } | Notification::Other => {}
        }
    }
    // Fail every command still waiting.
    client.inner.pending.lock().unwrap().clear();
    handler.closed();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_octal_escapes() {
        assert_eq!(decode_octal(br"a\033[0m\015\012b"), b"a\x1b[0m\r\nb");
        assert_eq!(decode_octal(br"\134"), b"\\");
        assert_eq!(decode_octal(br"tail\0"), b"tail\\0");
        assert_eq!(decode_octal("é".as_bytes()), "é".as_bytes());
    }

    #[test]
    fn parses_notifications() {
        assert_eq!(
            parse(b"%begin 1790946056 278 1"),
            Notification::Begin {
                num: 278,
                own: true
            }
        );
        assert_eq!(
            parse(b"%begin 1790946056 272 0"),
            Notification::Begin {
                num: 272,
                own: false
            }
        );
        assert_eq!(
            parse(b"%error 1790946056 282 1"),
            Notification::End {
                num: 282,
                error: true
            }
        );
        assert_eq!(
            parse(br"%output %3 hi\015\012"),
            Notification::Output {
                pane: "%3".into(),
                data: b"hi\r\n".to_vec()
            }
        );
        assert_eq!(
            parse(br"%extended-output %1 0 : echo hi\015"),
            Notification::Output {
                pane: "%1".into(),
                data: b"echo hi\r".to_vec()
            }
        );
        assert_eq!(
            parse(br"%extended-output %1 12 : a : b"),
            Notification::Output {
                pane: "%1".into(),
                data: b"a : b".to_vec()
            }
        );
        assert_eq!(
            parse(b"%pause %7"),
            Notification::Pause { pane: "%7".into() }
        );
        assert_eq!(parse(b"%window-add @4"), Notification::Layout);
        assert_eq!(parse(b"%exit"), Notification::Exit);
        assert_eq!(parse(b"%session-changed $0 quark"), Notification::Other);
    }
}
