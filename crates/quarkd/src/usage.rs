//! Token use of every worker and coordinator turn, mirrored into the event
//! log as `usage.turn` events.
//!
//! Each pass finds each Project's coordinator session log (from its
//! workspace root, any harness) and each task worker's (from its working
//! copy and harness), as the transcript tap does, and appends the turns
//! completed since the last pass ([`quark_transcript::read_turns`]). A log
//! quiet for [`QUIET`] has its open turn closed, so a worker's last turn is
//! counted before its working copy is cleaned up; anything it writes later
//! comes as a continuation of that turn.
//!
//! Read positions are kept per log file in one checkpoint per agent,
//! written in the same transaction as the events, so a turn lands exactly
//! once however the daemon dies. A relaunched agent's earlier log is
//! finished before it is left. Spend is priced when it is read
//! ([`crate::spend`]), not stored, so a price fix applies to past turns.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use quark_core::{HostId, NewEvent, ProjectId, TaskId};
use quark_eventlog::SqliteEventLog;
use quark_transcript::{read_turns, SessionFormat, SessionRoots, TurnUsage, UsageCursor};
use serde::{Deserialize, Serialize};

use crate::store::Store;
use crate::transcripts::{locate_any, roots_with_accounts};

/// The event kind. Payload: [`UsageTurn`].
pub const KIND: &str = "usage.turn";

/// How long a log must go unwritten before its open turn is counted.
pub const QUIET: Duration = Duration::from_secs(120);

/// Whose turn it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Agent {
    Worker,
    Coordinator,
}

/// One `usage.turn` event. A worker's carries its task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageTurn {
    pub agent: Agent,
    /// The session-log format: `claude`, `codex` or `pi`.
    pub harness: String,
    #[serde(flatten)]
    pub turn: TurnUsage,
}

/// One agent's read positions, by log path.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Logs {
    logs: BTreeMap<String, LogPos>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct LogPos {
    format: String,
    #[serde(flatten)]
    cursor: UsageCursor,
}

fn format_named(name: &str) -> Option<SessionFormat> {
    SessionFormat::ALL.into_iter().find(|f| f.as_str() == name)
}

pub struct UsageIngest {
    store: Arc<Store>,
    log: SqliteEventLog,
    host: HostId,
    roots: SessionRoots,
}

impl UsageIngest {
    pub fn new(store: Arc<Store>, log: SqliteEventLog, host: HostId, roots: SessionRoots) -> Self {
        Self {
            store,
            log,
            host,
            roots,
        }
    }

    /// One pass over every Project with a workspace.
    pub async fn ingest_all(&self) -> anyhow::Result<usize> {
        let store = self.store.clone();
        let projects = tokio::task::spawn_blocking(move || store.list_projects()).await??;
        let mut turns = 0;
        for project in projects {
            let Some(ws) = project.workspace_path else {
                continue;
            };
            match self.ingest_project(&project.id, PathBuf::from(ws)).await {
                Ok(n) => turns += n,
                Err(e) => {
                    tracing::warn!(project = %project.id, error = %format!("{e:#}"), "usage ingest failed")
                }
            }
        }
        Ok(turns)
    }

    async fn ingest_project(&self, project_id: &str, ws: PathBuf) -> anyhow::Result<usize> {
        let store = self.store.clone();
        let roots = self.roots.clone();
        let pid = project_id.to_string();
        // Locating scans harness directories: off the async threads.
        let (coordinator, workers) = tokio::task::spawn_blocking(move || {
            let roots = roots_with_accounts(&store, &roots);
            let coordinator = locate_any(&ws, &SessionFormat::ALL, &roots);
            let workers = store
                .task_agents(&pid)?
                .into_iter()
                .filter_map(|(id, harness, worktree)| {
                    let format = SessionFormat::for_harness(&harness)?;
                    let log =
                        worktree.and_then(|w| locate_any(&PathBuf::from(w), &[format], &roots));
                    Some((id, log))
                })
                .collect::<Vec<_>>();
            Ok::<_, anyhow::Error>((coordinator, workers))
        })
        .await??;
        let project = ProjectId::new(project_id);
        let mut n = self
            .ingest_agent(&project, None, Agent::Coordinator, coordinator)
            .await?
            .0;
        for (task, log) in workers {
            let (count, model) = self
                .ingest_agent(
                    &project,
                    Some(TaskId::from(task.as_str())),
                    Agent::Worker,
                    log,
                )
                .await?;
            n += count;
            if let Some(model) = model {
                let store = self.store.clone();
                let pid = project_id.to_string();
                tokio::task::spawn_blocking(move || store.set_task_model_seen(&pid, &task, &model))
                    .await??;
            }
        }
        Ok(n)
    }

    /// Appends one agent's new turns. Returns how many, and the model its
    /// latest turn used most.
    async fn ingest_agent(
        &self,
        project: &ProjectId,
        task: Option<TaskId>,
        agent: Agent,
        current: Option<(PathBuf, SessionFormat)>,
    ) -> anyhow::Result<(usize, Option<String>)> {
        let name = match &task {
            Some(t) => format!("usage/{project}/task/{t}"),
            None => format!("usage/{project}/coordinator"),
        };
        let saved: Logs = self
            .log
            .checkpoint(&name)
            .await?
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default();
        if current.is_none() && saved.logs.is_empty() {
            return Ok((0, None));
        }
        let read = {
            let saved = saved.clone();
            tokio::task::spawn_blocking(move || read_new(saved, current))
        }
        .await??;
        let (logs, turns) = read;
        if logs == saved {
            return Ok((0, None));
        }
        let latest = turns.last().and_then(|(_, t)| {
            t.models
                .iter()
                .filter(|m| !m.model.is_empty())
                .max_by_key(|m| m.output)
                .map(|m| m.model.clone())
        });
        let mut events = Vec::with_capacity(turns.len());
        for (format, turn) in &turns {
            let payload = UsageTurn {
                agent,
                harness: format.as_str().to_string(),
                turn: turn.clone(),
            };
            events.push(NewEvent::typed(
                self.host.clone(),
                project.clone(),
                task.clone(),
                KIND,
                &payload,
            )?);
        }
        let value = serde_json::to_string(&logs)?;
        self.log.append_batch(events, Some((name, value))).await?;
        Ok((turns.len(), latest))
    }

    pub async fn run(self, interval: Duration) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match self.ingest_all().await {
                Ok(n) if n > 0 => tracing::debug!(turns = n, "usage ingest"),
                Ok(_) => {}
                Err(e) => tracing::error!(error = %format!("{e:#}"), "usage ingest pass failed"),
            }
        }
    }
}

/// Reads every log of one agent: the one it writes now, and any earlier
/// one with a turn still open, which is closed. A log that is gone is
/// forgotten. Blocking.
fn read_new(
    mut logs: Logs,
    current: Option<(PathBuf, SessionFormat)>,
) -> std::io::Result<(Logs, Vec<(SessionFormat, TurnUsage)>)> {
    let current_path = current
        .as_ref()
        .map(|(p, _)| p.to_string_lossy().into_owned());
    if let Some((path, format)) = &current {
        logs.logs
            .entry(path.to_string_lossy().into_owned())
            .or_insert_with(|| LogPos {
                format: format.as_str().to_string(),
                cursor: UsageCursor::default(),
            });
    }
    let mut turns = Vec::new();
    let paths: Vec<String> = logs.logs.keys().cloned().collect();
    for path in paths {
        let is_current = current_path.as_deref() == Some(path.as_str());
        let pos = logs.logs.get_mut(&path).expect("listed");
        let Some(format) = format_named(&pos.format) else {
            continue;
        };
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                logs.logs.remove(&path);
                continue;
            }
            Err(e) => return Err(e),
        };
        let quiet = meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .is_some_and(|age| age >= QUIET);
        // Read to its end: nothing new, and no open turn left to close.
        if pos.cursor.offset == meta.len() {
            continue;
        }
        let (new, next) = read_turns(
            std::path::Path::new(&path),
            &pos.cursor,
            format,
            !is_current || quiet,
        )?;
        pos.cursor = next;
        turns.extend(new.into_iter().map(|t| (format, t)));
    }
    Ok((logs, turns))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use quark_core::{EventLog, Seq};
    use quark_systems::CreateProject;
    use serde_json::json;

    use super::*;

    fn append(path: &std::path::Path, lines: &[serde_json::Value]) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    fn user(id: &str) -> serde_json::Value {
        json!({"type": "user", "uuid": id, "timestamp": "2026-10-07T10:00:00Z",
            "message": {"role": "user", "content": "go"}})
    }

    fn call(id: &str, model: &str, output: u64) -> serde_json::Value {
        json!({"type": "assistant", "message": {"id": id, "model": model,
            "usage": {"input_tokens": 100, "output_tokens": output}}})
    }

    /// Claude Code's log directory for an agent working in `cwd`.
    fn log_dir(claude: &std::path::Path, cwd: &std::path::Path) -> PathBuf {
        let slug: String = cwd
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let dir = claude.join("projects").join(slug);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn mirrors_coordinator_and_worker_turns_once() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        let claude = dir.path().join("claude");
        let coord_log = log_dir(&claude, &ws).join("c.jsonl");
        let worker_log = log_dir(&claude, &wt).join("w.jsonl");
        append(
            &coord_log,
            &[user("c1"), call("m1", "claude-opus-5-5", 5), user("c2")],
        );
        append(
            &worker_log,
            &[user("w1"), call("m2", "claude-sonnet-5-5", 7), user("w2")],
        );

        let store = Arc::new(Store::open_in_memory().unwrap());
        let project = store
            .create_project(CreateProject {
                name: "Quark".into(),
                goal: None,
                workspace_path: Some(ws.display().to_string()),
                repos: vec![],
                agent_config: None,
                dispatch_preset: None,
                delivery: None,
            })
            .unwrap();
        store
            .apply_snapshot(
                &project.id,
                &crate::engine::FleetSnapshot {
                    tasks: vec![crate::engine::EngineTask {
                        id: "fix-42".into(),
                        title: "Fix".into(),
                        kind: None,
                        state: quark_systems::TaskState::Running,
                        state_note: None,
                        state_source: None,
                        harness: Some("claude".into()),
                        pull_request_url: None,
                        terminal: None,
                        worktree: Some(wt.clone()),
                    }],
                },
            )
            .unwrap();
        let log = SqliteEventLog::open(dir.path().join("events.db")).unwrap();
        let ingest = UsageIngest::new(
            store.clone(),
            log.clone(),
            HostId::from("h"),
            SessionRoots {
                claude: vec![claude],
                ..Default::default()
            },
        );
        assert_eq!(ingest.ingest_all().await.unwrap(), 2);
        assert_eq!(ingest.ingest_all().await.unwrap(), 0);

        let events = log.read(Seq::ZERO, 100).await.unwrap();
        let turns: Vec<(Option<String>, UsageTurn)> = events
            .iter()
            .filter(|e| e.kind.as_str() == KIND)
            .map(|e| (e.task.as_ref().map(|t| t.to_string()), e.decode().unwrap()))
            .collect();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].0, None);
        assert_eq!(turns[0].1.agent, Agent::Coordinator);
        assert_eq!(turns[0].1.turn.id, "c1");
        assert_eq!(turns[1].0.as_deref(), Some("fix-42"));
        assert_eq!(turns[1].1.agent, Agent::Worker);
        assert_eq!(turns[1].1.turn.models[0].output, 7);

        let task = store.list_tasks(&project.id).unwrap().remove(0);
        assert_eq!(task.model.as_deref(), Some("claude-sonnet-5-5"));
    }
}
