//! [`EngineAdapter`] over a firstmate home, built on the `quark-engine`
//! readers and its allowlisted writer.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use quark_systems::{AgentConfig, DeliveryPolicy, TaskKind, TaskState};

use quark_engine::holds::decisions;
use quark_engine::runner::CallLog;
use quark_engine::snapshot::{BacklogState, FleetSnapshot as FmSnapshot, Task};
use quark_engine::status::StatusTail as FmTail;
use quark_engine::write::{self, DeliveryMode, WriteOp};
use quark_engine::{EngineReader, EngineWriter, Error, Workspace};

use super::{
    EngineAdapter, EngineError, EngineTask, FleetSnapshot, Hold, SourceRepo, StatusTail,
    TaskControl, WorkspacePlan, WorkspaceRef,
};

/// Reads firstmate homes with scripts from one pinned engine checkout.
/// `WorkspaceRef::root` is the home (`FM_HOME`).
pub struct FirstmateEngine {
    engine_root: PathBuf,
    log: Arc<dyn CallLog>,
    tmux: Option<String>,
}

impl FirstmateEngine {
    pub fn new(engine_root: impl Into<PathBuf>, log: Arc<dyn CallLog>) -> Self {
        Self {
            engine_root: engine_root.into(),
            log,
            tmux: None,
        }
    }

    /// Runs every engine script with `TMUX` set to `value`, so the engine
    /// opens and finds windows on quarkd's tmux server rather than the
    /// user's own.
    pub fn with_tmux(mut self, value: Option<String>) -> Self {
        self.tmux = value;
        self
    }

    fn at(&self, home: &Path) -> Workspace {
        let ws = Workspace::new(home, &self.engine_root);
        match &self.tmux {
            Some(t) => ws.with_env("TMUX", t.clone()),
            None => ws,
        }
    }

    fn reader(&self, ws: &WorkspaceRef) -> Result<EngineReader, EngineError> {
        Ok(EngineReader::new(self.workspace(ws)?, self.log.clone()))
    }

    fn workspace(&self, ws: &WorkspaceRef) -> Result<Workspace, EngineError> {
        if !ws.root.is_dir() {
            return Err(EngineError::WorkspaceNotFound(ws.root.clone()));
        }
        Ok(self.at(&ws.root))
    }

    async fn write(&self, ws: &WorkspaceRef, op: WriteOp) -> Result<(), EngineError> {
        self.write_at(&ws.root, op).await.map(drop)
    }

    /// Run `op` with `FM_HOME` at `home` and return its stdout.
    async fn write_at(&self, home: &Path, op: WriteOp) -> Result<String, EngineError> {
        if !home.is_dir() {
            return Err(EngineError::WorkspaceNotFound(home.to_path_buf()));
        }
        let writer = EngineWriter::new(self.at(home), self.log.clone());
        blocking(move || writer.write(&op)).await
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

    async fn add_source(
        &self,
        command: &Path,
        source: &SourceRepo,
        delivery: DeliveryPolicy,
    ) -> Result<(), EngineError> {
        let op = WriteOp::ProjectAdd {
            name: source.name.clone(),
            origin: source.url.clone(),
            mode: delivery_mode(delivery),
            description: format!("{} (added by Quark)", source.url),
        };
        let out = self.write_at(command, op).await?;
        write::parse_project_added(&out).map_err(convert)?;
        Ok(())
    }

    /// Seeds the Project workspace as a local secondmate of the command
    /// center, keyed by the Project id.
    async fn seed_workspace(
        &self,
        command: &Path,
        plan: &WorkspacePlan,
    ) -> Result<PathBuf, EngineError> {
        let (charter, scope) = charter(plan);
        let op = WriteOp::HomeSeed {
            id: plan.project_id.clone(),
            home: plan.root.clone(),
            projects: plan.sources.iter().map(|s| s.name.clone()).collect(),
            charter,
            scope,
        };
        let out = self.write_at(command, op).await?;
        write::parse_seeded_home(&out).map_err(convert)
    }

    async fn start_coordinator(
        &self,
        command: &Path,
        ws: &WorkspaceRef,
        agent: &AgentConfig,
    ) -> Result<Option<String>, EngineError> {
        let op = WriteOp::SpawnSecondmate {
            id: ws.project_id.clone(),
            home: ws.root.clone(),
            harness: engine_harness(&agent.harness).to_string(),
            model: agent.model.clone(),
            effort: agent.effort.clone(),
        };
        let out = self.write_at(command, op).await?;
        let line = write::parse_spawned(&out).map_err(convert)?;
        Ok(spawned_window(&line))
    }
}

/// The local `session:window` target from fm-spawn's `spawned` line. A
/// remote secondmate reports `window=remote:<id>`, which no local tmux
/// server holds.
fn spawned_window(line: &str) -> Option<String> {
    let window = line
        .split_whitespace()
        .find_map(|f| f.strip_prefix("window="))?;
    (window.contains(':') && !window.starts_with("remote:")).then(|| window.to_string())
}

fn delivery_mode(d: DeliveryPolicy) -> DeliveryMode {
    match d {
        DeliveryPolicy::Gated => DeliveryMode::NoMistakes,
        DeliveryPolicy::Direct => DeliveryMode::DirectPr,
    }
}

/// Neutral harness ids that differ from the engine's adapter names.
pub fn engine_harness(harness: &str) -> &str {
    match harness {
        "claude-code" => "claude",
        "cursor-agent" => "cursor",
        "bob-shell" => "bob",
        other => other,
    }
}

/// The secondmate charter and routing scope for a Project workspace. The
/// coordinator reads the charter as its standing job description.
pub fn charter(plan: &WorkspacePlan) -> (String, String) {
    let repos: Vec<_> = plan.sources.iter().map(|s| s.name.as_str()).collect();
    let goal = plan
        .goal
        .as_deref()
        .map(str::trim)
        .filter(|g| !g.is_empty())
        .map(|g| format!(" Its goal: {g}"))
        .unwrap_or_default();
    let charter = format!(
        "Coordinate the Quark Project \"{}\" across {}.{goal} The Project repo checked out at project/ holds its instructions.md and memory/; read instructions.md before planning work.",
        plan.name,
        repos.join(", "),
    );
    let scope = format!(
        "All work for the Quark Project \"{}\" ({}) in {}.",
        plan.name,
        plan.project_id,
        repos.join(", ")
    );
    (charter, scope)
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
                terminal: tmux_target(t),
                worktree: t
                    .paths
                    .worktree
                    .as_ref()
                    .filter(|w| w.present)
                    .and_then(|w| w.path.as_ref())
                    .map(PathBuf::from),
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
            terminal: None,
            worktree: None,
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

/// The task's tmux window target. Other backends' endpoints are not tmux
/// targets, and remote ones (`remote:<id>`) are not on this machine.
fn tmux_target(t: &Task) -> Option<String> {
    if !matches!(t.backend.as_deref(), None | Some("tmux")) {
        return None;
    }
    let target = t.endpoint.as_ref()?.target.as_deref()?;
    if target.starts_with("remote:") || !target.contains(':') {
        return None;
    }
    Some(target.to_string())
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

#[cfg(test)]
mod tests {
    use super::spawned_window;

    #[test]
    fn spawned_window_is_the_local_tmux_target() {
        let line = "spawned prj_1 harness=claude kind=secondmate window=quark:prj_1 worktree=/w";
        assert_eq!(spawned_window(line).as_deref(), Some("quark:prj_1"));
        let remote = "spawned prj_1 harness=claude kind=secondmate mode=secondmate yolo=off \
                      window=remote:prj_1 worktree=/w remote=box backend=tmux";
        assert_eq!(spawned_window(remote), None);
        assert_eq!(spawned_window("spawned prj_1 harness=claude"), None);
    }
}
