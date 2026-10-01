//! SQLite projection and append-only event log.
//!
//! The engine's files are the source of truth for orchestration state; this
//! database is a rebuildable read model. Every projection change and the
//! events it produces commit in one transaction, and events are published to
//! live subscribers only after that commit, in `seq` order.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use quark_engine::{FleetSnapshot, Hold};
use quark_systems::{
    CreateProject, Decision, DecisionState, Event, EventType, Project, Task, TaskKind, TaskState,
    UpdateProject,
};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use tokio::sync::broadcast;

use crate::now_rfc3339;

const SCHEMA_VERSION: i64 = 1;

const SCHEMA_V1: &str = r#"
CREATE TABLE projects (
    id             TEXT PRIMARY KEY,
    name           TEXT NOT NULL,
    goal           TEXT,
    workspace_path TEXT,
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL
);

CREATE TABLE tasks (
    id               TEXT PRIMARY KEY,
    project_id       TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    engine_id        TEXT NOT NULL,
    title            TEXT NOT NULL,
    kind             TEXT,
    state            TEXT NOT NULL,
    state_note       TEXT,
    harness          TEXT,
    pull_request_url TEXT,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    UNIQUE (project_id, engine_id)
);

CREATE TABLE decisions (
    id          TEXT PRIMARY KEY,
    project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    engine_id   TEXT NOT NULL,
    task_id     TEXT,
    question    TEXT NOT NULL,
    state       TEXT NOT NULL,
    answer      TEXT,
    answered_by TEXT,
    opened_at   TEXT NOT NULL,
    answered_at TEXT,
    UNIQUE (project_id, engine_id)
);

CREATE TABLE events (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT,
    type       TEXT NOT NULL,
    ts         TEXT NOT NULL,
    payload    TEXT NOT NULL
);

CREATE TABLE adapter_calls (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    ts          TEXT NOT NULL,
    project_id  TEXT,
    operation   TEXT NOT NULL,
    ok          INTEGER NOT NULL,
    duration_ms INTEGER NOT NULL,
    detail      TEXT
);
"#;

/// Capacity of the live event channel. A subscriber that falls further behind
/// than this catches up from the store instead.
const BUS_CAPACITY: usize = 1024;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("not found")]
    NotFound,
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, StoreError>;

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
        migrate(&mut conn)?;
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

    pub fn create_project(&self, input: CreateProject) -> Result<Project> {
        let name = input.name.trim().to_string();
        if name.is_empty() {
            return Err(StoreError::Invalid("name must not be empty".into()));
        }
        self.write(|tx, events| {
            let now = now_rfc3339();
            let project = Project {
                id: new_id("prj"),
                name,
                goal: input.goal,
                workspace_path: input.workspace_path,
                created_at: now.clone(),
                updated_at: now,
            };
            tx.execute(
                "INSERT INTO projects (id, name, goal, workspace_path, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    project.id,
                    project.name,
                    project.goal,
                    project.workspace_path,
                    project.created_at,
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
            project.updated_at = now_rfc3339();
            tx.execute(
                "UPDATE projects SET name = ?2, goal = ?3, workspace_path = ?4, updated_at = ?5
                 WHERE id = ?1",
                params![
                    project.id,
                    project.name,
                    project.goal,
                    project.workspace_path,
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

            for et in &snapshot.tasks {
                let now = now_rfc3339();
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
                            created_at: now.clone(),
                            updated_at: now,
                        };
                        tx.execute(
                            "INSERT INTO tasks (id, project_id, engine_id, title, kind, state,
                                 state_note, harness, pull_request_url, created_at, updated_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
                                task.updated_at
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
                        let changed = old.title != et.title
                            || old.kind != et.kind
                            || old.state != et.state
                            || old.state_note != et.state_note
                            || old.harness != et.harness
                            || old.pull_request_url != et.pull_request_url;
                        if !changed {
                            continue;
                        }
                        let task = Task {
                            title: et.title.clone(),
                            kind: et.kind,
                            state: et.state,
                            state_note: et.state_note.clone(),
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
            Ok(())
        })
    }

    // Decisions

    pub fn list_decisions(&self, state: Option<DecisionState>) -> Result<Vec<Decision>> {
        self.read(|c| {
            let sql = format!(
                "{DECISION_SELECT} WHERE (?1 IS NULL OR state = ?1) ORDER BY opened_at, id"
            );
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map([state.map(decision_state_str)], decision_from_row)?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })
    }

    /// Projects engine holds into decisions, emitting `decision.opened` for new
    /// holds and `decision.answered` when an open one gains an answer.
    pub fn apply_holds(&self, project_id: &str, holds: &[Hold]) -> Result<()> {
        self.write(|tx, events| {
            for hold in holds {
                let existing = tx
                    .query_row(
                        &format!("{DECISION_SELECT} WHERE project_id = ?1 AND engine_id = ?2"),
                        params![project_id, hold.id],
                        decision_from_row,
                    )
                    .optional()?;
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
                match existing {
                    None => {
                        let answered = hold.answer.is_some();
                        let decision = Decision {
                            id: new_id("dec"),
                            project_id: project_id.to_string(),
                            task_id,
                            question: hold.question.clone(),
                            state: if answered {
                                DecisionState::Answered
                            } else {
                                DecisionState::Open
                            },
                            answer: hold.answer.clone(),
                            answered_by: hold.answered_by.clone(),
                            opened_at: now.clone(),
                            answered_at: answered.then(|| now.clone()),
                        };
                        insert_decision(tx, &hold.id, &decision)?;
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
                    }
                    Some(old) if old.state == DecisionState::Open && hold.answer.is_some() => {
                        let decision = Decision {
                            state: DecisionState::Answered,
                            answer: hold.answer.clone(),
                            answered_by: hold.answered_by.clone(),
                            answered_at: Some(now),
                            ..old
                        };
                        tx.execute(
                            "UPDATE decisions SET state = ?2, answer = ?3, answered_by = ?4,
                                 answered_at = ?5 WHERE id = ?1",
                            params![
                                decision.id,
                                decision_state_str(decision.state),
                                decision.answer,
                                decision.answered_by,
                                decision.answered_at
                            ],
                        )?;
                        append_event(
                            tx,
                            events,
                            Some(project_id),
                            EventType::DecisionAnswered,
                            serde_json::to_value(&decision)?,
                        )?;
                    }
                    Some(_) => {}
                }
            }
            Ok(())
        })
    }

    // Adapter calls

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

fn migrate(conn: &mut Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(StoreError::Invalid(format!(
            "database schema {version} is newer than this quarkd ({SCHEMA_VERSION})"
        )));
    }
    if version < 1 {
        let tx = conn.transaction()?;
        tx.execute_batch(SCHEMA_V1)?;
        tx.pragma_update(None, "user_version", 1)?;
        tx.commit()?;
    }
    Ok(())
}

fn append_event(
    tx: &Transaction,
    events: &mut Vec<Event>,
    project_id: Option<&str>,
    event_type: EventType,
    payload: serde_json::Value,
) -> Result<()> {
    let ts = now_rfc3339();
    tx.execute(
        "INSERT INTO events (project_id, type, ts, payload) VALUES (?1, ?2, ?3, ?4)",
        params![project_id, event_type.as_str(), ts, payload.to_string()],
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

const PROJECT_SELECT: &str =
    "SELECT id, name, goal, workspace_path, created_at, updated_at FROM projects";

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
    Ok(Project {
        id: r.get(0)?,
        name: r.get(1)?,
        goal: r.get(2)?,
        workspace_path: r.get(3)?,
        created_at: r.get(4)?,
        updated_at: r.get(5)?,
    })
}

const TASK_COLUMNS: &str = "id, project_id, title, kind, state, state_note, harness, \
                            pull_request_url, created_at, updated_at";
const TASK_SELECT: &str = "SELECT id, project_id, title, kind, state, state_note, harness, \
                           pull_request_url, created_at, updated_at FROM tasks";

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
    })
}

const DECISION_SELECT: &str = "SELECT id, project_id, task_id, question, state, answer, \
                               answered_by, opened_at, answered_at FROM decisions";

fn decision_state_str(s: DecisionState) -> &'static str {
    match s {
        DecisionState::Open => "open",
        DecisionState::Answered => "answered",
    }
}

fn decision_from_row(r: &Row) -> rusqlite::Result<Decision> {
    Ok(Decision {
        id: r.get(0)?,
        project_id: r.get(1)?,
        task_id: r.get(2)?,
        question: r.get(3)?,
        state: match r.get::<_, String>(4)?.as_str() {
            "answered" => DecisionState::Answered,
            _ => DecisionState::Open,
        },
        answer: r.get(5)?,
        answered_by: r.get(6)?,
        opened_at: r.get(7)?,
        answered_at: r.get(8)?,
    })
}

fn insert_decision(tx: &Transaction, engine_id: &str, d: &Decision) -> Result<()> {
    tx.execute(
        "INSERT INTO decisions (id, project_id, engine_id, task_id, question, state, answer,
             answered_by, opened_at, answered_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            d.id,
            d.project_id,
            engine_id,
            d.task_id,
            d.question,
            decision_state_str(d.state),
            d.answer,
            d.answered_by,
            d.opened_at,
            d.answered_at
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use quark_engine::EngineTask;

    fn engine_task(id: &str, state: TaskState) -> EngineTask {
        EngineTask {
            id: id.into(),
            title: format!("task {id}"),
            kind: Some(TaskKind::Ship),
            state,
            state_note: None,
            harness: Some("claude".into()),
            pull_request_url: None,
        }
    }

    fn project(store: &Store) -> Project {
        store
            .create_project(CreateProject {
                name: "demo".into(),
                goal: None,
                workspace_path: Some("/tmp/ws".into()),
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
        };
        store
            .apply_holds(&p.id, std::slice::from_ref(&hold))
            .unwrap();
        let open = store.list_decisions(Some(DecisionState::Open)).unwrap();
        assert_eq!(open.len(), 1);
        assert!(open[0].task_id.is_some());

        hold.answer = Some("yes".into());
        hold.answered_by = Some("user_1".into());
        store
            .apply_holds(&p.id, std::slice::from_ref(&hold))
            .unwrap();
        store
            .apply_holds(&p.id, std::slice::from_ref(&hold))
            .unwrap();
        assert!(
            store
                .list_decisions(Some(DecisionState::Open))
                .unwrap()
                .is_empty()
        );
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
}
