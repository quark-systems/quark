//! The engine adapter seam.
//!
//! `quarkd` never touches engine files itself. Every read of orchestration
//! state goes through an [`EngineAdapter`], and the daemon projects what it
//! returns into SQLite and the event stream.
//!
//! Phase 0 defines the read side only:
//!
//! - [`StubEngine`] is an in-memory adapter for tests and for running the
//!   daemon without an engine checkout.
//! - [`firstmate::FirstmateEngine`] wraps the typed readers in the
//!   `quark-engine` crate (fleet snapshot, status-log tails, hold records, PR
//!   poll records) and maps engine states onto the neutral [`TaskState`], so
//!   nothing engine-specific reaches the API.
//!
//! The write side (allowlisted `fm-*.sh` calls with argument validation and
//! adapter-call records) is a later addition to this trait.

pub mod firstmate;

use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use quark_systems::{TaskKind, TaskState};
use serde::{Deserialize, Serialize};

/// Addresses one engine workspace (a firstmate home). Location-neutral so a
/// cloud runtime can become another kind of location later.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkspaceRef {
    /// The Quark Project that owns the workspace.
    pub project_id: String,
    /// Local filesystem root of the workspace.
    pub root: PathBuf,
}

/// Point-in-time view of every task in one workspace.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FleetSnapshot {
    pub tasks: Vec<EngineTask>,
}

/// A task as the engine reports it, already mapped to neutral names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineTask {
    /// Engine task id; stable for the life of the task.
    pub id: String,
    pub title: String,
    pub kind: Option<TaskKind>,
    pub state: TaskState,
    pub state_note: Option<String>,
    pub harness: Option<String>,
    pub pull_request_url: Option<String>,
}

/// New status-log lines for one task, starting at a byte offset.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StatusTail {
    pub lines: Vec<String>,
    /// Offset to pass on the next call.
    pub next_offset: u64,
}

/// A question the engine is holding for a person.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hold {
    pub id: String,
    pub task_id: Option<String>,
    pub question: String,
    /// `Some` once the hold has been answered.
    pub answer: Option<String>,
    pub answered_by: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("workspace not found: {0}")]
    WorkspaceNotFound(PathBuf),
    #[error("task not found: {0}")]
    TaskNotFound(String),
    #[error("engine command failed: {0}")]
    Command(String),
    #[error("could not parse engine output: {0}")]
    Parse(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Read access to engine workspaces. Implementations must be cheap to call on
/// a short timer; the daemon diffs successive results itself.
#[async_trait]
pub trait EngineAdapter: Send + Sync {
    /// Short name reported by `GET /v1/health`.
    fn name(&self) -> &'static str;

    /// Every task currently known to the workspace.
    async fn snapshot(&self, ws: &WorkspaceRef) -> Result<FleetSnapshot, EngineError>;

    /// Status-log lines for `task_id` from byte `offset` onward.
    async fn status_tail(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
        offset: u64,
    ) -> Result<StatusTail, EngineError>;

    /// Open and recently answered holds in the workspace.
    async fn holds(&self, ws: &WorkspaceRef) -> Result<Vec<Hold>, EngineError>;
}

/// In-memory adapter. Every workspace sees the same configurable state.
#[derive(Debug, Default)]
pub struct StubEngine {
    snapshot: Mutex<FleetSnapshot>,
    holds: Mutex<Vec<Hold>>,
}

impl StubEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_snapshot(&self, snapshot: FleetSnapshot) {
        *self.snapshot.lock().unwrap() = snapshot;
    }

    pub fn set_holds(&self, holds: Vec<Hold>) {
        *self.holds.lock().unwrap() = holds;
    }
}

#[async_trait]
impl EngineAdapter for StubEngine {
    fn name(&self) -> &'static str {
        "stub"
    }

    async fn snapshot(&self, _ws: &WorkspaceRef) -> Result<FleetSnapshot, EngineError> {
        Ok(self.snapshot.lock().unwrap().clone())
    }

    async fn status_tail(
        &self,
        _ws: &WorkspaceRef,
        _task_id: &str,
        offset: u64,
    ) -> Result<StatusTail, EngineError> {
        Ok(StatusTail {
            lines: Vec::new(),
            next_offset: offset,
        })
    }

    async fn holds(&self, _ws: &WorkspaceRef) -> Result<Vec<Hold>, EngineError> {
        Ok(self.holds.lock().unwrap().clone())
    }
}
