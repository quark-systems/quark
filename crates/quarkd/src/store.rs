//! SQLite projection and append-only event log.
//!
//! The engine's files are the source of truth for orchestration state; this
//! database is a rebuildable read model. Every projection change and the
//! events it produces commit in one transaction, and events are published to
//! live subscribers only after that commit, in `seq` order.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;

use crate::engine::{FleetSnapshot, Hold, StatusEntry};
use quark_systems::{
    AgentConfig, CreateProject, Decision, DecisionBrief, DecisionState, DeliveryPolicy,
    DispatchPreset, DispatchRecord, DispatchTrigger, Event, EventType, Project, ProjectStatus,
    RepoSource, Task, TaskEvent, TaskKind, TaskState, TranscriptEntry, TranscriptItem,
    UpdateProject,
};
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};
use tokio::sync::broadcast;

use crate::now_rfc3339;

mod accounts;
mod decisions;
mod failover;
mod memory;
mod migrations;
mod pull_requests;

pub use accounts::{default_account_id, AccountRow};
pub use failover::is_daemon_decision;
pub use memory::{NewProposal, PendingLearning};
pub use pull_requests::{artifact_id, artifact_path, PrOwner, PrSyncTarget};

/// One `worker.output` event to append.
#[derive(Debug, Clone)]
pub struct TerminalOutput {
    pub project_id: Option<String>,
    pub output: quark_systems::TerminalOutput,
}

/// A task whose status log the projector tails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailTarget {
    pub task_id: String,
    pub engine_id: String,
    pub offset: u64,
}

/// Capacity of the live event channel. A subscriber that falls further behind
/// than this catches up from the store instead.
const BUS_CAPACITY: usize = 1024;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("not found")]
    NotFound,
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// A transcript the daemon projects into events.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TranscriptSource {
    /// A Project's coordinator; its id is the Project id.
    Coordinator { project_id: String },
    /// A worker, by API task id.
    Task { task_id: String },
}

impl TranscriptSource {
    fn key(&self) -> String {
        match self {
            TranscriptSource::Coordinator { project_id } => format!("coordinator:{project_id}"),
            TranscriptSource::Task { task_id } => format!("task:{task_id}"),
        }
    }
}

/// The answerer's reason and channel, for [`Store::answer_decision_with`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnswerNote {
    pub why: Option<String>,
    pub via: Option<String>,
}

/// Engine coordinates of a decision, from [`Store::decision_target`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionTarget {
    pub project_id: String,
    /// The engine's id for the question ([`Hold::id`]).
    pub engine_id: String,
    pub open: bool,
    pub workspace_path: Option<String>,
}

/// Engine coordinates of a task, from [`Store::task_target`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskTarget {
    pub project_id: String,
    pub engine_id: String,
    pub workspace_path: Option<String>,
}

/// One engine script call as stored in `adapter_calls`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptCall {
    pub ts: String,
    /// Filled in from the workspace on insert; ignored by `record_script_call`.
    pub project_id: Option<String>,
    /// `read` or `write`.
    pub kind: String,
    pub script: String,
    pub args: Vec<String>,
    pub workspace: String,
    pub ok: bool,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    /// Failure detail: stderr tail, or why the script could not run.
    pub detail: Option<String>,
}

pub struct Store {
    conn: Mutex<Connection>,
    bus: broadcast::Sender<Event>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Store> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Store> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        // A rebuildable read model: with WAL, NORMAL can lose the last commits
        // on power loss but never corrupts, and it keeps terminal output
        // from costing an fsync per chunk.
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        migrations::migrate(&mut conn)?;
        let (bus, _) = broadcast::channel(BUS_CAPACITY);
        Ok(Store {
            conn: Mutex::new(conn),
            bus,
        })
    }

    /// Live events committed after this call.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.bus.subscribe()
    }

    fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self.conn.lock().unwrap();
        f(&conn)
    }

    /// Runs `f` in a transaction, then publishes the events it appended. The
    /// connection stays locked while publishing so live order matches `seq`.
    fn write<T>(&self, f: impl FnOnce(&Transaction, &mut Vec<Event>) -> Result<T>) -> Result<T> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let mut events = Vec::new();
        let out = f(&tx, &mut events)?;
        tx.commit()?;
        for event in events {
            // No receivers is fine: clients replay from the store.
            let _ = self.bus.send(event);
        }
        Ok(out)
    }

    pub fn last_seq(&self) -> Result<i64> {
        self.read(
            |c| Ok(c.query_row("SELECT COALESCE(MAX(seq), 0) FROM events", [], |r| r.get(0))?),
        )
    }

    /// Events with `seq > after`, oldest first, at most `limit`.
    pub fn events_after(&self, after: i64, limit: u32) -> Result<Vec<Event>> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT seq, project_id, type, ts, payload FROM events
                 WHERE seq > ?1 ORDER BY seq LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![after, limit], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (seq, project_id, ty, ts, payload) = row?;
                let event_type = EventType::parse(&ty)
                    .ok_or_else(|| StoreError::Invalid(format!("unknown event type {ty}")))?;
                out.push(Event {
                    seq,
                    project_id,
                    event_type,
                    ts,
                    payload: serde_json::from_str(&payload)?,
                });
            }
            Ok(out)
        })
    }

    // Projects

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        self.read(|c| {
            let mut stmt = c.prepare(&format!("{PROJECT_SELECT} ORDER BY created_at, id"))?;
            let rows = stmt.query_map([], project_from_row)?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })
    }

    pub fn get_project(&self, id: &str) -> Result<Project> {
        self.read(|c| get_project(c, id))
    }

    /// Records a Project. One created with repos starts out `provisioning`;
    /// the caller then drives provisioning and reports through
    /// [`Store::set_project_status`].
    pub fn create_project(&self, input: CreateProject) -> Result<Project> {
        let name = input.name.trim().to_string();
        if name.is_empty() {
            return Err(StoreError::Invalid("name must not be empty".into()));
        }
        self.write(|tx, events| {
            let now = now_rfc3339();
            let provisioning = !input.repos.is_empty();
            let project = Project {
                id: new_id("prj"),
                name,
                goal: input.goal,
                workspace_path: input.workspace_path,
                status: if provisioning {
                    ProjectStatus::Provisioning
                } else {
                    ProjectStatus::Ready
                },
                status_detail: None,
                repos: input.repos,
                agent_config: input.agent_config,
                dispatch_preset: input.dispatch_preset,
                delivery: input.delivery,
                project_repo_path: None,
                standing_approval: false,
                created_at: now.clone(),
                updated_at: now,
            };
            tx.execute(
                "INSERT INTO projects (id, name, goal, workspace_path, created_at, updated_at,
                     status, status_detail, spec, project_repo_path)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    project.id,
                    project.name,
                    project.goal,
                    project.workspace_path,
                    project.created_at,
                    project.updated_at,
                    project.status.as_str(),
                    project.status_detail,
                    spec_json(&project)?,
                    project.project_repo_path,
                ],
            )?;
            append_event(
                tx,
                events,
                Some(&project.id),
                EventType::ProjectUpdated,
                serde_json::to_value(&project)?,
            )?;
            Ok(project)
        })
    }

    /// Moves a failed Project back to `provisioning` so it can be retried.
    /// Refuses any other status, atomically, so two retries never both run.
    pub fn retry_provisioning(&self, id: &str) -> Result<Project> {
        self.write(|tx, events| {
            let mut project = get_project(tx, id)?;
            if project.status != ProjectStatus::Failed || project.repos.is_empty() {
                return Err(StoreError::Conflict(
                    "only a Project whose provisioning failed can be provisioned again".into(),
                ));
            }
            project.status = ProjectStatus::Provisioning;
            project.status_detail = Some("Retrying".into());
            project.updated_at = now_rfc3339();
            tx.execute(
                "UPDATE projects SET status = ?2, status_detail = ?3, updated_at = ?4 WHERE id = ?1",
                params![
                    project.id,
                    project.status.as_str(),
                    project.status_detail,
                    project.updated_at
                ],
            )?;
            append_event(
                tx,
                events,
                Some(&project.id),
                EventType::ProjectUpdated,
                serde_json::to_value(&project)?,
            )?;
            Ok(project)
        })
    }

    /// Moves a Project through provisioning. `workspace_path` and
    /// `project_repo_path` are set when given and kept otherwise.
    pub fn set_project_status(
        &self,
        id: &str,
        status: ProjectStatus,
        detail: Option<&str>,
        workspace_path: Option<&str>,
        project_repo_path: Option<&str>,
    ) -> Result<Project> {
        self.write(|tx, events| {
            let mut project = get_project(tx, id)?;
            project.status = status;
            project.status_detail = detail.map(str::to_string);
            if let Some(p) = workspace_path {
                project.workspace_path = Some(p.to_string());
            }
            if let Some(p) = project_repo_path {
                project.project_repo_path = Some(p.to_string());
            }
            project.updated_at = now_rfc3339();
            tx.execute(
                "UPDATE projects SET status = ?2, status_detail = ?3, workspace_path = ?4,
                     project_repo_path = ?5, updated_at = ?6
                 WHERE id = ?1",
                params![
                    project.id,
                    project.status.as_str(),
                    project.status_detail,
                    project.workspace_path,
                    project.project_repo_path,
                    project.updated_at
                ],
            )?;
            append_event(
                tx,
                events,
                Some(&project.id),
                EventType::ProjectUpdated,
                serde_json::to_value(&project)?,
            )?;
            Ok(project)
        })
    }

    pub fn update_project(&self, id: &str, input: UpdateProject) -> Result<Project> {
        if let Some(name) = &input.name {
            if name.trim().is_empty() {
                return Err(StoreError::Invalid("name must not be empty".into()));
            }
        }
        self.write(|tx, events| {
            let mut project = get_project(tx, id)?;
            if let Some(name) = input.name {
                project.name = name.trim().to_string();
            }
            if input.goal.is_some() {
                project.goal = input.goal;
            }
            if input.workspace_path.is_some() {
                project.workspace_path = input.workspace_path;
            }
            if let Some(on) = input.standing_approval {
                project.standing_approval = on;
            }
            project.updated_at = now_rfc3339();
            tx.execute(
                "UPDATE projects SET name = ?2, goal = ?3, workspace_path = ?4, updated_at = ?5,
                     standing_approval = ?6
                 WHERE id = ?1",
                params![
                    project.id,
                    project.name,
                    project.goal,
                    project.workspace_path,
                    project.updated_at,
                    project.standing_approval
                ],
            )?;
            append_event(
                tx,
                events,
                Some(&project.id),
                EventType::ProjectUpdated,
                serde_json::to_value(&project)?,
            )?;
            Ok(project)
        })
    }

    // Tasks

    pub fn list_tasks(&self, project_id: &str) -> Result<Vec<Task>> {
        self.read(|c| {
            get_project(c, project_id)?;
            let mut stmt = c.prepare(&format!(
                "{TASK_SELECT} WHERE project_id = ?1 ORDER BY created_at, id"
            ))?;
            let rows = stmt.query_map([project_id], task_from_row)?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })
    }

    pub fn get_task(&self, id: &str) -> Result<Task> {
        self.read(|c| {
            c.query_row(&format!("{TASK_SELECT} WHERE id = ?1"), [id], task_from_row)
                .optional()?
                .ok_or(StoreError::NotFound)
        })
    }

    /// Where to reach a task in its engine: the owning Project, the engine's
    /// own task id and the Project's workspace, if one is attached.
    pub fn task_target(&self, id: &str) -> Result<TaskTarget> {
        self.read(|c| {
            c.query_row(
                "SELECT t.project_id, t.engine_id, p.workspace_path
                 FROM tasks t JOIN projects p ON p.id = t.project_id
                 WHERE t.id = ?1",
                [id],
                |r| {
                    Ok(TaskTarget {
                        project_id: r.get(0)?,
                        engine_id: r.get(1)?,
                        workspace_path: r.get(2)?,
                    })
                },
            )
            .optional()?
            .ok_or(StoreError::NotFound)
        })
    }

    /// Projects one engine snapshot: inserts new tasks and updates changed
    /// ones, emitting `task.created` and `task.state_changed`. Tasks missing
    /// from the snapshot are kept as history.
    pub fn apply_snapshot(&self, project_id: &str, snapshot: &FleetSnapshot) -> Result<()> {
        self.write(|tx, events| {
            let mut existing: HashMap<String, Task> = HashMap::new();
            {
                let mut stmt = tx.prepare(&format!(
                    "SELECT engine_id, {TASK_COLUMNS} FROM tasks WHERE project_id = ?1"
                ))?;
                let rows = stmt.query_map([project_id], |r| {
                    Ok((r.get::<_, String>(0)?, task_from_row_at(r, 1)?))
                })?;
                for row in rows {
                    let (engine_id, task) = row?;
                    existing.insert(engine_id, task);
                }
            }

            // A task whose pull request merged is done, whatever the engine
            // still reports for it; this and `apply_forge_pr` are the only
            // places that move a task there.
            let merged = merged_pull_requests(tx, project_id)?;
            let mut read: HashSet<&str> = HashSet::new();
            for et in &snapshot.tasks {
                // The engine could not read this task just now. Keep what is
                // recorded, or record nothing for a task not yet known, and
                // read it again next refresh. The sweep below still finishes
                // a merged one.
                if et.is_transient() {
                    continue;
                }
                read.insert(&et.id);
                let now = now_rfc3339();
                let worktree = et
                    .worktree
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned());
                match existing.get(&et.id) {
                    None => {
                        let task = Task {
                            id: new_id("tsk"),
                            project_id: project_id.to_string(),
                            title: et.title.clone(),
                            kind: et.kind,
                            state: et.state,
                            state_note: et.state_note.clone(),
                            harness: et.harness.clone(),
                            pull_request_url: et.pull_request_url.clone(),
                            account_id: et
                                .harness
                                .as_deref()
                                .and_then(|h| accounts::inherited_account(tx, project_id, h)),
                            failovers: Vec::new(),
                            model: None,
                            branch: None,
                            created_at: now.clone(),
                            updated_at: now,
                        };
                        tx.execute(
                            "INSERT INTO tasks (id, project_id, engine_id, title, kind, state,
                                 state_note, harness, pull_request_url, created_at, updated_at,
                                 worktree_path, account_id)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                            params![
                                task.id,
                                task.project_id,
                                et.id,
                                task.title,
                                task.kind.map(TaskKind::as_str),
                                task.state.as_str(),
                                task.state_note,
                                task.harness,
                                task.pull_request_url,
                                task.created_at,
                                task.updated_at,
                                worktree,
                                task.account_id
                            ],
                        )?;
                        append_event(
                            tx,
                            events,
                            Some(project_id),
                            EventType::TaskCreated,
                            serde_json::to_value(&task)?,
                        )?;
                    }
                    Some(old) => {
                        let (state, state_note) = if et
                            .pull_request_url
                            .as_ref()
                            .is_some_and(|u| merged.contains(u))
                        {
                            merged_state(old)
                        } else {
                            (et.state, et.state_note.clone())
                        };
                        let changed = old.title != et.title
                            || old.kind != et.kind
                            || old.state != state
                            || old.state_note != state_note
                            || old.harness != et.harness
                            || old.pull_request_url != et.pull_request_url;
                        // The worktree is not part of the API's task, so a
                        // move alone updates the row without an event.
                        tx.execute(
                            "UPDATE tasks SET worktree_path = ?3
                             WHERE project_id = ?1 AND engine_id = ?2
                               AND worktree_path IS NOT ?3",
                            params![project_id, et.id, worktree],
                        )?;
                        if !changed {
                            continue;
                        }
                        let task = Task {
                            title: et.title.clone(),
                            kind: et.kind,
                            state,
                            state_note,
                            harness: et.harness.clone(),
                            pull_request_url: et.pull_request_url.clone(),
                            updated_at: now,
                            ..old.clone()
                        };
                        tx.execute(
                            "UPDATE tasks SET title = ?2, kind = ?3, state = ?4, state_note = ?5,
                                 harness = ?6, pull_request_url = ?7, updated_at = ?8
                             WHERE id = ?1",
                            params![
                                task.id,
                                task.title,
                                task.kind.map(TaskKind::as_str),
                                task.state.as_str(),
                                task.state_note,
                                task.harness,
                                task.pull_request_url,
                                task.updated_at
                            ],
                        )?;
                        append_event(
                            tx,
                            events,
                            Some(project_id),
                            EventType::TaskStateChanged,
                            serde_json::json!({
                                "task": task,
                                "previous_state": old.state,
                            }),
                        )?;
                    }
                }
            }
            // Tasks the snapshot no longer lists (the engine cleaned them up
            // after their merge) or could not read just now.
            for (engine_id, old) in &existing {
                if !read.contains(engine_id.as_str()) {
                    finish_if_merged(tx, events, &merged, old)?;
                }
            }
            Ok(())
        })
    }

    /// Records the model a task's worker was started with (`None` for the
    /// harness default) and the branch its working copy has out, emitting
    /// `task.state_changed` when the task's model or branch changed.
    pub fn set_task_agent(
        &self,
        project_id: &str,
        engine_id: &str,
        model: Option<&str>,
        branch: Option<&str>,
    ) -> Result<()> {
        self.update_task_agent(
            project_id,
            engine_id,
            "UPDATE tasks SET model = ?3, branch = ?4
             WHERE project_id = ?1 AND engine_id = ?2
               AND (model IS NOT ?3 OR branch IS NOT ?4)",
            params![project_id, engine_id, model, branch],
        )
    }

    /// Records the model a task's session log last reported.
    pub fn set_task_model_seen(
        &self,
        project_id: &str,
        engine_id: &str,
        model: &str,
    ) -> Result<()> {
        self.update_task_agent(
            project_id,
            engine_id,
            "UPDATE tasks SET model_seen = ?3
             WHERE project_id = ?1 AND engine_id = ?2 AND model_seen IS NOT ?3",
            params![project_id, engine_id, model],
        )
    }

    fn update_task_agent(
        &self,
        project_id: &str,
        engine_id: &str,
        sql: &str,
        args: &[&dyn rusqlite::ToSql],
    ) -> Result<()> {
        self.write(|tx, events| {
            let read = |tx: &rusqlite::Transaction| {
                tx.query_row(
                    &format!("{TASK_SELECT} WHERE project_id = ?1 AND engine_id = ?2"),
                    params![project_id, engine_id],
                    task_from_row,
                )
                .optional()
            };
            let Some(before) = read(tx)? else {
                return Ok(());
            };
            if tx.execute(sql, args)? == 0 {
                return Ok(());
            }
            let Some(task) = read(tx)? else {
                return Ok(());
            };
            // Not a move of the task's own, so `updated_at` stays.
            if task.model == before.model && task.branch == before.branch {
                return Ok(());
            }
            append_event(
                tx,
                events,
                Some(project_id),
                EventType::TaskStateChanged,
                serde_json::json!({ "task": task, "previous_state": before.state }),
            )?;
            Ok(())
        })
    }

    /// Each of a Project's tasks that ran a worker: engine id, harness and
    /// working copy (while it exists), for reading its session log.
    pub fn task_agents(&self, project_id: &str) -> Result<Vec<(String, String, Option<String>)>> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT engine_id, harness, worktree_path FROM tasks
                 WHERE project_id = ?1 AND harness IS NOT NULL ORDER BY engine_id",
            )?;
            let rows = stmt.query_map([project_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }

    // Dispatch records

    /// Spawn generations already recorded for each of a Project's tasks.
    pub fn dispatch_generations(&self, project_id: &str) -> Result<HashMap<String, Vec<String>>> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT task_id, generation FROM dispatch_records WHERE project_id = ?1",
            )?;
            let rows = stmt.query_map([project_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            let mut out: HashMap<String, Vec<String>> = HashMap::new();
            for row in rows {
                let (task, generation) = row?;
                out.entry(task).or_default().push(generation);
            }
            Ok(out)
        })
    }

    /// Stores the record of one spawn, assigning its id and time, and emits
    /// `dispatch.recorded`. A record that names no account gets the account
    /// the task runs under, if one is known. A relaunch that moved the worker
    /// to another account after a rate limit notes that failover. A
    /// generation already recorded for the task is left as it was and
    /// returns `None`.
    pub fn record_dispatch(
        &self,
        record: &DispatchRecord,
        generation: &str,
    ) -> Result<Option<DispatchRecord>> {
        self.write(|tx, events| {
            let mut record = DispatchRecord {
                id: new_id("dsp"),
                recorded_at: now_rfc3339(),
                ..record.clone()
            };
            if record.chosen.account.is_none() {
                record.chosen.account = tx
                    .query_row(
                        "SELECT account_id FROM tasks WHERE id = ?1",
                        [&record.task_id],
                        |r| r.get::<_, Option<String>>(0),
                    )
                    .optional()?
                    .flatten();
            }
            if record.trigger == DispatchTrigger::Relaunch && record.failover.is_none() {
                if let Some(f) = failover::unrecorded(tx, &record.task_id)? {
                    record.summary = format!(
                        "{} Quark moved the worker from account {} to {} after a rate limit.",
                        record.summary,
                        f.from_account_id,
                        f.to_account_id.as_deref().unwrap_or("another account")
                    );
                    record.failover = Some(f);
                }
            }
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO dispatch_records
                     (id, task_id, project_id, generation, recorded_at, record)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    record.id,
                    record.task_id,
                    record.project_id,
                    generation,
                    record.recorded_at,
                    serde_json::to_string(&record)?
                ],
            )?;
            if inserted == 0 {
                return Ok(None);
            }
            append_event(
                tx,
                events,
                Some(&record.project_id),
                EventType::DispatchRecorded,
                serde_json::to_value(&record)?,
            )?;
            Ok(Some(record))
        })
    }

    /// A task's dispatch records, oldest first: its first spawn, then each
    /// relaunch.
    pub fn list_dispatch(&self, task_id: &str) -> Result<Vec<DispatchRecord>> {
        self.read(|c| {
            let known: bool = c.query_row(
                "SELECT EXISTS (SELECT 1 FROM tasks WHERE id = ?1)
                     OR EXISTS (SELECT 1 FROM dispatch_records WHERE task_id = ?1)",
                [task_id],
                |r| r.get(0),
            )?;
            if !known {
                return Err(StoreError::NotFound);
            }
            let mut stmt = c.prepare(
                "SELECT record FROM dispatch_records WHERE task_id = ?1
                 ORDER BY recorded_at, id",
            )?;
            let rows = stmt.query_map([task_id], |r| r.get::<_, String>(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(serde_json::from_str(&row?)?);
            }
            Ok(out)
        })
    }

    /// The task's working tree on this machine, if the engine reported one.
    pub fn task_worktree(&self, id: &str) -> Result<Option<String>> {
        self.read(|c| {
            c.query_row("SELECT worktree_path FROM tasks WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?
            .ok_or(StoreError::NotFound)
        })
    }

    /// Every task of a Project with the offset its status log was read to.
    pub fn tail_targets(&self, project_id: &str) -> Result<Vec<TailTarget>> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT t.id, t.engine_id, COALESCE(s.byte_offset, 0)
                 FROM tasks t LEFT JOIN status_cursors s ON s.task_id = t.id
                 WHERE t.project_id = ?1 ORDER BY t.created_at, t.id",
            )?;
            let rows = stmt.query_map([project_id], |r| {
                Ok(TailTarget {
                    task_id: r.get(0)?,
                    engine_id: r.get(1)?,
                    offset: r.get::<_, i64>(2)? as u64,
                })
            })?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })
    }

    /// Records new status entries for a task and advances its cursor,
    /// emitting one `task.event` per entry. `from` must be the offset the
    /// entries were read from; a stale call (another refresh already moved the
    /// cursor) is dropped so entries are never recorded twice.
    pub fn apply_status(
        &self,
        task_id: &str,
        from: u64,
        entries: &[StatusEntry],
        next_offset: u64,
    ) -> Result<()> {
        self.write(|tx, events| {
            let (project_id, current): (String, Option<i64>) = tx
                .query_row(
                    "SELECT t.project_id, s.byte_offset
                     FROM tasks t LEFT JOIN status_cursors s ON s.task_id = t.id
                     WHERE t.id = ?1",
                    [task_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?
                .ok_or(StoreError::NotFound)?;
            if current.unwrap_or(0) as u64 != from {
                return Ok(());
            }
            for entry in entries {
                let ts = now_rfc3339();
                tx.execute(
                    "INSERT INTO task_events (task_id, project_id, kind, decision_key, note, raw, ts)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        task_id,
                        project_id,
                        entry.kind,
                        entry.decision_key,
                        entry.note,
                        entry.raw,
                        ts
                    ],
                )?;
                let event = TaskEvent {
                    id: tx.last_insert_rowid(),
                    task_id: task_id.to_string(),
                    project_id: project_id.clone(),
                    kind: entry.kind.clone(),
                    decision_key: entry.decision_key.clone(),
                    note: entry.note.clone(),
                    ts,
                };
                append_event(
                    tx,
                    events,
                    Some(&project_id),
                    EventType::TaskEvent,
                    serde_json::to_value(&event)?,
                )?;
            }
            if current.map(|c| c as u64) != Some(next_offset) {
                tx.execute(
                    "INSERT INTO status_cursors (task_id, byte_offset) VALUES (?1, ?2)
                     ON CONFLICT (task_id) DO UPDATE SET byte_offset = excluded.byte_offset",
                    params![task_id, next_offset as i64],
                )?;
            }
            Ok(())
        })
    }

    /// A task's activity log, oldest first, entries with `id > after`.
    pub fn list_task_events(
        &self,
        task_id: &str,
        after: i64,
        limit: u32,
    ) -> Result<Vec<TaskEvent>> {
        self.read(|c| {
            c.query_row("SELECT 1 FROM tasks WHERE id = ?1", [task_id], |_| Ok(()))
                .optional()?
                .ok_or(StoreError::NotFound)?;
            let mut stmt = c.prepare(
                "SELECT id, task_id, project_id, kind, decision_key, note, ts FROM task_events
                 WHERE task_id = ?1 AND id > ?2 ORDER BY id LIMIT ?3",
            )?;
            let rows = stmt.query_map(params![task_id, after, limit], |r| {
                Ok(TaskEvent {
                    id: r.get(0)?,
                    task_id: r.get(1)?,
                    project_id: r.get(2)?,
                    kind: r.get(3)?,
                    decision_key: r.get(4)?,
                    note: r.get(5)?,
                    ts: r.get(6)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })
    }

    // Decisions

    pub fn list_decisions(&self, state: Option<DecisionState>) -> Result<Vec<Decision>> {
        self.list_project_decisions(None, state)
    }

    /// Decisions in one Project (every Project when `None`), oldest first.
    pub fn list_project_decisions(
        &self,
        project_id: Option<&str>,
        state: Option<DecisionState>,
    ) -> Result<Vec<Decision>> {
        self.read(|c| {
            let sql = format!(
                "{DECISION_SELECT} WHERE (?1 IS NULL OR state = ?1)
                   AND (?2 IS NULL OR project_id = ?2)
                 ORDER BY opened_at, id"
            );
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map(
                params![state.map(decision_state_str), project_id],
                decision_from_row,
            )?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })
    }

    /// One decision by id.
    pub fn get_decision(&self, id: &str) -> Result<Decision> {
        self.read(|c| {
            c.query_row(
                &format!("{DECISION_SELECT} WHERE id = ?1"),
                [id],
                decision_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound)
        })
    }

    /// Where to answer a decision in its engine: the owning Project, the
    /// engine's id for the question and the Project's workspace, if attached.
    pub fn decision_target(&self, id: &str) -> Result<DecisionTarget> {
        self.read(|c| {
            c.query_row(
                "SELECT d.project_id, d.engine_id, d.state, p.workspace_path
                 FROM decisions d JOIN projects p ON p.id = d.project_id
                 WHERE d.id = ?1",
                [id],
                |r| {
                    Ok(DecisionTarget {
                        project_id: r.get(0)?,
                        engine_id: r.get(1)?,
                        open: r.get::<_, String>(2)? == "open",
                        workspace_path: r.get(3)?,
                    })
                },
            )
            .optional()?
            .ok_or(StoreError::NotFound)
        })
    }

    /// Records an answer the engine has accepted, emitting
    /// `decision.answered`. Conflicts when the decision already has an answer.
    pub fn answer_decision(&self, id: &str, answer: &str, answered_by: &str) -> Result<Decision> {
        self.answer_decision_with(id, answer, answered_by, &AnswerNote::default())
    }

    /// [`Self::answer_decision`] with the answerer's reason and channel.
    pub fn answer_decision_with(
        &self,
        id: &str,
        answer: &str,
        answered_by: &str,
        note: &AnswerNote,
    ) -> Result<Decision> {
        self.write(|tx, events| {
            let old = tx
                .query_row(
                    &format!("{DECISION_SELECT} WHERE id = ?1"),
                    [id],
                    decision_from_row,
                )
                .optional()?
                .ok_or(StoreError::NotFound)?;
            // A refresh that saw the hold go before this call recorded it
            // marks the decision answered elsewhere, with no answer; this
            // answer is the one that closed it.
            let answered_elsewhere = old.answer.is_none() && old.answered_by.is_none();
            if old.state != DecisionState::Open && !answered_elsewhere {
                return Err(StoreError::Conflict(
                    "the decision is already answered".into(),
                ));
            }
            let decision = Decision {
                state: DecisionState::Answered,
                answer: Some(answer.to_string()),
                answered_by: Some(answered_by.to_string()),
                answered_at: Some(now_rfc3339()),
                answered_via: note.via.clone(),
                answer_why: note.why.clone(),
                ..old
            };
            mark_answered(tx, events, &decision)?;
            Ok(decision)
        })
    }

    /// Projects the engine's open holds into decisions, as read by a refresh
    /// that started at `observed_at` (RFC 3339), before it read the holds.
    ///
    /// - A new hold opens a decision (`decision.opened`).
    /// - An open decision whose hold carries an answer, or whose hold is gone,
    ///   becomes answered (`decision.answered`). A hold that went without an
    ///   answer through Quark was answered elsewhere, so its `answer` and
    ///   `answered_by` stay empty.
    /// - A hold that comes back after its decision was answered is the
    ///   question asked again, and opens a new decision.
    ///
    /// Each change also needs `observed_at` to be later than the decision's
    /// last change, so a refresh that read the engine before an answer (or
    /// before the decision opened) cannot undo it.
    pub fn apply_holds(&self, project_id: &str, holds: &[Hold], observed_at: &str) -> Result<()> {
        self.write(|tx, events| {
            for hold in holds {
                let existing = tx
                    .query_row(
                        &format!(
                            "{DECISION_SELECT} WHERE project_id = ?1 AND engine_id = ?2
                             ORDER BY rowid DESC LIMIT 1"
                        ),
                        params![project_id, hold.id],
                        decision_from_row,
                    )
                    .optional()?;
                let brief = api_task_ids(tx, project_id, hold.brief.clone().normalized())?;
                match existing {
                    Some(old) if old.state == DecisionState::Open => {
                        if hold.answer.is_none() && !brief.is_empty() && brief != old.brief {
                            // The asker added or changed its brief.
                            let decision = Decision { brief, ..old };
                            tx.execute(
                                "UPDATE decisions SET brief = ?2 WHERE id = ?1",
                                params![decision.id, serde_json::to_string(&decision.brief)?],
                            )?;
                            append_event(
                                tx,
                                events,
                                Some(project_id),
                                EventType::DecisionOpened,
                                serde_json::to_value(&decision)?,
                            )?;
                        } else if hold.answer.is_some() {
                            let decision = Decision {
                                state: DecisionState::Answered,
                                answer: hold.answer.clone(),
                                answered_by: hold.answered_by.clone(),
                                answered_at: Some(now_rfc3339()),
                                ..old
                            };
                            mark_answered(tx, events, &decision)?;
                        }
                    }
                    Some(old)
                        if hold.answer.is_some()
                            || !later(observed_at, old.answered_at.as_deref()) => {}
                    _ => open_decision(tx, events, project_id, hold)?,
                }
            }

            let open: Vec<(String, Decision)> = {
                let mut stmt = tx.prepare(&format!(
                    "SELECT engine_id, {DECISION_COLUMNS}
                     FROM decisions WHERE project_id = ?1 AND state = 'open'"
                ))?;
                let rows = stmt.query_map([project_id], |r| {
                    Ok((r.get(0)?, decision_from_row_at(r, 1)?))
                })?;
                rows.collect::<std::result::Result<_, _>>()?
            };
            for (engine_id, old) in open {
                // The daemon's own questions have no hold to go away.
                if failover::is_daemon_decision(&engine_id)
                    || holds.iter().any(|h| h.id == engine_id)
                    || !later(observed_at, Some(&old.opened_at))
                {
                    continue;
                }
                let decision = Decision {
                    state: DecisionState::Answered,
                    answered_at: Some(now_rfc3339()),
                    ..old
                };
                mark_answered(tx, events, &decision)?;
            }
            Ok(())
        })
    }

    // Transcripts

    /// The API id of the task the engine knows as `engine_id`.
    pub fn task_id_for_engine(&self, project_id: &str, engine_id: &str) -> Result<Option<String>> {
        self.read(|c| {
            Ok(c.query_row(
                "SELECT id FROM tasks WHERE project_id = ?1 AND engine_id = ?2",
                params![project_id, engine_id],
                |r| r.get(0),
            )
            .optional()?)
        })
    }

    /// The session log a transcript source was last read from, and the offset
    /// to continue at.
    pub fn transcript_cursor(&self, source: &TranscriptSource) -> Result<Option<(String, u64)>> {
        self.read(|c| {
            Ok(c.query_row(
                r#"SELECT path, "offset" FROM transcript_index WHERE source = ?1"#,
                [source.key()],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64)),
            )
            .optional()?)
        })
    }

    /// Transcript entries already projected for `source`, oldest first, with
    /// event `seq > after`, at most `limit`.
    pub fn transcript(
        &self,
        source: &TranscriptSource,
        after: i64,
        limit: u32,
    ) -> Result<Vec<TranscriptItem>> {
        let (event_type, key, id) = match source {
            TranscriptSource::Coordinator { project_id } => (
                EventType::CoordinatorMessage,
                "$.coordinator_id",
                project_id,
            ),
            TranscriptSource::Task { task_id } => {
                (EventType::WorkerTranscript, "$.task_id", task_id)
            }
        };
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT seq, json_extract(payload, '$.entry') FROM events
                 WHERE type = ?1 AND json_extract(payload, ?2) = ?3 AND seq > ?4
                 ORDER BY seq LIMIT ?5",
            )?;
            let rows = stmt
                .query_map(params![event_type.as_str(), key, id, after, limit], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                })?;
            let mut out = Vec::new();
            for row in rows {
                let (id, entry) = row?;
                out.push(TranscriptItem {
                    id,
                    entry: serde_json::from_str(&entry)?,
                });
            }
            Ok(out)
        })
    }

    /// Appends one event per entry read from `path` and moves the source's
    /// cursor to `next_offset`, atomically, so a restart neither repeats nor
    /// skips an entry.
    pub fn apply_transcript(
        &self,
        project_id: &str,
        source: &TranscriptSource,
        path: &str,
        next_offset: u64,
        entries: &[TranscriptEntry],
    ) -> Result<()> {
        self.write(|tx, events| {
            for entry in entries {
                let (event_type, payload) = match source {
                    TranscriptSource::Coordinator { project_id } => (
                        EventType::CoordinatorMessage,
                        serde_json::json!({ "coordinator_id": project_id, "entry": entry }),
                    ),
                    TranscriptSource::Task { task_id } => (
                        EventType::WorkerTranscript,
                        serde_json::json!({ "task_id": task_id, "entry": entry }),
                    ),
                };
                append_event(tx, events, Some(project_id), event_type, payload)?;
            }
            tx.execute(
                r#"INSERT INTO transcript_index (source, project_id, path, "offset", updated_at)
                   VALUES (?1, ?2, ?3, ?4, ?5)
                   ON CONFLICT (source) DO UPDATE SET
                       path = excluded.path, "offset" = excluded."offset",
                       updated_at = excluded.updated_at"#,
                params![
                    source.key(),
                    project_id,
                    path,
                    next_offset as i64,
                    now_rfc3339()
                ],
            )?;
            Ok(())
        })
    }

    /// Engine task id to daemon task id for one Project.
    pub fn task_ids_by_engine(&self, project_id: &str) -> Result<HashMap<String, String>> {
        self.read(|c| {
            let mut stmt = c.prepare("SELECT engine_id, id FROM tasks WHERE project_id = ?1")?;
            let rows = stmt.query_map([project_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })
    }

    // Terminal output

    /// Appends `worker.output` events in one transaction and returns them.
    pub fn append_terminal_output(&self, chunks: &[TerminalOutput]) -> Result<Vec<Event>> {
        self.write(|tx, events| {
            for chunk in chunks {
                let project_id = chunk.project_id.as_deref();
                let size = chunk.output.data_b64.len() / 4 * 3;
                append_subject_event(
                    tx,
                    events,
                    project_id,
                    EventType::WorkerOutput,
                    serde_json::to_value(&chunk.output)?,
                    Some((&chunk.output.terminal_id, size)),
                )?;
            }
            Ok(events.clone())
        })
    }

    /// Deletes a terminal's oldest output events so that about `keep_bytes`
    /// remain, then everything before the oldest snapshot still kept, so a
    /// replay of the terminal starts from a full screen. Replay after pruning
    /// has `seq` gaps; `seq` stays strictly increasing.
    pub fn prune_terminal_output(&self, terminal_id: &str, keep_bytes: u64) -> Result<usize> {
        self.read(|c| {
            let cutoff: Option<i64> = c.query_row(
                "SELECT MIN(seq) FROM (
                     SELECT seq, SUM(size) OVER (ORDER BY seq DESC) AS kept
                     FROM events WHERE subject = ?1
                 ) WHERE kept <= ?2",
                params![terminal_id, keep_bytes as i64],
                |r| r.get(0),
            )?;
            let Some(cutoff) = cutoff else {
                return Ok(0);
            };
            let snapshot: Option<i64> = c.query_row(
                "SELECT MIN(seq) FROM events
                 WHERE subject = ?1 AND seq >= ?2
                   AND json_extract(payload, '$.kind') = 'snapshot'",
                params![terminal_id, cutoff],
                |r| r.get(0),
            )?;
            Ok(c.execute(
                "DELETE FROM events WHERE subject = ?1 AND seq < ?2",
                params![terminal_id, snapshot.unwrap_or(cutoff)],
            )?)
        })
    }

    // Adapter calls

    /// Records one engine script call. The Project is the one whose workspace
    /// the script ran against, when the daemon knows it.
    pub fn record_script_call(&self, call: &ScriptCall) -> Result<()> {
        self.read(|c| {
            let project_id: Option<String> = c
                .query_row(
                    "SELECT id FROM projects WHERE workspace_path = ?1",
                    [&call.workspace],
                    |r| r.get(0),
                )
                .optional()?;
            c.execute(
                "INSERT INTO adapter_calls (ts, project_id, operation, ok, duration_ms, detail,
                     kind, script, args, exit_code, workspace)
                 VALUES (?1, ?2, 'script', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    call.ts,
                    project_id,
                    call.ok,
                    call.duration_ms as i64,
                    call.detail,
                    call.kind,
                    call.script,
                    serde_json::to_string(&call.args)?,
                    call.exit_code,
                    call.workspace
                ],
            )?;
            Ok(())
        })
    }

    /// Outcomes of a Project's most recent adapter calls for `operation`
    /// (`snapshot`, `gates`, `dispatch`...), newest first: whether each
    /// succeeded and its failure detail.
    pub fn recent_adapter_calls(
        &self,
        project_id: &str,
        operation: &str,
        limit: u32,
    ) -> Result<Vec<(bool, Option<String>)>> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT ok, detail FROM adapter_calls
                 WHERE project_id = ?1 AND operation = ?2 ORDER BY id DESC LIMIT ?3",
            )?;
            let rows = stmt.query_map(params![project_id, operation, limit], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }

    /// Most recent script calls first.
    pub fn recent_script_calls(&self, limit: u32) -> Result<Vec<ScriptCall>> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT ts, project_id, ok, duration_ms, detail, kind, script, args, exit_code,
                        workspace
                 FROM adapter_calls WHERE operation = 'script' ORDER BY id DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map([limit], |r| {
                Ok((
                    ScriptCall {
                        ts: r.get(0)?,
                        project_id: r.get(1)?,
                        ok: r.get(2)?,
                        duration_ms: r.get::<_, i64>(3)? as u64,
                        detail: r.get(4)?,
                        kind: r.get(5)?,
                        script: r.get(6)?,
                        args: Vec::new(),
                        exit_code: r.get(8)?,
                        workspace: r.get(9)?,
                    },
                    r.get::<_, String>(7)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (mut call, args) = row?;
                call.args = serde_json::from_str(&args)?;
                out.push(call);
            }
            Ok(out)
        })
    }

    pub fn record_adapter_call(
        &self,
        project_id: Option<&str>,
        operation: &str,
        ok: bool,
        duration_ms: u64,
        detail: Option<&str>,
    ) -> Result<()> {
        self.read(|c| {
            c.execute(
                "INSERT INTO adapter_calls (ts, project_id, operation, ok, duration_ms, detail)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    now_rfc3339(),
                    project_id,
                    operation,
                    ok,
                    duration_ms as i64,
                    detail
                ],
            )?;
            Ok(())
        })
    }
}

/// URLs of the Project's pull requests that merged.
fn merged_pull_requests(tx: &Transaction, project_id: &str) -> Result<HashSet<String>> {
    let mut stmt =
        tx.prepare("SELECT url FROM pull_requests WHERE project_id = ?1 AND state = 'merged'")?;
    let rows = stmt.query_map([project_id], |r| r.get(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// The state and note a task has once its pull request merged. A task that
/// is already done keeps its note, so an engine that keeps reporting the
/// finished task changes nothing.
fn merged_state(old: &Task) -> (TaskState, Option<String>) {
    match old.state {
        TaskState::Done => (TaskState::Done, old.state_note.clone()),
        _ => (TaskState::Done, Some(MERGED_NOTE.to_string())),
    }
}

const MERGED_NOTE: &str = "Pull request merged";

/// Moves `task` to done when its pull request is among `merged`.
fn finish_if_merged(
    tx: &Transaction,
    events: &mut Vec<Event>,
    merged: &HashSet<String>,
    task: &Task,
) -> Result<()> {
    if task
        .pull_request_url
        .as_ref()
        .is_some_and(|u| merged.contains(u))
    {
        finish_task(tx, events, task)?;
    }
    Ok(())
}

/// Moves a task whose pull request merged to done and emits
/// `task.state_changed`; a task already done is left alone.
fn finish_task(tx: &Transaction, events: &mut Vec<Event>, old: &Task) -> Result<()> {
    if old.state == TaskState::Done {
        return Ok(());
    }
    let (state, state_note) = merged_state(old);
    let task = Task {
        state,
        state_note,
        updated_at: now_rfc3339(),
        ..old.clone()
    };
    tx.execute(
        "UPDATE tasks SET state = ?2, state_note = ?3, updated_at = ?4 WHERE id = ?1",
        params![
            task.id,
            task.state.as_str(),
            task.state_note,
            task.updated_at
        ],
    )?;
    append_event(
        tx,
        events,
        Some(&task.project_id),
        EventType::TaskStateChanged,
        serde_json::json!({ "task": task, "previous_state": old.state }),
    )
}

fn append_event(
    tx: &Transaction,
    events: &mut Vec<Event>,
    project_id: Option<&str>,
    event_type: EventType,
    payload: serde_json::Value,
) -> Result<()> {
    append_subject_event(tx, events, project_id, event_type, payload, None)
}

/// [`append_event`] tagged with a subject and size for per-subject pruning.
fn append_subject_event(
    tx: &Transaction,
    events: &mut Vec<Event>,
    project_id: Option<&str>,
    event_type: EventType,
    payload: serde_json::Value,
    subject: Option<(&str, usize)>,
) -> Result<()> {
    let ts = now_rfc3339();
    tx.execute(
        "INSERT INTO events (project_id, type, ts, payload, subject, size)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            project_id,
            event_type.as_str(),
            ts,
            payload.to_string(),
            subject.map(|s| s.0),
            subject.map(|s| s.1 as i64)
        ],
    )?;
    events.push(Event {
        seq: tx.last_insert_rowid(),
        project_id: project_id.map(str::to_string),
        event_type,
        ts,
        payload,
    });
    Ok(())
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::now_v7().simple())
}

const PROJECT_SELECT: &str = "SELECT id, name, goal, workspace_path, created_at, updated_at, \
                              status, status_detail, spec, project_repo_path, \
                              standing_approval FROM projects";

/// The creation inputs kept with a Project row.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct ProjectSpec {
    #[serde(default)]
    repos: Vec<RepoSource>,
    agent_config: Option<AgentConfig>,
    dispatch_preset: Option<DispatchPreset>,
    delivery: Option<DeliveryPolicy>,
}

fn spec_json(p: &Project) -> Result<String> {
    Ok(serde_json::to_string(&ProjectSpec {
        repos: p.repos.clone(),
        agent_config: p.agent_config.clone(),
        dispatch_preset: p.dispatch_preset,
        delivery: p.delivery,
    })?)
}

fn get_project(c: &Connection, id: &str) -> Result<Project> {
    c.query_row(
        &format!("{PROJECT_SELECT} WHERE id = ?1"),
        [id],
        project_from_row,
    )
    .optional()?
    .ok_or(StoreError::NotFound)
}

fn project_from_row(r: &Row) -> rusqlite::Result<Project> {
    let spec: ProjectSpec = r
        .get::<_, Option<String>>(8)?
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    Ok(Project {
        id: r.get(0)?,
        name: r.get(1)?,
        goal: r.get(2)?,
        workspace_path: r.get(3)?,
        created_at: r.get(4)?,
        updated_at: r.get(5)?,
        status: ProjectStatus::parse(&r.get::<_, String>(6)?),
        status_detail: r.get(7)?,
        repos: spec.repos,
        agent_config: spec.agent_config,
        dispatch_preset: spec.dispatch_preset,
        delivery: spec.delivery,
        project_repo_path: r.get(9)?,
        standing_approval: r.get(10)?,
    })
}

const TASK_COLUMNS: &str = "id, project_id, title, kind, state, state_note, harness, \
                            pull_request_url, created_at, updated_at, account_id, failovers, \
                            COALESCE(model_seen, model), branch";
const TASK_SELECT: &str = "SELECT id, project_id, title, kind, state, state_note, harness, \
                           pull_request_url, created_at, updated_at, account_id, failovers, \
                           COALESCE(model_seen, model), branch FROM tasks";

fn task_from_row(r: &Row) -> rusqlite::Result<Task> {
    task_from_row_at(r, 0)
}

fn task_from_row_at(r: &Row, i: usize) -> rusqlite::Result<Task> {
    Ok(Task {
        id: r.get(i)?,
        project_id: r.get(i + 1)?,
        title: r.get(i + 2)?,
        kind: r
            .get::<_, Option<String>>(i + 3)?
            .as_deref()
            .and_then(TaskKind::parse),
        state: TaskState::parse(&r.get::<_, String>(i + 4)?),
        state_note: r.get(i + 5)?,
        harness: r.get(i + 6)?,
        pull_request_url: r.get(i + 7)?,
        created_at: r.get(i + 8)?,
        updated_at: r.get(i + 9)?,
        account_id: r.get(i + 10)?,
        // Written only by this daemon; an unreadable value reads as none.
        failovers: serde_json::from_str(&r.get::<_, String>(i + 11)?).unwrap_or_default(),
        model: r.get(i + 12)?,
        branch: r.get(i + 13)?,
    })
}

/// The columns [`decision_from_row_at`] reads, in order.
macro_rules! decision_columns {
    () => {
        "id, project_id, task_id, question, state, answer, answered_by, opened_at, \
         answered_at, number, brief, answered_via, answer_why, outcome, acted_at, rule_id, \
         made_rule_id"
    };
}

const DECISION_COLUMNS: &str = decision_columns!();
const DECISION_SELECT: &str = concat!("SELECT ", decision_columns!(), " FROM decisions");

fn decision_state_str(s: DecisionState) -> &'static str {
    s.as_str()
}

fn decision_from_row(r: &Row) -> rusqlite::Result<Decision> {
    decision_from_row_at(r, 0)
}

/// A decision whose `DECISION_SELECT` columns start at column `i`.
fn decision_from_row_at(r: &Row, i: usize) -> rusqlite::Result<Decision> {
    Ok(Decision {
        id: r.get(i)?,
        project_id: r.get(i + 1)?,
        task_id: r.get(i + 2)?,
        question: r.get(i + 3)?,
        state: DecisionState::parse(&r.get::<_, String>(i + 4)?),
        answer: r.get(i + 5)?,
        answered_by: r.get(i + 6)?,
        opened_at: r.get(i + 7)?,
        answered_at: r.get(i + 8)?,
        number: r.get(i + 9)?,
        // Written only by this daemon; an unreadable brief reads as empty.
        brief: serde_json::from_str(&r.get::<_, String>(i + 10)?).unwrap_or_default(),
        answered_via: r.get(i + 11)?,
        answer_why: r.get(i + 12)?,
        outcome: r.get(i + 13)?,
        acted_at: r.get(i + 14)?,
        rule_id: r.get(i + 15)?,
        made_rule_id: r.get(i + 16)?,
    })
}

/// Whether RFC 3339 time `a` is strictly later than `b`. An absent `b` is
/// the distant past; an unparsable time is never later, so nothing changes.
pub(crate) fn later(a: &str, b: Option<&str>) -> bool {
    use time::format_description::well_known::Rfc3339;
    let Ok(a) = time::OffsetDateTime::parse(a, &Rfc3339) else {
        return false;
    };
    match b {
        None => true,
        Some(b) => time::OffsetDateTime::parse(b, &Rfc3339).is_ok_and(|b| a > b),
    }
}

/// Opens a decision for `hold` (answered at once when the hold already
/// carries an answer).
fn open_decision(
    tx: &Transaction,
    events: &mut Vec<Event>,
    project_id: &str,
    hold: &Hold,
) -> Result<()> {
    let task_id: Option<String> = match &hold.task_id {
        Some(engine_task) => tx
            .query_row(
                "SELECT id FROM tasks WHERE project_id = ?1 AND engine_id = ?2",
                params![project_id, engine_task],
                |r| r.get(0),
            )
            .optional()?,
        None => None,
    };
    let now = now_rfc3339();
    let answered = hold.answer.is_some();
    let mut decision = Decision {
        id: new_id("dec"),
        project_id: project_id.to_string(),
        task_id,
        question: hold.question.clone(),
        state: if answered {
            DecisionState::Answered
        } else {
            DecisionState::Open
        },
        brief: api_task_ids(tx, project_id, hold.brief.clone().normalized())?,
        answer: hold.answer.clone(),
        answered_by: hold.answered_by.clone(),
        opened_at: now.clone(),
        answered_at: answered.then(|| now.clone()),
        ..Default::default()
    };
    insert_decision(tx, &hold.id, &mut decision)?;
    append_event(
        tx,
        events,
        Some(project_id),
        EventType::DecisionOpened,
        serde_json::to_value(&decision)?,
    )?;
    if answered {
        append_event(
            tx,
            events,
            Some(project_id),
            EventType::DecisionAnswered,
            serde_json::to_value(&decision)?,
        )?;
    }
    Ok(())
}

/// `brief` with each engine task id in `asked_by` and `blocks` replaced by
/// the API id of that task in the Project, so clients can link it.
fn api_task_ids(
    tx: &Transaction,
    project_id: &str,
    mut brief: DecisionBrief,
) -> Result<DecisionBrief> {
    let lookup = |engine_id: &str| -> Result<Option<String>> {
        Ok(tx
            .query_row(
                "SELECT id FROM tasks WHERE project_id = ?1 AND engine_id = ?2",
                params![project_id, engine_id],
                |r| r.get(0),
            )
            .optional()?)
    };
    if let Some(who) = &brief.asked_by {
        if let Some(id) = lookup(who)? {
            brief.asked_by = Some(id);
        }
    }
    for b in &mut brief.blocks {
        if let Some(id) = lookup(b)? {
            *b = id;
        }
    }
    Ok(brief)
}

/// Stores `decision`'s answer fields and emits `decision.answered`.
fn mark_answered(tx: &Transaction, events: &mut Vec<Event>, decision: &Decision) -> Result<()> {
    tx.execute(
        "UPDATE decisions SET state = ?2, answer = ?3, answered_by = ?4, answered_at = ?5,
             answered_via = ?6, answer_why = ?7
         WHERE id = ?1",
        params![
            decision.id,
            decision_state_str(decision.state),
            decision.answer,
            decision.answered_by,
            decision.answered_at,
            decision.answered_via,
            decision.answer_why
        ],
    )?;
    append_event(
        tx,
        events,
        Some(&decision.project_id),
        EventType::DecisionAnswered,
        serde_json::to_value(decision)?,
    )?;
    Ok(())
}

/// Inserts `d` under the Project's next number, which it sets on `d`.
fn insert_decision(tx: &Transaction, engine_id: &str, d: &mut Decision) -> Result<()> {
    d.number = tx.query_row(
        "SELECT COALESCE(MAX(number), 0) + 1 FROM decisions WHERE project_id = ?1",
        [&d.project_id],
        |r| r.get(0),
    )?;
    tx.execute(
        &format!(
            "INSERT INTO decisions (engine_id, {DECISION_COLUMNS})
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)"
        ),
        params![
            engine_id,
            d.id,
            d.project_id,
            d.task_id,
            d.question,
            decision_state_str(d.state),
            d.answer,
            d.answered_by,
            d.opened_at,
            d.answered_at,
            d.number,
            serde_json::to_string(&d.brief)?,
            d.answered_via,
            d.answer_why,
            d.outcome,
            d.acted_at,
            d.rule_id,
            d.made_rule_id
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::EngineTask;

    fn engine_task(id: &str, state: TaskState) -> EngineTask {
        EngineTask {
            id: id.into(),
            title: format!("task {id}"),
            kind: Some(TaskKind::Ship),
            state,
            state_note: None,
            state_source: None,
            harness: Some("claude".into()),
            pull_request_url: None,
            worktree: None,
            terminal: None,
        }
    }

    fn entry(kind: &str, note: &str) -> StatusEntry {
        StatusEntry {
            kind: kind.into(),
            decision_key: Some("default".into()),
            note: note.into(),
            raw: format!("{kind}: {note}"),
        }
    }

    #[test]
    fn status_entries_project_once_and_resume() {
        let store = Store::open_in_memory().unwrap();
        let p = project(&store);
        let mut et = engine_task("a", TaskState::Running);
        et.worktree = Some("/wt/a".into());
        store
            .apply_snapshot(&p.id, &FleetSnapshot { tasks: vec![et] })
            .unwrap();
        let targets = store.tail_targets(&p.id).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].offset, 0);
        let task_id = targets[0].task_id.clone();
        assert_eq!(
            store.task_worktree(&task_id).unwrap().as_deref(),
            Some("/wt/a")
        );

        let entries = [entry("working", "a"), entry("done", "b")];
        store.apply_status(&task_id, 0, &entries, 20).unwrap();
        // A second refresh that read from the old offset is dropped.
        store.apply_status(&task_id, 0, &entries, 20).unwrap();
        assert_eq!(store.tail_targets(&p.id).unwrap()[0].offset, 20);

        let log = store.list_task_events(&task_id, 0, 100).unwrap();
        assert_eq!(
            log.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
            ["working", "done"]
        );
        let page = store.list_task_events(&task_id, log[0].id, 100).unwrap();
        assert_eq!(page.len(), 1);

        let streamed: Vec<_> = store
            .events_after(0, 100)
            .unwrap()
            .into_iter()
            .filter(|e| e.event_type == EventType::TaskEvent)
            .collect();
        assert_eq!(streamed.len(), 2);
        assert_eq!(streamed[1].payload["note"], "b");
        assert_eq!(streamed[1].payload["task_id"], task_id.as_str());
        assert!(matches!(
            store.list_task_events("tsk_missing", 0, 10),
            Err(StoreError::NotFound)
        ));
    }

    fn project(store: &Store) -> Project {
        store
            .create_project(CreateProject {
                name: "demo".into(),
                goal: None,
                workspace_path: Some("/tmp/ws".into()),
                ..Default::default()
            })
            .unwrap()
    }

    #[test]
    fn snapshot_diff_emits_created_then_state_changed() {
        let store = Store::open_in_memory().unwrap();
        let p = project(&store);
        let mut snap = FleetSnapshot {
            tasks: vec![engine_task("a", TaskState::Running)],
        };
        store.apply_snapshot(&p.id, &snap).unwrap();
        // Re-applying an unchanged snapshot is a no-op.
        store.apply_snapshot(&p.id, &snap).unwrap();
        snap.tasks[0].state = TaskState::InReview;
        store.apply_snapshot(&p.id, &snap).unwrap();

        let events = store.events_after(0, 100).unwrap();
        let types: Vec<_> = events.iter().map(|e| e.event_type).collect();
        assert_eq!(
            types,
            [
                EventType::ProjectUpdated,
                EventType::TaskCreated,
                EventType::TaskStateChanged
            ]
        );
        assert_eq!(events[2].payload["previous_state"], "running");
        assert_eq!(events[2].payload["task"]["state"], "in_review");
        let seqs: Vec<_> = events.iter().map(|e| e.seq).collect();
        assert!(seqs.windows(2).all(|w| w[0] < w[1]));

        let tasks = store.list_tasks(&p.id).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].state, TaskState::InReview);
        assert_eq!(store.get_task(&tasks[0].id).unwrap(), tasks[0]);
    }

    #[test]
    fn holds_open_then_answer() {
        let store = Store::open_in_memory().unwrap();
        let p = project(&store);
        store
            .apply_snapshot(
                &p.id,
                &FleetSnapshot {
                    tasks: vec![engine_task("a", TaskState::NeedsDecision)],
                },
            )
            .unwrap();
        let mut hold = Hold {
            id: "h1".into(),
            task_id: Some("a".into()),
            question: "merge?".into(),
            answer: None,
            answered_by: None,
            brief: crate::engine::firstmate::worker_brief("a"),
        };
        store
            .apply_holds(&p.id, std::slice::from_ref(&hold), &now_rfc3339())
            .unwrap();
        let open = store.list_decisions(Some(DecisionState::Open)).unwrap();
        assert_eq!(open.len(), 1);
        assert!(open[0].task_id.is_some());
        // The brief names the task by its API id.
        assert_eq!(open[0].brief.asked_by, open[0].task_id);
        assert_eq!(open[0].brief.blocks, vec![open[0].task_id.clone().unwrap()]);

        hold.answer = Some("yes".into());
        hold.answered_by = Some("user_1".into());
        store
            .apply_holds(&p.id, std::slice::from_ref(&hold), &now_rfc3339())
            .unwrap();
        store
            .apply_holds(&p.id, std::slice::from_ref(&hold), &now_rfc3339())
            .unwrap();
        assert!(store
            .list_decisions(Some(DecisionState::Open))
            .unwrap()
            .is_empty());
        let all = store.list_decisions(None).unwrap();
        assert_eq!(all[0].answered_by.as_deref(), Some("user_1"));

        let answered = store
            .events_after(0, 100)
            .unwrap()
            .into_iter()
            .filter(|e| e.event_type == EventType::DecisionAnswered)
            .count();
        assert_eq!(answered, 1);
    }

    fn decision_events(store: &Store) -> Vec<Event> {
        store
            .events_after(0, 1000)
            .unwrap()
            .into_iter()
            .filter(|e| {
                matches!(
                    e.event_type,
                    EventType::DecisionOpened | EventType::DecisionAnswered
                )
            })
            .collect()
    }

    fn open_hold(id: &str) -> Hold {
        Hold {
            id: id.into(),
            task_id: None,
            question: "which way?".into(),
            answer: None,
            answered_by: None,
            brief: Default::default(),
        }
    }

    #[test]
    fn answer_records_who_answered() {
        let store = Store::open_in_memory().unwrap();
        let p = project(&store);
        store
            .apply_holds(&p.id, &[open_hold("t1:api")], &now_rfc3339())
            .unwrap();
        let open = store.list_decisions(Some(DecisionState::Open)).unwrap();
        let target = store.decision_target(&open[0].id).unwrap();
        assert_eq!(target.engine_id, "t1:api");
        assert!(target.open);

        let d = store.answer_decision(&open[0].id, "REST", "matt").unwrap();
        assert_eq!(d.state, DecisionState::Answered);
        assert_eq!(d.answered_by.as_deref(), Some("matt"));
        assert!(d.answered_at.is_some());
        assert_eq!(store.get_decision(&d.id).unwrap(), d);
        assert!(!store.decision_target(&d.id).unwrap().open);
        assert!(matches!(
            store.answer_decision(&d.id, "RPC", "ana"),
            Err(StoreError::Conflict(_))
        ));
        let events = decision_events(&store);
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].event_type, EventType::DecisionAnswered);
        assert_eq!(events[1].payload["answered_by"], "matt");

        // The engine no longer lists it; a later refresh changes nothing.
        store.apply_holds(&p.id, &[], &now_rfc3339()).unwrap();
        assert_eq!(decision_events(&store).len(), 2);
        assert_eq!(
            store.get_decision(&d.id).unwrap().answer.as_deref(),
            Some("REST")
        );
    }

    #[test]
    fn hold_gone_without_an_answer_was_answered_elsewhere() {
        let store = Store::open_in_memory().unwrap();
        let p = project(&store);
        store
            .apply_holds(&p.id, &[open_hold("h1")], &now_rfc3339())
            .unwrap();
        let id = store.list_decisions(None).unwrap()[0].id.clone();

        // A refresh that read the engine before the decision opened does not
        // close it.
        store
            .apply_holds(&p.id, &[], "2000-01-01T00:00:00Z")
            .unwrap();
        assert_eq!(store.get_decision(&id).unwrap().state, DecisionState::Open);

        store.apply_holds(&p.id, &[], &now_rfc3339()).unwrap();
        let d = store.get_decision(&id).unwrap();
        assert_eq!(d.state, DecisionState::Answered);
        assert_eq!(d.answer, None);
        assert_eq!(d.answered_by, None);

        // The answer that raced the refresh still lands on the decision.
        let d = store.answer_decision(&id, "yes", "matt").unwrap();
        assert_eq!(d.answered_by.as_deref(), Some("matt"));
        assert_eq!(decision_events(&store).len(), 3);
    }

    #[test]
    fn question_asked_again_opens_a_new_decision() {
        let store = Store::open_in_memory().unwrap();
        let p = project(&store);
        store
            .apply_holds(&p.id, &[open_hold("h1")], &now_rfc3339())
            .unwrap();
        let first = store.list_decisions(None).unwrap()[0].id.clone();
        store.answer_decision(&first, "later", "matt").unwrap();

        // A refresh that read the engine before the answer landed still sees
        // the hold; it must not reopen anything.
        store
            .apply_holds(&p.id, &[open_hold("h1")], "2000-01-01T00:00:00Z")
            .unwrap();
        assert_eq!(store.list_decisions(None).unwrap().len(), 1);

        store
            .apply_holds(&p.id, &[open_hold("h1")], &now_rfc3339())
            .unwrap();
        let all = store.list_decisions(None).unwrap();
        assert_eq!(all.len(), 2);
        let open = store.list_decisions(Some(DecisionState::Open)).unwrap();
        assert_eq!(open.len(), 1);
        assert_ne!(open[0].id, first);
        assert_eq!(store.decision_target(&open[0].id).unwrap().engine_id, "h1");
        assert_eq!(
            store.get_decision(&first).unwrap().answer.as_deref(),
            Some("later")
        );
    }

    #[test]
    fn reopen_keeps_data_and_seq() {
        let dir = std::env::temp_dir().join(format!("quarkd-test-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("quark.db");
        let id = {
            let store = Store::open(&path).unwrap();
            project(&store).id
        };
        let store = Store::open(&path).unwrap();
        assert_eq!(store.get_project(&id).unwrap().name, "demo");
        assert_eq!(store.last_seq().unwrap(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_v11_database_migrates_to_dispatch_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quark.db");
        {
            // A database as main's last schema left it, with a task that has
            // an account (v11).
            let mut conn = Connection::open(&path).unwrap();
            let tx = conn.transaction().unwrap();
            for m in &migrations::MIGRATIONS[..11] {
                tx.execute_batch(m.sql).unwrap();
            }
            tx.execute(
                "INSERT INTO projects (id, name, created_at, updated_at) VALUES ('prj_1', 'demo', 't', 't')",
                [],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO tasks (id, project_id, engine_id, title, state, harness, created_at,
                     updated_at, account_id)
                 VALUES ('tsk_1', 'prj_1', 'fix', 'Fix', 'running', 'claude', 't', 't', 'acc_2')",
                [],
            )
            .unwrap();
            tx.pragma_update(None, "user_version", 11).unwrap();
            tx.commit().unwrap();
        }

        let store = Store::open(&path).unwrap();
        let version: i64 = store
            .read(|c| Ok(c.query_row("PRAGMA user_version", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(version, migrations::SCHEMA_VERSION);
        assert_eq!(
            store.get_task("tsk_1").unwrap().account_id.as_deref(),
            Some("acc_2")
        );
        assert!(store.list_dispatch("tsk_1").unwrap().is_empty());

        // The new table works, and a record takes the task's account.
        let spawn = crate::engine::EngineSpawn {
            generation: "s1.1.1".into(),
            harness: "claude".into(),
            model: None,
            effort: None,
            spawned_at: None,
            project: None,
        };
        let record = crate::dispatch::build(
            "tsk_1",
            "prj_1",
            &spawn,
            quark_systems::DispatchTrigger::Spawn,
            crate::dispatch::Resolved::NotRun("test".into()),
        );
        let stored = store.record_dispatch(&record, "s1.1.1").unwrap().unwrap();
        assert_eq!(stored.chosen.account.as_deref(), Some("acc_2"));
        assert!(store.record_dispatch(&record, "s1.1.1").unwrap().is_none());
        assert_eq!(store.list_dispatch("tsk_1").unwrap(), vec![stored]);
    }
}
