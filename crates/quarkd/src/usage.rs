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
//! IBM Bob keeps no session log: its database (`~/.bob/db/bob.db`) holds
//! each task's running spend in Bobcoins. A Bob worker's tasks in its
//! working copy each count as one turn, reported again as a continuation
//! whenever its spend grows. Kiro CLI logs credits, not tokens, per turn.
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
use quark_transcript::{
    read_turns, ModelUsage, SessionFormat, SessionRoots, TurnUsage, UsageCursor, BOBCOIN,
};
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
    /// The session-log format (`claude`, `codex`, `pi`, `kiro`), or `bob`.
    pub harness: String,
    #[serde(flatten)]
    pub turn: TurnUsage,
}

/// One agent's read positions, by log path, and for a Bob worker what
/// each of its Bob tasks had spent when last read.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Logs {
    logs: BTreeMap<String, LogPos>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    bob: BTreeMap<String, BobSpent>,
}

/// A Bob task's spend so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
struct BobSpent {
    coins: f64,
    #[serde(default)]
    input: u64,
    #[serde(default)]
    output: u64,
    #[serde(default)]
    cache_read: u64,
    #[serde(default)]
    cache_write: u64,
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
    /// IBM Bob's database.
    bob_db: PathBuf,
}

impl UsageIngest {
    pub fn new(store: Arc<Store>, log: SqliteEventLog, host: HostId, roots: SessionRoots) -> Self {
        Self {
            store,
            log,
            host,
            roots,
            bob_db: std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".bob").join("db").join("bob.db"))
                .unwrap_or_default(),
        }
    }

    /// Reads Bob's spend from `db` instead of `~/.bob/db/bob.db`.
    pub fn with_bob_db(mut self, db: PathBuf) -> Self {
        self.bob_db = db;
        self
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
            let coordinator = locate_any(&ws, &SessionFormat::COORDINATOR, &roots, None);
            let workers = store
                .task_agents(&pid)?
                .into_iter()
                .filter_map(|a| {
                    let id = a.engine_id;
                    if crate::engine::engine_harness(&a.harness) == "bob" {
                        return Some((
                            id,
                            Source::Bob(a.worktree.map(PathBuf::from), a.worktree_at),
                        ));
                    }
                    let format = SessionFormat::for_harness(&a.harness)?;
                    let log = a.worktree.and_then(|w| {
                        locate_any(&PathBuf::from(w), &[format], &roots, a.worktree_at)
                    });
                    Some((id, Source::Log(log)))
                })
                .collect::<Vec<_>>();
            Ok::<_, anyhow::Error>((coordinator, workers))
        })
        .await??;
        let project = ProjectId::new(project_id);
        let mut n = self
            .ingest_agent(&project, None, Agent::Coordinator, Source::Log(coordinator))
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
        source: Source,
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
        let read = match source {
            Source::Log(current) => {
                if current.is_none() && saved.logs.is_empty() {
                    return Ok((0, None));
                }
                let saved = saved.clone();
                tokio::task::spawn_blocking(move || read_new(saved, current))
                    .await?
                    .map_err(anyhow::Error::from)
            }
            Source::Bob(None, _) => return Ok((0, None)),
            Source::Bob(Some(worktree), since) => {
                let saved = saved.clone();
                let db = self.bob_db.clone();
                tokio::task::spawn_blocking(move || read_bob(saved, &db, &worktree, since)).await?
            }
        };
        let (logs, turns) = read?;
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
        for (harness, turn) in &turns {
            let payload = UsageTurn {
                agent,
                harness: harness.to_string(),
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

/// Where an agent's usage is read from.
enum Source {
    /// Its session log, if one is found.
    Log(Option<(PathBuf, SessionFormat)>),
    /// IBM Bob's database, for the worker's working copy if it has one,
    /// and when the task took it.
    Bob(Option<PathBuf>, Option<SystemTime>),
}

/// The growth in spend of each Bob task run in `worktree` since `logs` was
/// saved, one turn per task that spent more. Tasks created before `since`
/// are an earlier occupant's of a reused slot. Blocking.
fn read_bob(
    mut logs: Logs,
    db: &std::path::Path,
    worktree: &std::path::Path,
    since: Option<SystemTime>,
) -> anyhow::Result<(Logs, Vec<(&'static str, TurnUsage)>)> {
    let tasks =
        quark_dispatch::local::bob::task_costs(db, worktree).map_err(|e| anyhow::anyhow!(e))?;
    let since_ms = since
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as f64);
    let mut turns = Vec::new();
    for t in tasks {
        if since_ms.is_some_and(|since| t.created_ms.is_some_and(|c| c < since)) {
            continue;
        }
        let before = logs.bob.get(&t.id).copied();
        let was = before.unwrap_or_default();
        let now = BobSpent {
            coins: t.coins,
            input: t.input,
            output: t.output,
            cache_read: t.cache_read,
            cache_write: t.cache_write,
        };
        // Spend only grows; a smaller figure is a rewritten row, not a refund.
        if now.coins <= was.coins + 1e-9 && before.is_some() {
            continue;
        }
        let (read, write) = (
            now.cache_read.saturating_sub(was.cache_read),
            now.cache_write.saturating_sub(was.cache_write),
        );
        let at = t
            .created_ms
            .and_then(|ms| {
                time::OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000).ok()
            })
            .and_then(|d| {
                d.format(&time::format_description::well_known::Rfc3339)
                    .ok()
            });
        turns.push((
            "bob",
            TurnUsage {
                id: t.id.clone(),
                at,
                continued: before.is_some(),
                models: vec![ModelUsage {
                    model: t.model.clone().unwrap_or_else(|| "bob".to_string()),
                    input: now.input.saturating_sub(was.input) + read + write,
                    output: now.output.saturating_sub(was.output),
                    cache_read: read,
                    cache_write: write,
                    calls: u32::from(before.is_none()),
                    cost_usd: None,
                    credits: Some((now.coins - was.coins).max(0.0)),
                    credit_unit: Some(BOBCOIN.to_string()),
                }],
                context: None,
            },
        ));
        logs.bob.insert(t.id, now);
    }
    Ok((logs, turns))
}

/// Reads every log of one agent: the one it writes now, and any earlier
/// one with a turn still open, which is closed. A log that is gone is
/// forgotten. Blocking.
fn read_new(
    mut logs: Logs,
    current: Option<(PathBuf, SessionFormat)>,
) -> std::io::Result<(Logs, Vec<(&'static str, TurnUsage)>)> {
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
        turns.extend(new.into_iter().map(|t| (format.as_str(), t)));
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
                        worktree_at: None,
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

    #[tokio::test]
    async fn mirrors_bob_spend_and_kiro_credits() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        let bob_wt = dir.path().join("bob-wt");
        let kiro_wt = dir.path().join("kiro-wt");
        for d in [&ws, &bob_wt, &kiro_wt] {
            std::fs::create_dir_all(d).unwrap();
        }
        let bob_db = dir.path().join("bob.db");
        let con = rusqlite::Connection::open(&bob_db).unwrap();
        con.execute_batch(
            "CREATE TABLE tasks (id TEXT PRIMARY KEY, parent_id TEXT, directory TEXT,
                task_type TEXT, created_at INTEGER, costs TEXT);",
        )
        .unwrap();
        con.execute(
            "INSERT INTO tasks VALUES ('t1', NULL, ?1, 'normal', 1792576800000, '{\"cost\": 2}')",
            [bob_wt.to_string_lossy()],
        )
        .unwrap();
        let kiro_home = dir.path().join(".kiro");
        let sess = kiro_home.join("sessions/ab12/sess_1");
        std::fs::create_dir_all(&sess).unwrap();
        std::fs::write(
            sess.join("session.json"),
            json!({"id": "sess_1", "modelId": "auto", "workspacePaths": [kiro_wt]}).to_string(),
        )
        .unwrap();
        append(
            &sess.join("messages.jsonl"),
            &[
                json!({"id": "k1", "payload": {"type": "user", "content": "go"}}),
                json!({"id": "s", "payload": {"type": "usage_summary", "executionId": "e1",
                    "promptTurnSummaries": [{"unit": "credit", "usage": 1.25}]}}),
                json!({"id": "k2", "payload": {"type": "user", "content": "more"}}),
            ],
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
        let task = |id: &str, harness: &str, wt: &PathBuf| crate::engine::EngineTask {
            id: id.into(),
            title: id.into(),
            kind: None,
            state: quark_systems::TaskState::Running,
            state_note: None,
            state_source: None,
            harness: Some(harness.into()),
            pull_request_url: None,
            terminal: None,
            worktree: Some(wt.clone()),
            worktree_at: None,
        };
        store
            .apply_snapshot(
                &project.id,
                &crate::engine::FleetSnapshot {
                    tasks: vec![task("b", "bob", &bob_wt), task("k", "kiro", &kiro_wt)],
                },
            )
            .unwrap();
        let log = SqliteEventLog::open(dir.path().join("events.db")).unwrap();
        let ingest = UsageIngest::new(
            store.clone(),
            log.clone(),
            HostId::from("h"),
            SessionRoots {
                kiro: vec![kiro_home],
                ..Default::default()
            },
        )
        .with_bob_db(bob_db);
        assert_eq!(ingest.ingest_all().await.unwrap(), 2);
        assert_eq!(ingest.ingest_all().await.unwrap(), 0);
        // Bob's task spends more: counted as a continuation of its turn.
        con.execute("UPDATE tasks SET costs = '{\"cost\": 3.5}'", [])
            .unwrap();
        assert_eq!(ingest.ingest_all().await.unwrap(), 1);

        let events = log.read(Seq::ZERO, 100).await.unwrap();
        let turns: Vec<(String, UsageTurn)> = events
            .iter()
            .filter(|e| e.kind.as_str() == KIND)
            .map(|e| (e.task.as_ref().unwrap().to_string(), e.decode().unwrap()))
            .collect();
        let of = |task: &str| -> Vec<&UsageTurn> {
            turns
                .iter()
                .filter(|(t, _)| t == task)
                .map(|(_, u)| u)
                .collect()
        };
        let bob = of("b");
        assert_eq!(bob.len(), 2);
        assert_eq!(bob[0].harness, "bob");
        assert_eq!(bob[0].turn.id, "t1");
        assert_eq!(bob[0].turn.at.as_deref(), Some("2026-10-21T10:00:00Z"));
        assert!(!bob[0].turn.continued && bob[1].turn.continued);
        assert_eq!(bob[0].turn.models[0].credits, Some(2.0));
        assert_eq!(bob[1].turn.models[0].credits, Some(1.5));
        let kiro = of("k");
        assert_eq!(kiro.len(), 1);
        assert_eq!(kiro[0].harness, "kiro");
        assert_eq!(kiro[0].turn.models[0].model, "kiro-auto");
        assert_eq!(kiro[0].turn.models[0].credits, Some(1.25));
    }
}
