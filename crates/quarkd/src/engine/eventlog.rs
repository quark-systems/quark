//! Slice 1's native read path: firstmate's fleet read back from the event
//! log instead of from firstmate's files.
//!
//! [`EventLogEngine`] answers the slice-1 reads (`snapshot`, `status_tail`,
//! `holds`) from [`FirstmateFleet`], the fold of what the ingest bridge
//! mirrored into `events.db`. Before each read it runs the bridge over the
//! Project's home and catches the fold up, so it answers from the same
//! moment firstmate does. Every other operation belongs to a later slice
//! and is refused: [`super::shadow::ShadowEngine`] only asks this engine
//! for slice-1 reads.
//!
//! What it can't answer yet: the log carries no backlog, so queued work
//! that has no worker is missing; and firstmate's current state also reads
//! the worker's pane and validation run, which the log does not carry, so a
//! task's state here is what its status log says.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use quark_core::{replay, ProjectId, TaskId};
use quark_eventlog::{FirstmateBridge, FirstmateFleet, FleetTask, SqliteEventLog};
use quark_systems::{AgentConfig, DeliveryPolicy, MergeMethod, TaskKind};

use super::{
    EngineAdapter, EngineError, EngineTask, FleetSnapshot, Hold, SourceRepo, StatusEntry,
    StatusTail, TaskControl, WorkspacePlan, WorkspaceRef,
};

/// `state_source` of every task this engine reports.
pub const SOURCE: &str = "event-log";
const REPLAY_BATCH: usize = 1000;

pub struct EventLogEngine {
    log: SqliteEventLog,
    bridge: FirstmateBridge,
    fleet: FirstmateFleet,
}

impl EventLogEngine {
    /// `bridge` must write to `log`; share it with the daemon's ingest loop
    /// so the two never ingest at once.
    pub fn new(log: SqliteEventLog, bridge: FirstmateBridge) -> Self {
        Self {
            log,
            bridge,
            fleet: FirstmateFleet::new(),
        }
    }

    async fn catch_up(&self, ws: &WorkspaceRef) -> Result<ProjectId, EngineError> {
        let project = ProjectId::new(ws.project_id.clone());
        self.bridge
            .ingest(&project, &ws.root)
            .await
            .map_err(|e| EngineError::Command(format!("event log ingest: {e}")))?;
        replay(&self.log, &self.fleet, REPLAY_BATCH)
            .await
            .map_err(|e| EngineError::Command(format!("event log replay: {e}")))?;
        Ok(project)
    }

    fn refuse<T>(op: &str) -> Result<T, EngineError> {
        Err(EngineError::Invalid(format!(
            "{op} is not served from the event log"
        )))
    }
}

fn engine_task(t: &FleetTask) -> EngineTask {
    EngineTask {
        id: t.task.to_string(),
        title: t.task.to_string(),
        kind: match t.kind.as_deref() {
            Some("scout") => Some(TaskKind::Scout),
            Some("ship") => Some(TaskKind::Ship),
            _ => None,
        },
        state: t.state,
        state_note: t.current.as_ref().map(|c| c.note.clone()),
        state_source: Some(SOURCE.to_string()),
        harness: t.harness.clone(),
        pull_request_url: t.pull_request.clone(),
        terminal: None,
        worktree: None,
    }
}

#[async_trait]
impl EngineAdapter for EventLogEngine {
    fn name(&self) -> &'static str {
        "event-log"
    }

    async fn snapshot(&self, ws: &WorkspaceRef) -> Result<FleetSnapshot, EngineError> {
        let project = self.catch_up(ws).await?;
        Ok(FleetSnapshot {
            tasks: self
                .fleet
                .tasks(&project)
                .iter()
                .filter(|t| t.kind.as_deref() != Some("secondmate"))
                .map(engine_task)
                .collect(),
        })
    }

    async fn status_tail(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
        offset: u64,
    ) -> Result<StatusTail, EngineError> {
        quark_engine::validate_task_id(task_id)
            .map_err(|_| EngineError::TaskNotFound(task_id.to_string()))?;
        let project = self.catch_up(ws).await?;
        let lines = self
            .fleet
            .get(&project, &TaskId::from(task_id))
            .map(|t| t.lines)
            .unwrap_or_default();
        let mut next_offset = offset;
        let entries = lines
            .into_iter()
            .filter(|l| l.offset >= offset)
            .map(|l| {
                next_offset = l.offset + l.raw.len() as u64 + 1;
                StatusEntry {
                    kind: l.verb,
                    decision_key: l.key,
                    note: l.note,
                    raw: l.raw,
                }
            })
            .collect();
        Ok(StatusTail {
            entries,
            next_offset,
        })
    }

    async fn holds(&self, ws: &WorkspaceRef) -> Result<Vec<Hold>, EngineError> {
        let project = self.catch_up(ws).await?;
        Ok(self
            .fleet
            .tasks(&project)
            .iter()
            .flat_map(|t| {
                t.open.iter().map(|d| Hold {
                    id: format!("{}:{}", t.task, d.key),
                    task_id: Some(t.task.to_string()),
                    question: d.note.clone(),
                    answer: None,
                    answered_by: None,
                })
            })
            .collect())
    }

    async fn send_message(&self, _: &WorkspaceRef, _: &str, _: &str) -> Result<(), EngineError> {
        Self::refuse("send_message")
    }

    async fn answer(&self, _: &WorkspaceRef, _: &str, _: &str, _: &str) -> Result<(), EngineError> {
        Self::refuse("answer")
    }

    async fn control(&self, _: &WorkspaceRef, _: &str, _: &TaskControl) -> Result<(), EngineError> {
        Self::refuse("control")
    }

    async fn merge_pull_request(
        &self,
        _: &WorkspaceRef,
        _: &str,
        _: &str,
        _: Option<MergeMethod>,
    ) -> Result<(), EngineError> {
        Self::refuse("merge_pull_request")
    }

    async fn set_standing_approval(
        &self,
        _: &WorkspaceRef,
        _: &[String],
        _: bool,
    ) -> Result<(), EngineError> {
        Self::refuse("set_standing_approval")
    }

    async fn add_source(
        &self,
        _: &Path,
        _: &SourceRepo,
        _: DeliveryPolicy,
    ) -> Result<(), EngineError> {
        Self::refuse("add_source")
    }

    async fn seed_workspace(&self, _: &Path, _: &WorkspacePlan) -> Result<PathBuf, EngineError> {
        Self::refuse("seed_workspace")
    }

    async fn start_coordinator(
        &self,
        _: &Path,
        _: &WorkspaceRef,
        _: &AgentConfig,
        _: &[(String, String)],
    ) -> Result<(), EngineError> {
        Self::refuse("start_coordinator")
    }
}

/// Wrap `bash` in a [`super::shadow::ShadowEngine`] whose slice-1 reads
/// are also answered from the event log, when slice 1 is in shadow.
pub fn shadowed(
    bash: Arc<dyn EngineAdapter>,
    switch: quark_core::SliceSwitch,
    log: SqliteEventLog,
    bridge: FirstmateBridge,
    host: quark_core::HostId,
) -> Arc<dyn EngineAdapter> {
    if switch.mode(quark_core::Slice::EventLog) == quark_core::SliceMode::Bash {
        return bash;
    }
    let native = Arc::new(EventLogEngine::new(log.clone(), bridge));
    Arc::new(
        super::shadow::ShadowEngine::new(bash, native, switch, Arc::new(log), host)
            .comparing(&[quark_core::Slice::EventLog]),
    )
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use quark_core::HostId;
    use quark_systems::TaskState;

    use super::*;

    fn append(path: &Path, text: &str) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    #[tokio::test]
    async fn reads_the_fleet_back_from_the_log() {
        let home = tempfile::tempdir().unwrap();
        let state = home.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(
            state.join("t1.meta"),
            "spawn_gen=s1.1.1\nharness=codex\nkind=ship\n",
        )
        .unwrap();
        append(
            &state.join("t1.status"),
            "working: go\nneeds-decision [key=api]: which shape\n",
        );
        std::fs::write(
            state.join("m.meta"),
            "spawn_gen=s1.1.2\nharness=claude\nkind=secondmate\n",
        )
        .unwrap();
        let db = tempfile::tempdir().unwrap();
        let log = SqliteEventLog::open(db.path().join("events.db")).unwrap();
        let engine = EventLogEngine::new(log.clone(), FirstmateBridge::new(log, HostId::from("h")));
        let ws = WorkspaceRef {
            project_id: "p".into(),
            root: home.path().into(),
        };

        let snap = engine.snapshot(&ws).await.unwrap();
        assert_eq!(snap.tasks.len(), 1, "secondmates are not tasks");
        assert_eq!(snap.tasks[0].state, TaskState::NeedsDecision);
        assert_eq!(snap.tasks[0].harness.as_deref(), Some("codex"));

        let holds = engine.holds(&ws).await.unwrap();
        assert_eq!(holds[0].id, "t1:api");
        assert_eq!(holds[0].question, "which shape");

        let tail = engine.status_tail(&ws, "t1", 0).await.unwrap();
        assert_eq!(tail.entries.len(), 2);
        assert_eq!(
            tail.next_offset,
            std::fs::metadata(state.join("t1.status")).unwrap().len()
        );
        append(&state.join("t1.status"), "resolved [key=api]: b\n");
        let more = engine
            .status_tail(&ws, "t1", tail.next_offset)
            .await
            .unwrap();
        assert_eq!(more.entries.len(), 1);
        assert_eq!(more.entries[0].kind, "resolved");
        assert!(engine.holds(&ws).await.unwrap().is_empty());

        // Cleanup removes the record.
        std::fs::remove_file(state.join("t1.meta")).unwrap();
        assert!(engine.snapshot(&ws).await.unwrap().tasks.is_empty());
    }
}
