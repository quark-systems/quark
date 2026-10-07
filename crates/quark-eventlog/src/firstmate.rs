//! The firstmate ingest bridge: today's bash engine, mirrored into the log.
//!
//! While slice 1 runs beside bash, firstmate still writes its own files. The
//! bridge reads one firstmate home and appends what it finds:
//!
//! - each new line of `state/<task>.status` as a `firstmate.status` event
//!   ([`StatusPayload`]);
//! - each new worker generation in `state/<task>.meta` as a
//!   `firstmate.spawn` event ([`quark_engine::meta::SpawnMeta`]);
//! - the set of tasks that have a `state/<task>.meta`, whenever it changes,
//!   as a `firstmate.tasks` event ([`TasksPayload`]). Firstmate removes a
//!   task's record at cleanup, so this is how the log learns a task left.
//!
//! Status lines are wake-event history, not current task state, so the
//! bridge records them as they are and leaves turning them into
//! `task.transition` events to the native read path. Read positions are
//! checkpoints in the log, written in the same transaction as the events
//! they cover, so every line lands exactly once however the daemon dies.
//! A status file that is replaced or truncated is read again from the
//! start, the same trade the engine makes.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use quark_core::{CoreError, HostId, NewEvent, ProjectId, Result, TaskId};
use quark_engine::meta;
use quark_engine::status::{DecisionKey, StatusTail};
use serde::{Deserialize, Serialize};

use crate::SqliteEventLog;

/// Kinds the bridge owns (prefix `firstmate`).
pub mod kinds {
    /// One status line. Payload: [`super::StatusPayload`].
    pub const STATUS: &str = "firstmate.status";
    /// A worker was spawned or relaunched. Payload:
    /// [`quark_engine::meta::SpawnMeta`].
    pub const SPAWN: &str = "firstmate.spawn";
    /// The tasks with a metadata record changed. Payload:
    /// [`super::TasksPayload`].
    pub const TASKS: &str = "firstmate.tasks";
}

/// Every task id with a `state/<task>.meta`, sorted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TasksPayload {
    pub live: Vec<String>,
}

/// One `state/<task>.status` line as the engine wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusPayload {
    /// Leading verb: `working`, `needs-decision`, `blocked`, `paused`,
    /// `resolved`, `done`, `failed`, ...
    pub verb: String,
    /// The decision key the engine folds the line under; `None` when the
    /// engine skips it (an invalid slug).
    pub key: Option<String>,
    pub corr: Option<String>,
    pub note: String,
    /// The line exactly as written.
    pub raw: String,
    /// Byte offset of the line in the file.
    pub offset: u64,
}

/// What one [`FirstmateBridge::ingest`] pass appended.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IngestReport {
    pub status_lines: usize,
    pub spawns: usize,
    /// Whether the set of tasks changed.
    pub tasks_changed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct StatusCursor {
    dev: u64,
    ino: u64,
    offset: u64,
}

/// Mirrors firstmate homes into an event log.
///
/// Clones share one lock, so passes never interleave: each reads a
/// checkpoint and then writes it, and two at once would append twice.
#[derive(Clone)]
pub struct FirstmateBridge {
    log: SqliteEventLog,
    host: HostId,
    pass: std::sync::Arc<tokio::sync::Mutex<()>>,
}

fn io(path: &Path, e: impl std::fmt::Display) -> CoreError {
    CoreError::Backend(format!("{}: {e}", path.display()))
}

impl FirstmateBridge {
    pub fn new(log: SqliteEventLog, host: HostId) -> Self {
        Self {
            log,
            host,
            pass: Default::default(),
        }
    }

    /// Append everything new in the firstmate home `home`, which belongs to
    /// `project`. Safe to run again at any time; nothing lands twice.
    pub async fn ingest(&self, project: &ProjectId, home: &Path) -> Result<IngestReport> {
        let _pass = self.pass.lock().await;
        let state = home.join("state");
        let mut report = IngestReport::default();
        let mut live = Vec::new();
        for (task, kind, path) in task_files(&state)? {
            match kind {
                FileKind::Status => {
                    report.status_lines += self.ingest_status(project, &task, &path).await?
                }
                FileKind::Meta => {
                    report.spawns += self.ingest_meta(project, &task, &path).await?;
                    live.push(task.to_string());
                }
            }
        }
        report.tasks_changed = self.ingest_tasks(project, live).await?;
        Ok(report)
    }

    async fn ingest_tasks(&self, project: &ProjectId, live: Vec<String>) -> Result<bool> {
        let name = format!("firstmate/{project}/tasks");
        let payload = TasksPayload { live };
        let value =
            serde_json::to_string(&payload).map_err(|e| CoreError::Backend(e.to_string()))?;
        let saved = self.log.checkpoint(&name).await?;
        // A home that never had a task needs no event.
        if saved.as_deref() == Some(value.as_str()) || (saved.is_none() && payload.live.is_empty())
        {
            return Ok(false);
        }
        let event = NewEvent::typed(
            self.host.clone(),
            project.clone(),
            None,
            kinds::TASKS,
            &payload,
        )?;
        self.log
            .append_batch(vec![event], Some((name, value)))
            .await?;
        Ok(true)
    }

    async fn ingest_status(
        &self,
        project: &ProjectId,
        task: &TaskId,
        path: &Path,
    ) -> Result<usize> {
        let name = format!("firstmate/{project}/{task}/status");
        let saved: Option<StatusCursor> = self
            .log
            .checkpoint(&name)
            .await?
            .and_then(|v| serde_json::from_str(&v).ok());
        let path_buf = path.to_path_buf();
        let read = tokio::task::spawn_blocking(move || -> Result<Option<_>> {
            let meta = match std::fs::metadata(&path_buf) {
                Ok(m) => m,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(io(&path_buf, e)),
            };
            let (dev, ino) = (meta.dev(), meta.ino());
            let offset = saved
                .filter(|c| c.dev == dev && c.ino == ino)
                .map_or(0, |c| c.offset);
            let mut tail = StatusTail::resume(&path_buf, offset);
            let lines = tail.read_new().map_err(|e| io(&path_buf, e))?;
            let cursor = StatusCursor {
                dev,
                ino,
                offset: tail.offset(),
            };
            Ok(Some((lines, cursor)))
        })
        .await
        .map_err(|e| CoreError::Backend(e.to_string()))??;
        let Some((lines, cursor)) = read else {
            return Ok(0);
        };
        if saved == Some(cursor) {
            return Ok(0);
        }
        let mut events = Vec::with_capacity(lines.len());
        for line in &lines {
            let e = &line.event;
            let payload = StatusPayload {
                verb: e.verb.clone(),
                key: match &e.key {
                    DecisionKey::Invalid(_) => None,
                    k => k.fold_key().map(str::to_string),
                },
                corr: e.corr.clone(),
                note: e.note.clone(),
                raw: e.raw.clone(),
                offset: line.offset,
            };
            events.push(NewEvent::typed(
                self.host.clone(),
                project.clone(),
                Some(task.clone()),
                kinds::STATUS,
                &payload,
            )?);
        }
        let value =
            serde_json::to_string(&cursor).map_err(|e| CoreError::Backend(e.to_string()))?;
        self.log.append_batch(events, Some((name, value))).await?;
        Ok(lines.len())
    }

    async fn ingest_meta(&self, project: &ProjectId, task: &TaskId, path: &Path) -> Result<usize> {
        let name = format!("firstmate/{project}/{task}/spawn");
        let path_buf = path.to_path_buf();
        let spawn = tokio::task::spawn_blocking(move || meta::read(&path_buf))
            .await
            .map_err(|e| CoreError::Backend(e.to_string()))?
            .map_err(|e| io(path, e))?;
        let Some(spawn) = spawn else {
            return Ok(0);
        };
        if self.log.checkpoint(&name).await?.as_deref() == Some(spawn.generation.as_str()) {
            return Ok(0);
        }
        let event = NewEvent::typed(
            self.host.clone(),
            project.clone(),
            Some(task.clone()),
            kinds::SPAWN,
            &spawn,
        )?;
        self.log
            .append_batch(vec![event], Some((name, spawn.generation.clone())))
            .await?;
        Ok(1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum FileKind {
    // Spawn before status for the same task, so a new worker's generation
    // precedes its first line.
    Meta,
    Status,
}

/// `state/<task>.status` and `state/<task>.meta`, sorted so each pass reads
/// in the same order. Dotfiles and ids the engine would refuse are skipped.
fn task_files(state: &Path) -> Result<Vec<(TaskId, FileKind, PathBuf)>> {
    let entries = match std::fs::read_dir(state) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io(state, e)),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| io(state, e))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let (task, kind) = if let Some(t) = name.strip_suffix(".status") {
            (t, FileKind::Status)
        } else if let Some(t) = name.strip_suffix(".meta") {
            (t, FileKind::Meta)
        } else {
            continue;
        };
        if quark_engine::validate_task_id(task).is_err() {
            continue;
        }
        out.push((TaskId::from(task), kind, entry.path()));
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use quark_core::{EventLog, Seq};

    use super::*;

    fn append(path: &Path, text: &str) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    fn statuses(events: &[quark_core::Event]) -> Vec<StatusPayload> {
        events
            .iter()
            .filter(|e| e.kind.as_str() == kinds::STATUS)
            .map(|e| e.decode().unwrap())
            .collect()
    }

    #[tokio::test]
    async fn mirrors_status_lines_and_spawns_once() {
        let home = tempfile::tempdir().unwrap();
        let state = home.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(
            state.join("t1.meta"),
            "spawn_gen=s1790000000.1.1\nharness=claude\nkind=ship\n",
        )
        .unwrap();
        append(
            &state.join("t1.status"),
            "working: started\nneeds-decision [key=api]: pick",
        );
        std::fs::write(state.join(".hash-t1.status"), "x\n").unwrap();

        let db = tempfile::tempdir().unwrap();
        let log = SqliteEventLog::open(db.path().join("events.db")).unwrap();
        let bridge = FirstmateBridge::new(log.clone(), HostId::from("h"));
        let p = ProjectId::from("p");

        // The partial second line waits for its newline.
        let r = bridge.ingest(&p, home.path()).await.unwrap();
        assert_eq!(
            r,
            IngestReport {
                status_lines: 1,
                spawns: 1,
                tasks_changed: true,
            }
        );
        let all = log.read(Seq::ZERO, 100).await.unwrap();
        assert_eq!(all[0].kind.as_str(), kinds::SPAWN);
        assert_eq!(all[1].task, Some(TaskId::from("t1")));

        append(
            &state.join("t1.status"),
            "\nresolved [key=api]: went with a\n",
        );
        let r = bridge.ingest(&p, home.path()).await.unwrap();
        assert_eq!(r.status_lines, 2);
        assert_eq!(r.spawns, 0);
        assert_eq!(
            bridge.ingest(&p, home.path()).await.unwrap(),
            IngestReport::default()
        );

        let lines = statuses(&log.read(Seq::ZERO, 100).await.unwrap());
        let verbs: Vec<_> = lines.iter().map(|l| l.verb.as_str()).collect();
        assert_eq!(verbs, ["working", "needs-decision", "resolved"]);
        assert_eq!(lines[1].key.as_deref(), Some("api"));
        assert_eq!(lines[0].key.as_deref(), Some("default"));
        assert_eq!(lines[1].offset, "working: started\n".len() as u64);

        // A relaunch is a new generation.
        std::fs::write(
            state.join("t1.meta"),
            "spawn_gen=s1790000100.1.1\nharness=codex\n",
        )
        .unwrap();
        assert_eq!(bridge.ingest(&p, home.path()).await.unwrap().spawns, 1);
    }

    #[tokio::test]
    async fn replaced_file_is_read_from_the_start() {
        let home = tempfile::tempdir().unwrap();
        let state = home.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let status = state.join("t.status");
        append(&status, "working: a\nworking: b\n");
        let db = tempfile::tempdir().unwrap();
        let log = SqliteEventLog::open(db.path().join("events.db")).unwrap();
        let bridge = FirstmateBridge::new(log.clone(), HostId::from("h"));
        let p = ProjectId::from("p");
        assert_eq!(
            bridge.ingest(&p, home.path()).await.unwrap().status_lines,
            2
        );

        let fresh = state.join("t.status.new");
        append(&fresh, "done: c\n");
        std::fs::rename(&fresh, &status).unwrap();
        assert_eq!(
            bridge.ingest(&p, home.path()).await.unwrap().status_lines,
            1
        );
        let lines = statuses(&log.read(Seq::ZERO, 100).await.unwrap());
        assert_eq!(lines.last().unwrap().raw, "done: c");
    }

    #[tokio::test]
    async fn records_when_a_task_leaves() {
        let home = tempfile::tempdir().unwrap();
        let state = home.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let db = tempfile::tempdir().unwrap();
        let log = SqliteEventLog::open(db.path().join("events.db")).unwrap();
        let bridge = FirstmateBridge::new(log.clone(), HostId::from("h"));
        let p = ProjectId::from("p");
        // No tasks yet: nothing to say.
        assert!(!bridge.ingest(&p, home.path()).await.unwrap().tasks_changed);

        std::fs::write(state.join("a.meta"), "kind=ship\n").unwrap();
        std::fs::write(state.join("b.meta"), "spawn_gen=s1.1.1\nharness=x\n").unwrap();
        assert!(bridge.ingest(&p, home.path()).await.unwrap().tasks_changed);
        assert!(!bridge.ingest(&p, home.path()).await.unwrap().tasks_changed);
        std::fs::remove_file(state.join("a.meta")).unwrap();
        assert!(bridge.ingest(&p, home.path()).await.unwrap().tasks_changed);

        let sets: Vec<TasksPayload> = log
            .read(Seq::ZERO, 100)
            .await
            .unwrap()
            .iter()
            .filter(|e| e.kind.as_str() == kinds::TASKS)
            .map(|e| e.decode().unwrap())
            .collect();
        assert_eq!(sets[0].live, ["a", "b"]);
        assert_eq!(sets[1].live, ["b"]);
    }

    #[tokio::test]
    async fn missing_home_is_empty() {
        let db = tempfile::tempdir().unwrap();
        let log = SqliteEventLog::open(db.path().join("events.db")).unwrap();
        let bridge = FirstmateBridge::new(log, HostId::from("h"));
        let r = bridge
            .ingest(&ProjectId::from("p"), Path::new("/nonexistent/fm-home"))
            .await
            .unwrap();
        assert_eq!(r, IngestReport::default());
    }
}
