//! Running a worker process natively or in a sandbox.

use std::collections::BTreeMap;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationMode {
    /// A plain child process.
    Native,
    /// Seatbelt on macOS, bubblewrap on Linux.
    Sandbox,
    /// Containers and microVMs, with Cloud mode.
    Container,
}

/// What the sandbox lets the process touch.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    /// Paths writable inside the sandbox; the worktree is always one.
    pub writable: Vec<PathBuf>,
    /// Paths readable but not writable.
    pub readable: Vec<PathBuf>,
    /// Hosts reachable over the network; empty with `network` false means
    /// none.
    pub allowed_hosts: Vec<String>,
    pub network: bool,
}

/// A command to wrap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessSpec {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub mode: IsolationMode,
    pub policy: Policy,
}

/// Turns a [`ProcessSpec`] into the argv a session backend runs, so
/// isolation composes with tmux and the PTY supervisor rather than owning
/// the process.
#[async_trait]
pub trait Isolation: Send + Sync {
    /// Modes this host supports.
    fn modes(&self) -> Vec<IsolationMode>;

    /// The wrapped argv and env, such as `sandbox-exec -f <profile> ...`.
    /// [`crate::CoreError::Unsupported`] for a mode not in
    /// [`Isolation::modes`].
    async fn wrap(&self, spec: &ProcessSpec) -> Result<(Vec<String>, BTreeMap<String, String>)>;
}
