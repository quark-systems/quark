//! Adapter between `quarkd` and the firstmate engine.
//!
//! This crate is the engine boundary (spec ADR-8). It turns firstmate's
//! on-disk state into typed values and never writes engine files itself:
//!
//! - [`snapshot`]: `fm-fleet-snapshot.sh --json` (schema `fm-fleet-snapshot.v1`).
//! - [`status`]: `state/<id>.status` wake-event lines and an incremental tail.
//! - [`holds`]: captain holds and open decisions derived from a snapshot.
//! - [`pr`]: `state/<id>.pr-poll` sidecars and merge-notified markers.
//! - [`summary`]: `state/home-summary.json` (schema `fm-secondmate-home-summary.v1`).
//! - [`write`]: typed writes, each run through its own engine script.
//!
//! Every script invocation goes through [`runner::ScriptRunner`], which only runs
//! allowlisted, genuine `bin/fm-*.sh` files from the pinned engine checkout and
//! records each call as an [`runner::AdapterCall`].
//!
//! Status lines are wake-event history, not current state. Current task state
//! comes from the snapshot's `current_state`, which the engine reconciles.

mod error;
pub mod holds;
pub mod pr;
pub mod runner;
pub mod snapshot;
pub mod status;
pub mod summary;
pub mod workspace;
pub mod write;

pub use error::{Error, Result};
pub use workspace::{validate_task_id, Workspace};

use std::sync::Arc;
use std::time::Duration;

use runner::{CallLog, ScriptRunner};
use write::WriteOp;

/// Read-only view of one firstmate workspace (a firstmate home).
pub struct EngineReader {
    workspace: Workspace,
    runner: ScriptRunner,
}

impl EngineReader {
    pub fn new(workspace: Workspace, log: Arc<dyn CallLog>) -> Self {
        let runner = ScriptRunner::new(workspace.clone(), log);
        Self { workspace, runner }
    }

    /// Override the per-call timeout (default 60s).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.runner = self.runner.with_timeout(timeout);
        self
    }

    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// Run `fm-fleet-snapshot.sh --json` and parse the result.
    pub fn fleet_snapshot(&self) -> Result<snapshot::FleetSnapshot> {
        let out = self.runner.run(runner::FLEET_SNAPSHOT, &["--json"])?;
        snapshot::parse(&out)
    }

    /// Read the published `state/home-summary.json`, if this home has one.
    pub fn home_summary(&self) -> Result<Option<summary::HomeSummary>> {
        summary::read(&self.workspace.home_summary_path())
    }

    /// Read a task's PR poll sidecar, if one is registered.
    pub fn pr_poll(&self, task_id: &str) -> Result<Option<pr::PrPollRecord>> {
        pr::read_poll(&self.workspace.pr_poll_path(task_id)?)
    }

    /// Read a task's merge-notified marker, if a merge was already delivered.
    pub fn merge_notified(&self, task_id: &str) -> Result<Option<pr::MergeNotified>> {
        pr::read_merge_notified(&self.workspace.merge_notified_path(task_id)?)
    }

    /// A tail over a task's status log, starting at the beginning of the file.
    pub fn status_tail(&self, task_id: &str) -> Result<status::StatusTail> {
        Ok(status::StatusTail::new(
            self.workspace.status_log_path(task_id)?,
        ))
    }
}

/// Changes one firstmate workspace, only through allowlisted [`WriteOp`]s.
pub struct EngineWriter {
    runner: ScriptRunner,
}

impl EngineWriter {
    pub fn new(workspace: Workspace, log: Arc<dyn CallLog>) -> Self {
        Self {
            runner: ScriptRunner::new(workspace, log),
        }
    }

    /// One timeout for every write, replacing each operation's own bound.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.runner = self.runner.with_write_timeout(timeout);
        self
    }

    /// Validate and run one write. Returns the script's stdout; a non-zero
    /// exit is an error carrying stderr, and nothing is retried.
    pub fn write(&self, op: &WriteOp) -> Result<String> {
        let out = self.runner.run_write(op)?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }
}
