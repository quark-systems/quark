//! Terminal sessions workers run in: tmux control mode and the daemon-owned
//! PTY supervisor.

use std::collections::BTreeMap;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{Result, TaskId};

/// Backend-assigned session id, stable while the session lives.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub String);

/// What to run in a new session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSpec {
    pub task: Option<TaskId>,
    /// Human-readable name, such as the tmux window name.
    pub name: String,
    pub cwd: PathBuf,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub size: TermSize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermSize {
    pub cols: u16,
    pub rows: u16,
}

impl Default for TermSize {
    fn default() -> Self {
        Self {
            cols: 200,
            rows: 50,
        }
    }
}

/// Live state of one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub name: String,
    pub task: Option<TaskId>,
    pub alive: bool,
    /// The process's exit code once it has exited.
    pub exit_code: Option<i32>,
}

/// Screen contents at one moment, for reconnecting viewers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub size: TermSize,
    /// The screen and scrollback as raw terminal bytes, replayable into a
    /// fresh emulator.
    pub bytes: Vec<u8>,
}

/// Output from a session, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Output {
    Bytes { data: Vec<u8> },
    Exited { code: Option<i32> },
}

/// A live output stream from [`SessionBackend::attach`].
#[async_trait]
pub trait OutputStream: Send {
    /// The next chunk; `None` once the session is gone.
    async fn next(&mut self) -> Option<Output>;
}

/// Creates and drives terminal sessions. Sessions outlive viewers: attach
/// and detach never affect the process.
#[async_trait]
pub trait SessionBackend: Send + Sync {
    /// Short name such as `tmux` or `pty`.
    fn name(&self) -> &'static str;

    async fn create(&self, spec: &SessionSpec) -> Result<SessionInfo>;

    /// Output from now on. Pair with [`SessionBackend::snapshot`] to catch
    /// up first.
    async fn attach(&self, id: &SessionId) -> Result<Box<dyn OutputStream>>;

    /// Keystrokes or pasted text, as raw bytes.
    async fn input(&self, id: &SessionId, bytes: &[u8]) -> Result<()>;

    async fn resize(&self, id: &SessionId, size: TermSize) -> Result<()>;

    async fn snapshot(&self, id: &SessionId) -> Result<Snapshot>;

    async fn kill(&self, id: &SessionId) -> Result<()>;

    /// Every session this backend knows, including ones that survived a
    /// daemon restart.
    async fn list(&self) -> Result<Vec<SessionInfo>>;
}
