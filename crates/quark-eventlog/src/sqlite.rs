//! [`SqliteEventLog`]: the [`EventLog`] in its own SQLite file, `events.db`.
//!
//! Every append runs in one `BEGIN IMMEDIATE` transaction in WAL mode with
//! `synchronous=FULL`, so an event is on disk before `append` returns and a
//! crash at any point leaves either the whole transaction or none of it.
//! `seq` is assigned inside that transaction as the current head plus one,
//! so it never has gaps. A known `id` returns its original `seq`.
//!
//! Next to the events, a `checkpoints` table holds named cursors that are
//! written in the same transaction as the events they cover
//! ([`SqliteEventLog::append_batch`]). An ingester that stores its read
//! position this way delivers each source record exactly once across
//! crashes.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use quark_core::{
    CoreError, Event, EventId, EventKind, EventLog, HostId, NewEvent, ProjectId, Result, Seq,
    Subscription, TaskId,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tokio::sync::Notify;
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS events (
    seq     INTEGER PRIMARY KEY,
    id      TEXT NOT NULL UNIQUE,
    ts      TEXT NOT NULL,
    host    TEXT NOT NULL,
    project TEXT NOT NULL,
    task    TEXT,
    kind    TEXT NOT NULL,
    payload TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS events_task ON events(project, task, seq);
CREATE INDEX IF NOT EXISTS events_kind ON events(kind, seq);
CREATE TABLE IF NOT EXISTS checkpoints (
    name  TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

/// How often a subscription re-reads the file when nothing in this process
/// appended, so it also follows writers in other processes.
const POLL: Duration = Duration::from_millis(250);
const SUBSCRIBE_BATCH: usize = 256;

struct Shared {
    conn: Mutex<Connection>,
    appended: Notify,
    path: PathBuf,
}

/// The durable [`EventLog`]. Cheap to clone; clones share one connection.
#[derive(Clone)]
pub struct SqliteEventLog {
    shared: Arc<Shared>,
}

fn backend(e: impl std::fmt::Display) -> CoreError {
    CoreError::Backend(format!("event log: {e}"))
}

impl SqliteEventLog {
    /// Open (or create) the log at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(backend)?;
        }
        let conn = Connection::open(&path).map_err(backend)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(backend)?;
        conn.pragma_update(None, "synchronous", "FULL")
            .map_err(backend)?;
        conn.pragma_update(None, "busy_timeout", 5000)
            .map_err(backend)?;
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(backend)?;
        if version > SCHEMA_VERSION {
            return Err(CoreError::Unsupported(format!(
                "{} has event log schema {version}; this build knows {SCHEMA_VERSION}",
                path.display()
            )));
        }
        conn.execute_batch(SCHEMA).map_err(backend)?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(backend)?;
        Ok(Self {
            shared: Arc::new(Shared {
                conn: Mutex::new(conn),
                appended: Notify::new(),
                path,
            }),
        })
    }

    pub fn path(&self) -> &Path {
        &self.shared.path
    }

    /// Run `f` on the connection off the async runtime.
    async fn with_conn<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let shared = self.shared.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = shared.conn.lock().unwrap_or_else(|p| p.into_inner());
            f(&mut conn)
        })
        .await
        .map_err(backend)?
    }

    /// Append `events` in order and, in the same transaction, set the
    /// checkpoint `name` to `value`. Either all of it is stored or none of
    /// it. Returns each event's `seq` (the original one for a known id).
    pub async fn append_batch(
        &self,
        events: Vec<NewEvent>,
        checkpoint: Option<(String, String)>,
    ) -> Result<Vec<Seq>> {
        let seqs = self
            .with_conn(move |conn| {
                let tx = conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(backend)?;
                let mut head: u64 = tx
                    .query_row("SELECT COALESCE(MAX(seq), 0) FROM events", [], |r| {
                        r.get::<_, i64>(0)
                    })
                    .map_err(backend)? as u64;
                let mut seqs = Vec::with_capacity(events.len());
                for e in &events {
                    let id = e.id.0.to_string();
                    let known: Option<i64> = tx
                        .query_row("SELECT seq FROM events WHERE id = ?1", [&id], |r| r.get(0))
                        .optional()
                        .map_err(backend)?;
                    if let Some(seq) = known {
                        seqs.push(Seq(seq as u64));
                        continue;
                    }
                    head += 1;
                    let ts = e.ts.format(&Rfc3339).map_err(backend)?;
                    let payload = serde_json::to_string(&e.payload).map_err(backend)?;
                    tx.execute(
                        "INSERT INTO events (seq, id, ts, host, project, task, kind, payload)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![
                            head as i64,
                            id,
                            ts,
                            e.host.as_str(),
                            e.project.as_str(),
                            e.task.as_ref().map(|t| t.as_str()),
                            e.kind.as_str(),
                            payload
                        ],
                    )
                    .map_err(backend)?;
                    seqs.push(Seq(head));
                }
                if let Some((name, value)) = checkpoint {
                    tx.execute(
                        "INSERT INTO checkpoints (name, value) VALUES (?1, ?2)
                         ON CONFLICT(name) DO UPDATE SET value = excluded.value",
                        params![name, value],
                    )
                    .map_err(backend)?;
                }
                tx.commit().map_err(backend)?;
                Ok(seqs)
            })
            .await?;
        self.shared.appended.notify_waiters();
        Ok(seqs)
    }

    /// The value of checkpoint `name`, if one was stored.
    pub async fn checkpoint(&self, name: &str) -> Result<Option<String>> {
        let name = name.to_string();
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT value FROM checkpoints WHERE name = ?1",
                [name],
                |r| r.get(0),
            )
            .optional()
            .map_err(backend)
        })
        .await
    }

    /// Check the file: SQLite's own integrity check, then that `seq` runs
    /// 1..=head with no gaps. Returns the head.
    pub async fn verify(&self) -> Result<Seq> {
        self.with_conn(|conn| {
            let ok: String = conn
                .query_row("PRAGMA integrity_check", [], |r| r.get(0))
                .map_err(backend)?;
            if ok != "ok" {
                return Err(CoreError::Backend(format!("integrity check: {ok}")));
            }
            let (count, min, max): (i64, i64, i64) = conn
                .query_row(
                    "SELECT COUNT(*), COALESCE(MIN(seq), 1), COALESCE(MAX(seq), 0) FROM events",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .map_err(backend)?;
            if min != 1 || count != max {
                return Err(CoreError::Backend(format!(
                    "seq gap: {count} events spanning {min}..={max}"
                )));
            }
            Ok(Seq(max as u64))
        })
        .await
    }

    fn read_blocking(conn: &Connection, after: Seq, limit: usize) -> Result<Vec<Event>> {
        let mut stmt = conn
            .prepare_cached(
                "SELECT seq, id, ts, host, project, task, kind, payload
                 FROM events WHERE seq > ?1 ORDER BY seq LIMIT ?2",
            )
            .map_err(backend)?;
        let rows = stmt
            .query_map(
                params![after.0 as i64, limit.min(i64::MAX as usize) as i64],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, String>(7)?,
                    ))
                },
            )
            .map_err(backend)?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, id, ts, host, project, task, kind, payload) = row.map_err(backend)?;
            out.push(Event {
                id: EventId(Uuid::parse_str(&id).map_err(backend)?),
                seq: Seq(seq as u64),
                ts: OffsetDateTime::parse(&ts, &Rfc3339).map_err(backend)?,
                host: HostId(host),
                project: ProjectId(project),
                task: task.map(TaskId),
                kind: EventKind(kind),
                payload: serde_json::from_str(&payload).map_err(backend)?,
            });
        }
        Ok(out)
    }
}

#[async_trait]
impl EventLog for SqliteEventLog {
    async fn append(&self, event: NewEvent) -> Result<Seq> {
        let seqs = self.append_batch(vec![event], None).await?;
        Ok(seqs[0])
    }

    async fn read(&self, after: Seq, limit: usize) -> Result<Vec<Event>> {
        self.with_conn(move |conn| Self::read_blocking(conn, after, limit))
            .await
    }

    async fn head(&self) -> Result<Seq> {
        self.with_conn(|conn| {
            conn.query_row("SELECT COALESCE(MAX(seq), 0) FROM events", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|s| Seq(s as u64))
            .map_err(backend)
        })
        .await
    }

    async fn subscribe(&self, after: Seq) -> Result<Box<dyn Subscription>> {
        Ok(Box::new(SqliteSubscription {
            log: self.clone(),
            at: after,
            buffered: VecDeque::new(),
        }))
    }
}

struct SqliteSubscription {
    log: SqliteEventLog,
    at: Seq,
    buffered: VecDeque<Event>,
}

#[async_trait]
impl Subscription for SqliteSubscription {
    async fn next(&mut self) -> Option<Result<Event>> {
        loop {
            if let Some(e) = self.buffered.pop_front() {
                self.at = e.seq;
                return Some(Ok(e));
            }
            // Register for the wakeup before reading, so an append in
            // between is not missed.
            let notified = self.log.shared.appended.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            match self.log.read(self.at, SUBSCRIBE_BATCH).await {
                Ok(events) if !events.is_empty() => self.buffered.extend(events),
                Ok(_) => {
                    tokio::select! {
                        _ = notified => {}
                        _ = tokio::time::sleep(POLL) => {}
                    }
                }
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: &str) -> NewEvent {
        NewEvent::new(
            HostId::from("h"),
            ProjectId::from("p"),
            Some(TaskId::from("t")),
            kind,
            serde_json::json!({ "n": 1 }),
        )
    }

    #[tokio::test]
    async fn append_read_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let log = SqliteEventLog::open(dir.path().join("events.db")).unwrap();
        let a = ev("a.x");
        assert_eq!(log.append(a.clone()).await.unwrap(), Seq(1));
        assert_eq!(log.append(ev("b.x")).await.unwrap(), Seq(2));
        // Idempotent by id.
        assert_eq!(log.append(a.clone()).await.unwrap(), Seq(1));
        assert_eq!(log.head().await.unwrap(), Seq(2));
        let all = log.read(Seq::ZERO, 10).await.unwrap();
        assert_eq!(all[0], a.with_seq(Seq(1)));
        assert_eq!(log.read(Seq(1), 10).await.unwrap().len(), 1);
        assert_eq!(log.read(Seq::ZERO, 1).await.unwrap().len(), 1);
        assert_eq!(log.verify().await.unwrap(), Seq(2));
    }

    #[tokio::test]
    async fn survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.db");
        let a = ev("a.x");
        {
            let log = SqliteEventLog::open(&path).unwrap();
            log.append(a.clone()).await.unwrap();
        }
        let log = SqliteEventLog::open(&path).unwrap();
        assert_eq!(log.append(a).await.unwrap(), Seq(1));
        assert_eq!(log.append(ev("b.x")).await.unwrap(), Seq(2));
    }

    #[tokio::test]
    async fn batch_and_checkpoint_commit_together() {
        let dir = tempfile::tempdir().unwrap();
        let log = SqliteEventLog::open(dir.path().join("events.db")).unwrap();
        let known = ev("a.x");
        log.append(known.clone()).await.unwrap();
        let seqs = log
            .append_batch(
                vec![ev("b.x"), known, ev("c.x")],
                Some(("src".into(), "42".into())),
            )
            .await
            .unwrap();
        assert_eq!(seqs, vec![Seq(2), Seq(1), Seq(3)]);
        assert_eq!(log.checkpoint("src").await.unwrap().as_deref(), Some("42"));
        assert_eq!(log.checkpoint("other").await.unwrap(), None);
        assert_eq!(log.verify().await.unwrap(), Seq(3));
    }

    #[tokio::test]
    async fn subscription_catches_up_then_follows() {
        let dir = tempfile::tempdir().unwrap();
        let log = SqliteEventLog::open(dir.path().join("events.db")).unwrap();
        log.append(ev("a.x")).await.unwrap();
        let mut sub = log.subscribe(Seq::ZERO).await.unwrap();
        assert_eq!(sub.next().await.unwrap().unwrap().seq, Seq(1));
        let writer = log.clone();
        let task = tokio::spawn(async move { sub.next().await.unwrap().unwrap().seq });
        tokio::task::yield_now().await;
        writer.append(ev("b.x")).await.unwrap();
        assert_eq!(task.await.unwrap(), Seq(2));
    }

    #[tokio::test]
    async fn subscription_follows_another_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.db");
        let reader = SqliteEventLog::open(&path).unwrap();
        let mut sub = reader.subscribe(Seq::ZERO).await.unwrap();
        // A second handle stands in for another process: its appends do
        // not wake `reader`'s subscription, so the poll has to find them.
        let other = SqliteEventLog::open(&path).unwrap();
        other.append(ev("a.x")).await.unwrap();
        let got = tokio::time::timeout(Duration::from_secs(5), sub.next())
            .await
            .unwrap();
        assert_eq!(got.unwrap().unwrap().seq, Seq(1));
    }

    #[tokio::test]
    async fn refuses_a_newer_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", 99).unwrap();
        }
        assert!(matches!(
            SqliteEventLog::open(&path),
            Err(CoreError::Unsupported(_))
        ));
    }
}
