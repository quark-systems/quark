//! The file fallback, for harnesses with neither MCP nor hooks.
//!
//! The worker appends one line per message to a status file quarkd names in
//! its instructions, in firstmate's status grammar so existing briefs keep
//! working:
//!
//! ```text
//! working: starting on the parser
//! blocked: the staging token expired
//! needs-decision [key=api-shape]: REST or gRPC? I recommend REST.
//! learned: integration tests need tmux 3.2 or newer
//! done: PR https://github.com/o/r/pull/12 checks green
//! {"tool": "learned", "fact": "a JSON worker message also works"}
//! ```
//!
//! [`StatusFile::poll`] reads complete lines past its cursor and records
//! each one. Event ids are derived from the task, generation, byte offset
//! and line, so re-reading the file from the start (after a restart)
//! records nothing twice.

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use quark_core::worker::{Transport, WorkerMessage};
use quark_core::{CoreError, EventId, Result};
use uuid::Uuid;

use crate::recorder::{Recorder, WorkerIdentity};

/// Parses one status line. `Ok(None)` for blank lines and `#` comments.
///
/// `<verb>[ corr=<id>][ [key=<slug>]]: <note>` maps by verb: `done` to
/// [`WorkerMessage::Done`] (the first URL in the note is the pull request),
/// `needs-decision` or `ask` to [`WorkerMessage::Ask`] (key `default` when
/// none is stated, before the colon or at the head of the note), `learned`
/// to [`WorkerMessage::Learned`], and any other verb to
/// [`WorkerMessage::Report`] with the verb as the state. A line starting
/// with `{` is a JSON worker message.
pub fn parse_status_line(line: &str) -> Result<Option<WorkerMessage>> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return Ok(None);
    }
    if line.starts_with('{') {
        return serde_json::from_str(line)
            .map(Some)
            .map_err(|e| CoreError::Invalid(format!("status line: {e}")));
    }
    let Some((head, note)) = line.split_once(':') else {
        return Err(CoreError::Invalid(format!(
            "status line has no `<state>:` prefix: {line}"
        )));
    };
    let mut words = head.split_whitespace();
    let verb = words.next().unwrap_or("").to_ascii_lowercase();
    if verb.is_empty() {
        return Err(CoreError::Invalid(format!(
            "status line has no state: {line}"
        )));
    }
    let mut key = words.find_map(key_token);
    let mut note = note.trim();
    if let Some(rest) = note.strip_prefix("[key=") {
        if let Some((k, after)) = rest.split_once(']') {
            if valid_key(k) {
                if key.is_none() {
                    key = Some(k.to_string());
                }
                note = after.trim();
            }
        }
    }
    let note = note.to_string();
    Ok(Some(match verb.as_str() {
        "done" => WorkerMessage::Done {
            pull_request: first_url(&note),
            summary: if note.is_empty() { "done".into() } else { note },
        },
        "needs-decision" | "ask" => WorkerMessage::Ask {
            key: key.unwrap_or_else(|| "default".into()),
            question: note,
        },
        "learned" => WorkerMessage::Learned { fact: note },
        _ => WorkerMessage::Report { state: verb, note },
    }))
}

fn key_token(word: &str) -> Option<String> {
    let k = word.strip_prefix("[key=")?.strip_suffix(']')?;
    valid_key(k).then(|| k.to_string())
}

fn valid_key(k: &str) -> bool {
    !k.is_empty()
        && k.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn first_url(note: &str) -> Option<String> {
    note.split_whitespace()
        .find(|w| w.starts_with("https://") || w.starts_with("http://"))
        .map(|w| w.trim_end_matches(['.', ',', ')', ';']).to_string())
}

/// What one [`StatusFile::poll`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Polled {
    /// Lines recorded in the log.
    pub recorded: usize,
    /// Lines that did not parse; skipped with a warning.
    pub invalid: usize,
    /// The worker's generation was replaced. Its lines are still recorded;
    /// the caller should stop polling this file.
    pub replaced: bool,
}

/// One worker's status file and how far it has been read.
#[derive(Debug, Clone)]
pub struct StatusFile {
    path: PathBuf,
    who: WorkerIdentity,
    offset: u64,
}

impl StatusFile {
    /// A reader starting at the beginning of `path`.
    pub fn new(path: impl Into<PathBuf>, who: WorkerIdentity) -> Self {
        Self {
            path: path.into(),
            who,
            offset: 0,
        }
    }

    /// Bytes consumed so far; only whole lines are consumed.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Records every complete line past the cursor. A missing file is
    /// empty; a file shorter than the cursor was replaced and is read from
    /// the start. Stops without advancing on a log failure, so the next
    /// poll retries the same line.
    pub async fn poll(&mut self, recorder: &Recorder) -> Result<Polled> {
        let (start, bytes) = self.read_new()?;
        self.offset = start;
        let mut polled = Polled::default();
        let mut at = start;
        let mut rest = &bytes[..];
        while let Some(end) = rest.iter().position(|b| *b == b'\n') {
            let raw = &rest[..end];
            let line = String::from_utf8_lossy(raw);
            match parse_status_line(&line) {
                Ok(None) => {}
                Ok(Some(message)) => {
                    let id = line_id(&self.who, at, raw);
                    match recorder
                        .receive_as(id, self.who.envelope(Transport::File, message))
                        .await
                    {
                        Ok(_) => polled.recorded += 1,
                        Err(CoreError::Refused(_)) => {
                            polled.recorded += 1;
                            polled.replaced = true;
                        }
                        Err(CoreError::Invalid(msg)) => {
                            tracing::warn!(path = %self.path.display(), "skipped status line: {msg}");
                            polled.invalid += 1;
                        }
                        Err(e) => return Err(e),
                    }
                }
                Err(e) => {
                    tracing::warn!(path = %self.path.display(), "skipped status line: {e}");
                    polled.invalid += 1;
                }
            }
            at += end as u64 + 1;
            self.offset = at;
            rest = &rest[end + 1..];
        }
        Ok(polled)
    }

    /// Where reading starts and the bytes from there to the end.
    fn read_new(&self) -> Result<(u64, Vec<u8>)> {
        let mut file = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((0, Vec::new())),
            Err(e) => return Err(io(&self.path, e)),
        };
        let len = file.metadata().map_err(|e| io(&self.path, e))?.len();
        let start = if len < self.offset { 0 } else { self.offset };
        file.seek(SeekFrom::Start(start))
            .map_err(|e| io(&self.path, e))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|e| io(&self.path, e))?;
        Ok((start, bytes))
    }

    /// Polls every `every` until the worker is replaced or the task is gone.
    /// Log failures are retried on the next tick.
    pub fn spawn(
        mut self,
        recorder: Arc<Recorder>,
        every: Duration,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                match self.poll(&recorder).await {
                    Ok(p) if p.replaced => return,
                    Ok(_) => {}
                    Err(CoreError::NotFound(_)) => return,
                    Err(e) => tracing::warn!(path = %self.path.display(), "status file: {e}"),
                }
            }
        })
    }
}

fn io(path: &std::path::Path, e: std::io::Error) -> CoreError {
    CoreError::Backend(format!("{}: {e}", path.display()))
}

/// A stable id for the line at `offset`: FNV-1a over the identity, offset
/// and bytes, as a UUIDv8. Stable across processes and Rust versions.
fn line_id(who: &WorkerIdentity, offset: u64, line: &[u8]) -> EventId {
    const BASIS: u128 = 0x6c62272e07bb014262b821756295c58d;
    const PRIME: u128 = 0x0000000001000000000000000000013b;
    let mut h = BASIS;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            h ^= u128::from(*b);
            h = h.wrapping_mul(PRIME);
        }
    };
    feed(b"quark-worker/status-line\0");
    feed(who.task.as_str().as_bytes());
    feed(b"\0");
    feed(who.generation.as_bytes());
    feed(b"\0");
    feed(&offset.to_le_bytes());
    feed(line);
    EventId(Uuid::new_v8(h.to_be_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::tests::setup;
    use quark_core::worker::WorkerEnvelope;
    use std::io::Write;

    fn parse(line: &str) -> WorkerMessage {
        parse_status_line(line).unwrap().unwrap()
    }

    #[test]
    fn parses_firstmate_status_lines() {
        assert_eq!(
            parse("blocked: staging token expired"),
            WorkerMessage::Report {
                state: "blocked".into(),
                note: "staging token expired".into()
            }
        );
        assert_eq!(
            parse("done: PR https://github.com/o/r/pull/12. checks green"),
            WorkerMessage::Done {
                summary: "PR https://github.com/o/r/pull/12. checks green".into(),
                pull_request: Some("https://github.com/o/r/pull/12".into())
            }
        );
        let ask = |key: &str| WorkerMessage::Ask {
            key: key.into(),
            question: "REST or gRPC?".into(),
        };
        assert_eq!(
            parse("needs-decision [key=api-shape]: REST or gRPC?"),
            ask("api-shape")
        );
        assert_eq!(
            parse("needs-decision: [key=api-shape] REST or gRPC?"),
            ask("api-shape")
        );
        assert_eq!(
            parse("needs-decision corr=0123456789abcdef [key=api-shape]: REST or gRPC?"),
            ask("api-shape")
        );
        assert_eq!(parse("ask: REST or gRPC?"), ask("default"));
        assert_eq!(
            parse("learned: tests need tmux"),
            WorkerMessage::Learned {
                fact: "tests need tmux".into()
            }
        );
        assert_eq!(
            parse(r#"{"tool": "signal", "signal": "turn_end"}"#),
            WorkerMessage::Signal {
                signal: "turn_end".into()
            }
        );
        assert_eq!(parse_status_line("  ").unwrap(), None);
        assert_eq!(parse_status_line("# comment").unwrap(), None);
        assert!(parse_status_line("no colon here").is_err());
    }

    #[tokio::test]
    async fn polls_whole_lines_once() {
        let (rec, log, _) = setup();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t1.status");
        let mut sf = StatusFile::new(&path, WorkerIdentity::new("t1", "g2"));
        assert_eq!(sf.poll(&rec).await.unwrap(), Polled::default());

        let mut f = std::fs::File::create(&path).unwrap();
        write!(f, "working: start\nnot a line\nlearned: half").unwrap();
        let p = sf.poll(&rec).await.unwrap();
        assert_eq!((p.recorded, p.invalid), (1, 1));
        assert_eq!(sf.offset(), "working: start\nnot a line\n".len() as u64);

        writeln!(f, " a fact").unwrap();
        assert_eq!(sf.poll(&rec).await.unwrap().recorded, 1);
        assert_eq!(sf.poll(&rec).await.unwrap().recorded, 0);
        assert_eq!(log.events().len(), 2);
        let last: WorkerEnvelope = log.events()[1].decode().unwrap();
        assert_eq!(last.via, Transport::File);
        assert_eq!(
            last.message,
            WorkerMessage::Learned {
                fact: "half a fact".into()
            }
        );

        // A restart rereads from the start and records nothing new.
        let mut again = StatusFile::new(&path, WorkerIdentity::new("t1", "g2"));
        assert_eq!(again.poll(&rec).await.unwrap().recorded, 2);
        assert_eq!(log.events().len(), 2);
    }

    #[tokio::test]
    async fn replaced_worker_stops_the_reader() {
        let (rec, log, _) = setup();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t1.status");
        std::fs::write(&path, "working: old worker\n").unwrap();
        let sf = StatusFile::new(&path, WorkerIdentity::new("t1", "g1"));
        sf.spawn(rec, Duration::from_millis(5)).await.unwrap();
        assert_eq!(log.events().len(), 1);
    }
}
