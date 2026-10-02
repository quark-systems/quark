//! [`EngineAdapter`] over a firstmate home, built on the `quark-engine`
//! readers and its allowlisted writer.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use quark_systems::{TaskKind, TaskState};

use quark_engine::holds::decisions;
use quark_engine::runner::CallLog;
use quark_engine::snapshot::{BacklogState, FleetSnapshot as FmSnapshot, Task};
use quark_engine::status::StatusTail as FmTail;
use quark_engine::write::WriteOp;
use quark_engine::{EngineReader, EngineWriter, Error, Workspace};

use super::{
    EngineAdapter, EngineError, EngineTask, FleetSnapshot, Hold, StatusTail, TaskControl,
    WorkspaceRef,
};

/// Reads firstmate homes with scripts from one pinned engine checkout.
/// `WorkspaceRef::root` is the home (`FM_HOME`).
pub struct FirstmateEngine {
    engine_root: PathBuf,
    log: Arc<dyn CallLog>,
}

impl FirstmateEngine {
    pub fn new(engine_root: impl Into<PathBuf>, log: Arc<dyn CallLog>) -> Self {
        Self {
            engine_root: engine_root.into(),
            log,
        }
    }

    fn reader(&self, ws: &WorkspaceRef) -> Result<EngineReader, EngineError> {
        Ok(EngineReader::new(self.workspace(ws)?, self.log.clone()))
    }

    fn workspace(&self, ws: &WorkspaceRef) -> Result<Workspace, EngineError> {
        if !ws.root.is_dir() {
            return Err(EngineError::WorkspaceNotFound(ws.root.clone()));
        }
        Ok(Workspace::new(&ws.root, &self.engine_root))
    }

    async fn write(&self, ws: &WorkspaceRef, op: WriteOp) -> Result<(), EngineError> {
        let writer = EngineWriter::new(self.workspace(ws)?, self.log.clone());
        blocking(move || writer.write(&op).map(drop)).await
    }

    async fn read_snapshot(&self, ws: &WorkspaceRef) -> Result<FmSnapshot, EngineError> {
        let reader = self.reader(ws)?;
        blocking(move || reader.fleet_snapshot()).await
    }
}

#[async_trait]
impl EngineAdapter for FirstmateEngine {
    fn name(&self) -> &'static str {
        "firstmate"
    }

    async fn snapshot(&self, ws: &WorkspaceRef) -> Result<FleetSnapshot, EngineError> {
        Ok(neutral_snapshot(&self.read_snapshot(ws).await?))
    }

    async fn status_tail(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
        offset: u64,
    ) -> Result<StatusTail, EngineError> {
        let path = self
            .reader(ws)?
            .workspace()
            .status_log_path(task_id)
            .map_err(convert)?;
        blocking(move || {
            let mut tail = FmTail::resume(path, offset);
            let lines = tail.read_new()?;
            Ok(StatusTail {
                lines: lines.into_iter().map(|l| l.event.raw).collect(),
                next_offset: tail.offset(),
            })
        })
        .await
    }

    async fn holds(&self, ws: &WorkspaceRef) -> Result<Vec<Hold>, EngineError> {
        Ok(neutral_holds(&self.read_snapshot(ws).await?))
    }

    async fn send_message(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
        text: &str,
    ) -> Result<(), EngineError> {
        let op = WriteOp::Send {
            task_id: task_id.into(),
            text: text.into(),
        };
        self.write(ws, op).await
    }

    /// Cancel is `exit`, never teardown: the worktree and its changes stay.
    async fn control(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
        action: &TaskControl,
    ) -> Result<(), EngineError> {
        let task_id = task_id.to_string();
        let op = match action.clone() {
            TaskControl::Cancel => WriteOp::Exit { task_id },
            TaskControl::Relaunch {
                harness,
                model,
                effort,
                note,
            } => WriteOp::Relaunch {
                task_id,
                harness,
                model,
                effort,
                note,
            },
        };
        self.write(ws, op).await
    }
}

/// Live tasks from metadata, plus queued backlog work that has not started.
/// Secondmates are workspaces, not tasks, so they are left out.
pub fn neutral_snapshot(s: &FmSnapshot) -> FleetSnapshot {
    let mut tasks: Vec<EngineTask> = s
        .tasks
        .iter()
        .filter(|t| t.kind.as_deref() != Some("secondmate"))
        .map(|t| {
            let backlog = s.backlog_items().find(|r| r.id.as_deref() == Some(&t.id));
            let current = t.current_state.as_ref();
            let pull_request_url =
                t.pr.as_ref()
                    .and_then(|p| p.url.clone())
                    .or_else(|| backlog.and_then(|r| r.pr_url.clone()));
            EngineTask {
                id: t.id.clone(),
                title: backlog
                    .and_then(|r| r.title.clone())
                    .unwrap_or_else(|| t.id.clone()),
                kind: task_kind(t.kind.as_deref()),
                state: task_state(t, pull_request_url.is_some()),
                state_note: current.and_then(|c| c.detail.clone()),
                harness: t.harness.clone(),
                pull_request_url,
            }
        })
        .collect();

    for r in s.backlog_items() {
        let (Some(id), BacklogState::Queued) = (&r.id, r.state) else {
            continue;
        };
        let Some(kind) = task_kind(r.kind.as_deref()) else {
            continue;
        };
        if tasks.iter().any(|t| &t.id == id) {
            continue;
        }
        tasks.push(EngineTask {
            id: id.clone(),
            title: r.title.clone().unwrap_or_else(|| id.clone()),
            kind: Some(kind),
            state: TaskState::Queued,
            state_note: r.hold_reason.clone(),
            harness: None,
            pull_request_url: None,
        });
    }
    FleetSnapshot { tasks }
}

/// Questions waiting on a person now: captain holds in the live bucket and
/// every open keyed decision. Deferred, aged and blocked holds are not
/// waiting on anyone yet.
pub fn neutral_holds(s: &FmSnapshot) -> Vec<Hold> {
    let d = decisions(s);
    let held = d.actionable_holds().map(|h| Hold {
        id: h.task_id.clone(),
        task_id: Some(h.task_id.clone()),
        question: match (&h.title, &h.reason) {
            (Some(t), Some(r)) => format!("{t}: {r}"),
            (Some(t), None) => t.clone(),
            (None, Some(r)) => r.clone(),
            (None, None) => h.task_id.clone(),
        },
        answer: None,
        answered_by: None,
    });
    let open = d.open.iter().map(|o| Hold {
        id: format!("{}:{}", o.task_id, o.key),
        task_id: Some(o.task_id.clone()),
        question: o.summary.clone().unwrap_or_else(|| o.verb.clone()),
        answer: None,
        answered_by: None,
    });
    held.chain(open).collect()
}

fn task_kind(kind: Option<&str>) -> Option<TaskKind> {
    match kind? {
        "ship" => Some(TaskKind::Ship),
        "scout" => Some(TaskKind::Scout),
        _ => None,
    }
}

/// Engine states come from `fm-crew-state.sh`:
/// working, parked, done, blocked, paused, failed, unknown.
fn task_state(t: &Task, has_pr: bool) -> TaskState {
    let Some(current) = &t.current_state else {
        return TaskState::Unknown;
    };
    match current.state.as_str() {
        "working" => TaskState::Running,
        "parked" => TaskState::NeedsDecision,
        "blocked" => TaskState::Blocked,
        "paused" => TaskState::Paused,
        "failed" => TaskState::Failed,
        // A finished ship task keeps its record until its PR lands.
        "done" if has_pr => TaskState::InReview,
        "done" => TaskState::Done,
        _ => TaskState::Unknown,
    }
}

async fn blocking<T, F>(f: F) -> Result<T, EngineError>
where
    T: Send + 'static,
    F: FnOnce() -> quark_engine::Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| EngineError::Command(format!("engine task failed: {e}")))?
        .map_err(convert)
}

fn convert(e: Error) -> EngineError {
    match e {
        Error::Io { source, .. } => EngineError::Io(source),
        Error::InvalidTaskId(id) => EngineError::TaskNotFound(id),
        Error::InvalidArgument { reason, .. } => EngineError::Invalid(reason),
        e @ (Error::Json { .. } | Error::Schema { .. } | Error::Malformed { .. }) => {
            EngineError::Parse(e.to_string())
        }
        e => EngineError::Command(e.to_string()),
    }
}
