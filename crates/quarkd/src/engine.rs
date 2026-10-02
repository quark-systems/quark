//! The engine adapter seam.
//!
//! `quarkd` never touches engine files itself. Every read of orchestration
//! state goes through an [`EngineAdapter`], and the daemon projects what it
//! returns into SQLite and the event stream.
//!
//! Every write goes through it too, as a neutral operation (steer a task,
//! cancel it, relaunch it, answer a decision, provision a Project workspace)
//! that the adapter maps onto its own engine calls.
//!
//! - [`StubEngine`] is an in-memory adapter for tests and for running the
//!   daemon without an engine checkout.
//! - [`firstmate::FirstmateEngine`] wraps the `quark-engine` crate: typed
//!   readers (fleet snapshot, status-log tails, hold records, PR poll records)
//!   mapped onto the neutral [`TaskState`], and allowlisted `fm-*.sh` writes
//!   with argument validation, so nothing engine-specific reaches the API.
//!
//! Adapters also name the directories worth watching, so the projector can
//! refresh a workspace as soon as a worker reports, not only on its timer.

pub mod firstmate;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use quark_systems::{AgentConfig, DeliveryPolicy, Evidence, MergeMethod, TaskKind, TaskState};
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
    /// The tmux `session:window` target the task runs in, when it runs in
    /// tmux. Terminal sessions map windows to tasks by it.
    #[serde(default)]
    pub terminal: Option<String>,
    /// The task's isolated working copy, while it exists. Its harness keys
    /// the session log by this directory.
    #[serde(default)]
    pub worktree: Option<PathBuf>,
}

/// One status-log line, parsed by the adapter into neutral fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusEntry {
    /// What was reported, e.g. `working` or `needs-decision`.
    pub kind: String,
    pub decision_key: Option<String>,
    pub note: String,
    /// The line as written, for the projection's audit trail.
    pub raw: String,
}

/// New status-log lines for one task, starting at a byte offset.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StatusTail {
    pub entries: Vec<StatusEntry>,
    /// Offset to pass on the next call.
    pub next_offset: u64,
}

/// A question the engine is holding for a person.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hold {
    /// Stable for the life of the question, and what
    /// [`EngineAdapter::answer`] takes. A question asked again after an
    /// answer may reuse it.
    pub id: String,
    pub task_id: Option<String>,
    pub question: String,
    /// `Some` once the hold has been answered.
    pub answer: Option<String>,
    pub answered_by: Option<String>,
}

/// Neutral lifecycle actions on a task's agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskControl {
    /// Stop the agent. Its worktree and uncommitted changes are kept.
    Cancel,
    /// Replace the agent in the same worktree, optionally on another harness,
    /// model or effort. `note` tells the new agent where things stand.
    Relaunch {
        harness: Option<String>,
        model: Option<String>,
        effort: Option<String>,
        note: String,
    },
}

/// A code repo to clone into a Project workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRepo {
    /// Short name, unique in the Project and path-safe.
    pub name: String,
    pub url: String,
}

/// Everything the engine needs to seed one Project workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspacePlan {
    pub project_id: String,
    pub name: String,
    pub goal: Option<String>,
    pub sources: Vec<SourceRepo>,
    /// Where the workspace goes. It must not exist yet, or be the workspace
    /// an earlier attempt seeded for the same Project.
    pub root: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The request was refused before anything ran.
    #[error("invalid request: {0}")]
    Invalid(String),
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

    /// Directories whose changes should trigger an immediate refresh of the
    /// workspace. Empty means the timer alone drives refreshes.
    fn watch_dirs(&self, _ws: &WorkspaceRef) -> Vec<PathBuf> {
        Vec::new()
    }

    /// Whether a change to `path` (inside a [`EngineAdapter::watch_dirs`]
    /// directory) can change tasks. Filters out the engine's own bookkeeping
    /// so it does not cause refresh churn.
    fn is_task_change(&self, _path: &Path) -> bool {
        false
    }

    /// Deliver a steering message to a task's worker. `Ok` means the message
    /// is durably recorded for the worker, not that it has been read.
    async fn send_message(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
        text: &str,
    ) -> Result<(), EngineError>;

    /// Answer the open hold `hold_id` with `answer`, recording `answered_by`
    /// as the person who answered. `Ok` means the engine durably recorded the
    /// answer and the question is no longer waiting; the hold leaves later
    /// [`EngineAdapter::holds`] results.
    async fn answer(
        &self,
        ws: &WorkspaceRef,
        hold_id: &str,
        answer: &str,
        answered_by: &str,
    ) -> Result<(), EngineError>;

    /// Apply a lifecycle action. `Ok` means the engine verified the result.
    async fn control(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
        action: &TaskControl,
    ) -> Result<(), EngineError>;

    /// Merge the pull request at `url`, which `task_id` opened, through the
    /// engine's guarded merge. `Ok` means the engine confirmed the merge; a
    /// refusal (not green, conflicting, held) is a [`EngineError::Command`].
    async fn merge_pull_request(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
        url: &str,
        method: Option<MergeMethod>,
    ) -> Result<(), EngineError>;

    /// Set whether green pull requests for the workspace's `repos` merge
    /// without asking a person (standing approval).
    async fn set_standing_approval(
        &self,
        ws: &WorkspaceRef,
        repos: &[String],
        on: bool,
    ) -> Result<(), EngineError>;

    /// The task's verification gate results, if its gates have run. Each
    /// artifact's `id` is its path relative to the task's gate directory and
    /// its `url` is empty; the daemon assigns both. `stale` is left false.
    async fn gate_evidence(
        &self,
        _ws: &WorkspaceRef,
        _task_id: &str,
    ) -> Result<Option<Evidence>, EngineError> {
        Ok(None)
    }

    /// The file of a gate artifact the task's evidence lists, by its path
    /// relative to the task's gate directory.
    fn gate_artifact(
        &self,
        _ws: &WorkspaceRef,
        task_id: &str,
        _path: &str,
    ) -> Result<PathBuf, EngineError> {
        Err(EngineError::TaskNotFound(task_id.to_string()))
    }

    /// Clone `source` into the command-center workspace and register it, so
    /// a Project workspace can be seeded from it. Idempotent for the same
    /// name and URL.
    async fn add_source(
        &self,
        command: &Path,
        source: &SourceRepo,
        delivery: DeliveryPolicy,
    ) -> Result<(), EngineError>;

    /// Create the Project workspace at `plan.root`, owned by the
    /// command-center workspace, with every source cloned into it. Returns
    /// the workspace root.
    async fn seed_workspace(
        &self,
        command: &Path,
        plan: &WorkspacePlan,
    ) -> Result<PathBuf, EngineError>;

    /// Start the coordinator of a seeded Project workspace with `agent`.
    async fn start_coordinator(
        &self,
        command: &Path,
        ws: &WorkspaceRef,
        agent: &AgentConfig,
    ) -> Result<(), EngineError>;

    /// Replace the workspace's verification-gate config with `config`
    /// (schema `fm.gates.v1`, see [`crate::gates`]). Engines without gates
    /// ignore it.
    async fn set_gates(&self, _ws: &WorkspaceRef, _config: &str) -> Result<(), EngineError> {
        Ok(())
    }

    /// Each running Project coordinator's tmux window target in the
    /// command-center workspace, keyed by Project id. Engines without
    /// coordinator windows have none.
    async fn coordinator_terminals(
        &self,
        _command: &Path,
    ) -> Result<HashMap<String, String>, EngineError> {
        Ok(HashMap::new())
    }
}

/// Refuses an artifact path the evidence does not list.
pub fn listed_artifact(e: &Evidence, path: &str) -> Result<(), EngineError> {
    let listed = e
        .gates
        .iter()
        .flat_map(|g| &g.cases)
        .flat_map(|c| &c.artifacts)
        .any(|a| a.id == path);
    if listed {
        Ok(())
    } else {
        Err(EngineError::Invalid(format!("no gate artifact {path:?}")))
    }
}

/// A write the [`StubEngine`] received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StubWrite {
    Message {
        task_id: String,
        text: String,
    },
    Control {
        task_id: String,
        action: TaskControl,
    },
    Answer {
        hold_id: String,
        answer: String,
        answered_by: String,
    },
    AddSource {
        name: String,
        url: String,
    },
    SeedWorkspace {
        project_id: String,
        sources: Vec<String>,
    },
    StartCoordinator {
        project_id: String,
        harness: String,
    },
    MergePullRequest {
        task_id: String,
        url: String,
        method: Option<MergeMethod>,
    },
    StandingApproval {
        repos: Vec<String>,
        on: bool,
    },
    Gates {
        project_id: String,
        config: String,
    },
}

/// In-memory adapter. Every workspace sees the same configurable state.
/// Seeding creates the workspace directory so later steps can write there.
#[derive(Debug, Default)]
pub struct StubEngine {
    snapshot: Mutex<FleetSnapshot>,
    holds: Mutex<Vec<Hold>>,
    writes: Mutex<Vec<StubWrite>>,
    write_error: Mutex<Option<String>>,
    coordinators: Mutex<HashMap<String, String>>,
    /// Status entries per engine task id; the offset is an index.
    status: Mutex<HashMap<String, Vec<StatusEntry>>>,
    /// Gate evidence and its artifact directory per engine task id.
    evidence: Mutex<HashMap<String, (Evidence, PathBuf)>>,
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

    /// Coordinator window targets by Project id.
    pub fn set_coordinators(&self, coordinators: HashMap<String, String>) {
        *self.coordinators.lock().unwrap() = coordinators;
    }

    /// Appends a status entry to an engine task's log.
    pub fn push_status(&self, task_id: &str, entry: StatusEntry) {
        self.status
            .lock()
            .unwrap()
            .entry(task_id.to_string())
            .or_default()
            .push(entry);
    }

    /// Gate results for an engine task, with artifacts under `dir`.
    pub fn set_evidence(&self, task_id: &str, evidence: Evidence, dir: PathBuf) {
        self.evidence
            .lock()
            .unwrap()
            .insert(task_id.to_string(), (evidence, dir));
    }

    /// Writes received so far, oldest first.
    pub fn writes(&self) -> Vec<StubWrite> {
        self.writes.lock().unwrap().clone()
    }

    /// Make every later write fail as an engine command error.
    pub fn fail_writes(&self, message: Option<&str>) {
        *self.write_error.lock().unwrap() = message.map(str::to_string);
    }

    fn accept(&self, write: StubWrite) -> Result<(), EngineError> {
        if let Some(m) = self.write_error.lock().unwrap().clone() {
            return Err(EngineError::Command(m));
        }
        self.writes.lock().unwrap().push(write);
        Ok(())
    }
}

#[async_trait]
impl EngineAdapter for StubEngine {
    fn name(&self) -> &'static str {
        "stub"
    }

    async fn coordinator_terminals(
        &self,
        _command: &Path,
    ) -> Result<HashMap<String, String>, EngineError> {
        Ok(self.coordinators.lock().unwrap().clone())
    }

    async fn snapshot(&self, _ws: &WorkspaceRef) -> Result<FleetSnapshot, EngineError> {
        Ok(self.snapshot.lock().unwrap().clone())
    }

    async fn status_tail(
        &self,
        _ws: &WorkspaceRef,
        task_id: &str,
        offset: u64,
    ) -> Result<StatusTail, EngineError> {
        let status = self.status.lock().unwrap();
        let log = status.get(task_id).map(Vec::as_slice).unwrap_or_default();
        let start = (offset as usize).min(log.len());
        Ok(StatusTail {
            entries: log[start..].to_vec(),
            next_offset: log.len() as u64,
        })
    }

    async fn holds(&self, _ws: &WorkspaceRef) -> Result<Vec<Hold>, EngineError> {
        Ok(self.holds.lock().unwrap().clone())
    }

    async fn send_message(
        &self,
        _ws: &WorkspaceRef,
        task_id: &str,
        text: &str,
    ) -> Result<(), EngineError> {
        self.accept(StubWrite::Message {
            task_id: task_id.into(),
            text: text.into(),
        })
    }

    /// Records the answer and drops the hold, as an engine would.
    async fn answer(
        &self,
        _ws: &WorkspaceRef,
        hold_id: &str,
        answer: &str,
        answered_by: &str,
    ) -> Result<(), EngineError> {
        if !self.holds.lock().unwrap().iter().any(|h| h.id == hold_id) {
            return Err(EngineError::Invalid(format!(
                "no open question {hold_id} in the engine"
            )));
        }
        self.accept(StubWrite::Answer {
            hold_id: hold_id.into(),
            answer: answer.into(),
            answered_by: answered_by.into(),
        })?;
        self.holds.lock().unwrap().retain(|h| h.id != hold_id);
        Ok(())
    }

    async fn control(
        &self,
        _ws: &WorkspaceRef,
        task_id: &str,
        action: &TaskControl,
    ) -> Result<(), EngineError> {
        self.accept(StubWrite::Control {
            task_id: task_id.into(),
            action: action.clone(),
        })
    }

    async fn gate_evidence(
        &self,
        _ws: &WorkspaceRef,
        task_id: &str,
    ) -> Result<Option<Evidence>, EngineError> {
        Ok(self
            .evidence
            .lock()
            .unwrap()
            .get(task_id)
            .map(|(e, _)| e.clone()))
    }

    fn gate_artifact(
        &self,
        _ws: &WorkspaceRef,
        task_id: &str,
        path: &str,
    ) -> Result<PathBuf, EngineError> {
        let evidence = self.evidence.lock().unwrap();
        let (e, dir) = evidence
            .get(task_id)
            .ok_or_else(|| EngineError::TaskNotFound(task_id.into()))?;
        listed_artifact(e, path)?;
        Ok(dir.join(path))
    }

    async fn merge_pull_request(
        &self,
        _ws: &WorkspaceRef,
        task_id: &str,
        url: &str,
        method: Option<MergeMethod>,
    ) -> Result<(), EngineError> {
        self.accept(StubWrite::MergePullRequest {
            task_id: task_id.into(),
            url: url.into(),
            method,
        })
    }

    async fn set_standing_approval(
        &self,
        _ws: &WorkspaceRef,
        repos: &[String],
        on: bool,
    ) -> Result<(), EngineError> {
        self.accept(StubWrite::StandingApproval {
            repos: repos.to_vec(),
            on,
        })
    }

    async fn add_source(
        &self,
        _command: &Path,
        source: &SourceRepo,
        _delivery: DeliveryPolicy,
    ) -> Result<(), EngineError> {
        self.accept(StubWrite::AddSource {
            name: source.name.clone(),
            url: source.url.clone(),
        })
    }

    async fn seed_workspace(
        &self,
        _command: &Path,
        plan: &WorkspacePlan,
    ) -> Result<PathBuf, EngineError> {
        self.accept(StubWrite::SeedWorkspace {
            project_id: plan.project_id.clone(),
            sources: plan.sources.iter().map(|s| s.name.clone()).collect(),
        })?;
        std::fs::create_dir_all(&plan.root)?;
        Ok(plan.root.clone())
    }

    async fn start_coordinator(
        &self,
        _command: &Path,
        ws: &WorkspaceRef,
        agent: &AgentConfig,
    ) -> Result<(), EngineError> {
        self.accept(StubWrite::StartCoordinator {
            project_id: ws.project_id.clone(),
            harness: agent.harness.clone(),
        })
    }

    async fn set_gates(&self, ws: &WorkspaceRef, config: &str) -> Result<(), EngineError> {
        self.accept(StubWrite::Gates {
            project_id: ws.project_id.clone(),
            config: config.to_string(),
        })
    }
}
