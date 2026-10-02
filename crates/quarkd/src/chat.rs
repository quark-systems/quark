//! Chat input to coordinators.
//!
//! A message for a coordinator is typed into its live session, then
//! confirmed from the coordinator's own session log. Delivery sits behind
//! [`CoordinatorInput`]: [`SessionsInput`] types into the coordinator's
//! terminal, and [`NoSessions`] refuses every message (rather than dropping
//! it) when the daemon has no terminal sessions. The reply is not returned
//! here: it arrives as `coordinator.message` events read from the session log.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use quark_transcript::{locate, read_from, SessionFormat, SessionRoots, TranscriptRole};

use crate::engine::WorkspaceRef;
use crate::sessions::{SessionError, Sessions};

/// How long to wait for the session log to record a typed message.
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(10);
const CONFIRM_POLL: Duration = Duration::from_millis(250);
/// Pause between the pasted text and Enter, so the harness has finished
/// handling the paste before it sees the submit.
const SUBMIT_DELAY: Duration = Duration::from_millis(150);

/// Longest message accepted, in bytes.
pub const MAX_MESSAGE_BYTES: usize = 32 * 1024;

/// How far a delivery got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Typed and the submit was confirmed.
    Confirmed,
    /// Typed and submitted, but the session did not confirm the submit. The
    /// caller must not retype it blindly.
    Unconfirmed,
}

#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    /// No live session to type into; nothing was typed.
    #[error("coordinator session unavailable: {0}")]
    Unavailable(String),
    /// Delivery failed; nothing may be assumed delivered.
    #[error("delivery failed: {0}")]
    Failed(String),
}

/// Types a message into the coordinator session of one workspace.
#[async_trait]
pub trait CoordinatorInput: Send + Sync {
    async fn send(&self, ws: &WorkspaceRef, text: &str) -> Result<Delivery, ChatError>;
}

/// The input used while no terminal session client is attached.
#[derive(Debug, Default)]
pub struct NoSessions;

#[async_trait]
impl CoordinatorInput for NoSessions {
    async fn send(&self, _ws: &WorkspaceRef, _text: &str) -> Result<Delivery, ChatError> {
        Err(ChatError::Unavailable(
            "no terminal session client is attached to this daemon".into(),
        ))
    }
}

/// Types into the coordinator's terminal (terminal id = Project id).
///
/// The text goes in as one bracketed paste, so newlines stay inside the
/// message instead of submitting it early, then Enter submits it. Every
/// supported coordinator harness (Claude Code, Codex, Pi) understands
/// bracketed paste; [`validate`] already refuses escape characters, so the
/// text cannot end the paste itself. The submit is confirmed when the
/// coordinator's session log records a user message starting with the text.
pub struct SessionsInput {
    sessions: Sessions,
    roots: SessionRoots,
    confirm_timeout: Duration,
}

impl SessionsInput {
    pub fn new(sessions: Sessions, roots: SessionRoots) -> Self {
        Self {
            sessions,
            roots,
            confirm_timeout: CONFIRM_TIMEOUT,
        }
    }

    /// How long to wait for the session log before answering `Unconfirmed`.
    pub fn with_confirm_timeout(mut self, timeout: Duration) -> Self {
        self.confirm_timeout = timeout;
        self
    }
}

#[async_trait]
impl CoordinatorInput for SessionsInput {
    async fn send(&self, ws: &WorkspaceRef, text: &str) -> Result<Delivery, ChatError> {
        let terminal = ws.project_id.as_str();
        self.sessions.get(terminal).map_err(|e| match e {
            SessionError::NotFound => {
                ChatError::Unavailable("the coordinator has no live terminal".into())
            }
            other => ChatError::Unavailable(other.to_string()),
        })?;

        let roots = self.roots.clone();
        let root = ws.root.clone();
        let before = tokio::task::spawn_blocking(move || log_position(&roots, &root))
            .await
            .map_err(|e| ChatError::Failed(e.to_string()))?;

        let mut paste = Vec::with_capacity(text.len() + 12);
        paste.extend_from_slice(b"\x1b[200~");
        paste.extend_from_slice(text.as_bytes());
        paste.extend_from_slice(b"\x1b[201~");
        self.sessions
            .input(terminal, &paste, None)
            .await
            .map_err(|e| ChatError::Failed(e.to_string()))?;
        tokio::time::sleep(SUBMIT_DELAY).await;
        self.sessions
            .input(terminal, b"\r", None)
            .await
            .map_err(|e| ChatError::Failed(format!("typed but not submitted: {e}")))?;

        let deadline = tokio::time::Instant::now() + self.confirm_timeout;
        let want = first_line(text).to_string();
        loop {
            let roots = self.roots.clone();
            let root = ws.root.clone();
            let before = before.clone();
            let want = want.clone();
            let seen = tokio::task::spawn_blocking(move || {
                logged_since(&roots, &root, before.as_ref(), &want)
            })
            .await
            .unwrap_or(false);
            if seen {
                return Ok(Delivery::Confirmed);
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(Delivery::Unconfirmed);
            }
            tokio::time::sleep(CONFIRM_POLL).await;
        }
    }
}

/// A coordinator session log and its length before typing.
type LogPosition = (PathBuf, SessionFormat, u64);

fn newest_log(roots: &SessionRoots, cwd: &Path) -> Option<(PathBuf, SessionFormat)> {
    SessionFormat::ALL
        .iter()
        .filter_map(|&f| locate(f, cwd, roots).map(|p| (p, f)))
        .max_by_key(|(p, _)| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
        })
}

fn log_position(roots: &SessionRoots, cwd: &Path) -> Option<LogPosition> {
    let (path, format) = newest_log(roots, cwd)?;
    let len = std::fs::metadata(&path).ok()?.len();
    Some((path, format, len))
}

/// Whether the coordinator's log gained a user message starting with `want`
/// since `before`. A log that appeared or changed since is read from its start.
fn logged_since(
    roots: &SessionRoots,
    cwd: &Path,
    before: Option<&LogPosition>,
    want: &str,
) -> bool {
    let Some((path, format)) = newest_log(roots, cwd) else {
        return false;
    };
    let offset = match before {
        Some((p, _, len)) if *p == path => *len,
        _ => 0,
    };
    let Ok(batch) = read_from(&path, offset, format) else {
        return false;
    };
    batch
        .entries
        .iter()
        .any(|(_, e)| e.role == TranscriptRole::User && e.text.trim_start().starts_with(want))
}

fn first_line(text: &str) -> &str {
    text.trim().lines().next().unwrap_or("").trim_end()
}

/// Records messages instead of typing them, for tests and the stub engine.
#[derive(Debug)]
pub struct RecordingInput {
    outcome: Mutex<Result<Delivery, String>>,
    sent: Mutex<Vec<(WorkspaceRef, String)>>,
}

impl Default for RecordingInput {
    fn default() -> Self {
        Self {
            outcome: Mutex::new(Ok(Delivery::Confirmed)),
            sent: Mutex::new(Vec::new()),
        }
    }
}

impl RecordingInput {
    pub fn new() -> Self {
        Self::default()
    }

    /// What every later `send` reports; `Err` makes it fail.
    pub fn set_outcome(&self, outcome: Result<Delivery, String>) {
        *self.outcome.lock().unwrap() = outcome;
    }

    pub fn sent(&self) -> Vec<(WorkspaceRef, String)> {
        self.sent.lock().unwrap().clone()
    }
}

#[async_trait]
impl CoordinatorInput for RecordingInput {
    async fn send(&self, ws: &WorkspaceRef, text: &str) -> Result<Delivery, ChatError> {
        let outcome = self.outcome.lock().unwrap().clone();
        match outcome {
            Ok(d) => {
                self.sent
                    .lock()
                    .unwrap()
                    .push((ws.clone(), text.to_string()));
                Ok(d)
            }
            Err(e) => Err(ChatError::Failed(e)),
        }
    }
}

/// Why `text` cannot be typed into a session, if it cannot. Newlines and tabs
/// are allowed; other control characters (escape sequences in particular)
/// would drive the terminal rather than say something to the coordinator.
pub fn validate(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("text must not be empty".into());
    }
    if text.len() > MAX_MESSAGE_BYTES {
        return Err(format!("text is longer than {MAX_MESSAGE_BYTES} bytes"));
    }
    if text
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err("text must not contain control characters other than newline and tab".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate;

    #[test]
    fn validates_typed_text() {
        assert!(validate("fix #42\nand add tests").is_ok());
        assert!(validate("  ").is_err());
        assert!(validate("\u{1b}[2J").is_err());
        assert!(validate(&"x".repeat(super::MAX_MESSAGE_BYTES + 1)).is_err());
    }
}
