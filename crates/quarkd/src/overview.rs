//! The Project dashboard's Overview, read from the native event log.
//!
//! [`OverviewModel`] folds `events.db` into each Project's live task status
//! and a compact history, catching up from where it stopped on every read,
//! so a request only reads what was appended since the last one. The
//! history answers "since you last looked": everything after a `seq` the
//! app saved on its previous visit.
//!
//! A task's state comes from its `task.transition` events, its harness and
//! model from `supervisor.launched`, and its latest note from the worker's
//! own messages (see `docs/engine/event-log.md`); other kinds count toward
//! a digest's `events` and are otherwise skipped.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use quark_core::event::kinds;
use quark_core::worker::{WorkerEnvelope, WorkerMessage};
use quark_core::TaskEvent;
use quark_core::{Event, EventLog, Result, Seq};
use quark_eventlog::SqliteEventLog;
use quark_supervisor::{Cause, SupervisorEvent};
use quark_systems::{
    DigestItem, DigestKind, LiveStatus, OverviewDigest, ProjectOverview, PulseState, StatusCounts,
    TaskPulse,
};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

/// Finished tasks stay in the live list (and its counts) this long.
pub const FINISHED_FOR: Duration = Duration::days(1);
/// Most highlights one digest lists.
pub const HIGHLIGHTS: usize = 50;
const READ_BATCH: usize = 1000;
const NOTE_MAX: usize = 500;
const LAUNCHED: &str = "supervisor.launched";

/// Every Project's fold of one event log.
pub struct OverviewModel {
    log: SqliteEventLog,
    after: Seq,
    projects: HashMap<String, ProjectFold>,
}

#[derive(Default)]
struct ProjectFold {
    head: u64,
    last: Option<OffsetDateTime>,
    tasks: BTreeMap<String, TaskFold>,
    history: Vec<Entry>,
}

struct TaskFold {
    verb: String,
    note: String,
    at: OffsetDateTime,
    harness: Option<String>,
    model: Option<String>,
    open: BTreeSet<String>,
    pull_request: Option<String>,
}

struct Entry {
    seq: u64,
    at: OffsetDateTime,
    task: Option<String>,
    what: What,
}

enum What {
    Spawn {
        text: String,
    },
    Status {
        verb: String,
        note: String,
        url: Option<String>,
        /// The key this line newly opened or closed.
        opened: bool,
        closed: bool,
    },
    Other,
}

impl OverviewModel {
    pub fn new(log: SqliteEventLog) -> Self {
        Self {
            log,
            after: Seq::ZERO,
            projects: HashMap::new(),
        }
    }

    /// Fold everything appended since the last call.
    pub async fn catch_up(&mut self) -> Result<()> {
        loop {
            let events = self.log.read(self.after, READ_BATCH).await?;
            let Some(last) = events.last() else {
                return Ok(());
            };
            self.after = last.seq;
            for e in &events {
                self.apply(e);
            }
        }
    }

    fn apply(&mut self, e: &Event) {
        let p = self
            .projects
            .entry(e.project.as_str().to_string())
            .or_default();
        p.head = e.seq.0;
        p.last = Some(e.ts);
        let task = e.task.as_ref().map(|t| t.as_str().to_string());
        let what = match (e.kind.as_str(), &task) {
            (LAUNCHED, Some(t)) => match e.decode::<SupervisorEvent>() {
                Ok(SupervisorEvent::Launched {
                    harness,
                    model,
                    cause,
                    ..
                }) => {
                    let text = match &model {
                        Some(model) => format!("{harness} · {model}"),
                        None => harness.clone(),
                    };
                    let f = p
                        .tasks
                        .entry(t.clone())
                        .or_insert_with(|| TaskFold::new(e.ts));
                    f.harness = Some(harness);
                    f.model = model;
                    if cause == Cause::Spawn {
                        What::Spawn { text }
                    } else {
                        What::Other
                    }
                }
                _ => What::Other,
            },
            (kinds::TASK, Some(t)) => match e.decode::<TaskEvent>() {
                Ok(ev) => {
                    let f = p
                        .tasks
                        .entry(t.clone())
                        .or_insert_with(|| TaskFold::new(e.ts));
                    f.transition(&ev, e.ts)
                }
                Err(_) => What::Other,
            },
            (kinds::WORKER, Some(t)) => {
                if let (Ok(w), Some(f)) = (e.decode::<WorkerEnvelope>(), p.tasks.get_mut(t)) {
                    f.message(&w.message);
                }
                What::Other
            }
            _ => What::Other,
        };
        p.history.push(Entry {
            seq: e.seq.0,
            at: e.ts,
            task,
            what,
        });
    }

    /// `project`'s live status at `now` and, with `since`, its digest.
    pub fn overview(
        &self,
        project: &str,
        since: Option<u64>,
        now: OffsetDateTime,
    ) -> ProjectOverview {
        let Some(p) = self.projects.get(project) else {
            return ProjectOverview {
                project_id: project.to_string(),
                head: 0,
                live: LiveStatus::default(),
                digest: since.map(|since| OverviewDigest {
                    since,
                    ..Default::default()
                }),
                error: None,
            };
        };
        ProjectOverview {
            project_id: project.to_string(),
            head: p.head,
            live: p.live(now),
            digest: since.map(|s| p.digest(s)),
            error: None,
        }
    }

    /// `project`'s digest of everything logged after `at`.
    pub fn digest_after(&self, project: &str, at: OffsetDateTime) -> OverviewDigest {
        let Some(p) = self.projects.get(project) else {
            return OverviewDigest::default();
        };
        let before = p.history.partition_point(|e| e.at <= at);
        let since = before.checked_sub(1).map_or(0, |i| p.history[i].seq);
        p.digest(since)
    }
}

impl TaskFold {
    fn new(at: OffsetDateTime) -> Self {
        Self {
            verb: String::new(),
            note: String::new(),
            at,
            harness: None,
            model: None,
            open: BTreeSet::new(),
            pull_request: None,
        }
    }

    /// A worker's note, shown until the next one; the transition it leads
    /// to (if any) follows it in the log.
    fn message(&mut self, m: &WorkerMessage) {
        let note = match m {
            WorkerMessage::Report { note, .. } => note,
            WorkerMessage::Ask { question, .. } => question,
            WorkerMessage::Done { summary, .. } => summary,
            WorkerMessage::Learned { .. } | WorkerMessage::Signal { .. } => return,
        };
        if !note.is_empty() {
            self.note = clip(note);
        }
    }

    fn transition(&mut self, ev: &TaskEvent, at: OffsetDateTime) -> What {
        let (mut opened, mut closed) = (false, false);
        let mut url = None;
        let (verb, note): (&str, Option<&str>) = match ev {
            TaskEvent::Queued { title } => ("queued", Some(title)),
            TaskEvent::Started { .. } | TaskEvent::Resumed => ("working", None),
            TaskEvent::DecisionNeeded { key, question } => {
                opened = self.open.insert(key.clone());
                ("needs-decision", Some(question))
            }
            TaskEvent::DecisionAnswered { key } => {
                closed = self.open.remove(key);
                ("resolved", None)
            }
            TaskEvent::Blocked { reason } => ("blocked", Some(reason)),
            TaskEvent::Paused { reason } => ("paused", Some(reason)),
            TaskEvent::InReview { pull_request } => {
                url = pull_request.clone();
                if url.is_some() {
                    self.pull_request = url.clone();
                }
                ("done", None)
            }
            // A merge after review is the same piece of work finishing,
            // not a second `done`.
            TaskEvent::Completed if self.verb == "done" => ("completed", None),
            TaskEvent::Completed => ("done", None),
            TaskEvent::Failed { reason } => ("failed", Some(reason)),
            TaskEvent::Cancelled => ("failed", Some("cancelled")),
        };
        if let Some(note) = note.filter(|n| !n.is_empty()) {
            self.note = clip(note);
        }
        self.verb = verb.to_string();
        self.at = at;
        What::Status {
            verb: verb.to_string(),
            note: self.note.clone(),
            url,
            opened,
            closed,
        }
    }

    fn state(&self) -> PulseState {
        match self.verb.as_str() {
            "done" | "completed" => PulseState::Done,
            "failed" => PulseState::Failed,
            "blocked" => PulseState::Blocked,
            _ if !self.open.is_empty() => PulseState::NeedsDecision,
            "needs-decision" => PulseState::NeedsDecision,
            "paused" => PulseState::Paused,
            _ => PulseState::Working,
        }
    }
}

impl ProjectFold {
    fn live(&self, now: OffsetDateTime) -> LiveStatus {
        let mut counts = StatusCounts::default();
        let mut tasks: Vec<TaskPulse> = Vec::new();
        for (id, t) in &self.tasks {
            let state = t.state();
            if !state.is_open() && now - t.at > FINISHED_FOR {
                continue;
            }
            *match state {
                PulseState::Working => &mut counts.working,
                PulseState::NeedsDecision => &mut counts.needs_decision,
                PulseState::Blocked => &mut counts.blocked,
                PulseState::Paused => &mut counts.paused,
                PulseState::Done => &mut counts.done,
                PulseState::Failed => &mut counts.failed,
            } += 1;
            tasks.push(TaskPulse {
                engine_task: id.clone(),
                task_id: None,
                title: None,
                state,
                verb: t.verb.clone(),
                note: t.note.clone(),
                at: rfc3339(t.at),
                harness: t.harness.clone(),
                model: t.model.clone(),
                open_decisions: t.open.iter().cloned().collect(),
                pull_request: t.pull_request.clone(),
            });
        }
        // Open first, then newest first; RFC 3339 UTC strings sort by time.
        tasks.sort_by(|a, b| {
            b.state
                .is_open()
                .cmp(&a.state.is_open())
                .then_with(|| b.at.cmp(&a.at))
                .then_with(|| a.engine_task.cmp(&b.engine_task))
        });
        LiveStatus {
            counts,
            tasks,
            last_activity: self.last.map(rfc3339),
        }
    }

    fn digest(&self, since: u64) -> OverviewDigest {
        let start = self.history.partition_point(|e| e.seq <= since);
        let after = &self.history[start..];
        let mut d = OverviewDigest {
            since,
            from: after.first().map(|e| rfc3339(e.at)),
            to: after.last().map(|e| rfc3339(e.at)),
            events: after.len() as u32,
            ..Default::default()
        };
        for e in after.iter().rev() {
            let (kind, text, url) = match &e.what {
                What::Spawn { text } => {
                    d.spawned += 1;
                    (DigestKind::Spawned, text.clone(), None)
                }
                What::Status {
                    verb,
                    note,
                    url,
                    opened,
                    closed,
                } => {
                    let kind = match verb.as_str() {
                        "done" => {
                            d.done += 1;
                            d.pull_requests += u32::from(url.is_some());
                            DigestKind::Done
                        }
                        "failed" => {
                            d.failed += 1;
                            DigestKind::Failed
                        }
                        _ if *opened => {
                            d.decisions_opened += 1;
                            DigestKind::DecisionOpened
                        }
                        _ if *closed => {
                            d.decisions_resolved += 1;
                            DigestKind::DecisionResolved
                        }
                        _ => continue,
                    };
                    (kind, note.clone(), url.clone())
                }
                What::Other => continue,
            };
            if d.highlights.len() == HIGHLIGHTS {
                d.truncated = true;
                continue;
            }
            d.highlights.push(DigestItem {
                seq: e.seq,
                at: rfc3339(e.at),
                engine_task: e.task.clone(),
                task_id: None,
                title: None,
                kind,
                text,
                url,
            });
        }
        d
    }
}

fn clip(s: &str) -> String {
    match s.char_indices().nth(NOTE_MAX) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.to_offset(time::UtcOffset::UTC)
        .format(&Rfc3339)
        .unwrap_or_default()
}

/// The model for `log`, shared by every request in this process that reads
/// the same file, so each one only folds what is new. An in-memory log gets
/// a fresh model each time.
pub fn shared(log: &SqliteEventLog) -> Arc<tokio::sync::Mutex<OverviewModel>> {
    type Models = Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<OverviewModel>>>>;
    static MODELS: OnceLock<Models> = OnceLock::new();
    let fresh = || Arc::new(tokio::sync::Mutex::new(OverviewModel::new(log.clone())));
    let path = log.path();
    if path.as_os_str().is_empty() || path == Path::new(":memory:") {
        return fresh();
    }
    MODELS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .entry(path.to_path_buf())
        .or_insert_with(fresh)
        .clone()
}

#[cfg(test)]
mod tests {
    use quark_core::session::SessionId;
    use quark_core::{HostId, NewEvent, ProjectId, TaskId};

    use super::*;

    async fn add(log: &SqliteEventLog, project: &str, task: &str, ev: TaskEvent) -> u64 {
        let e = NewEvent::typed(
            HostId::from("local"),
            ProjectId::new(project),
            Some(TaskId::from(task)),
            kinds::TASK,
            &ev,
        )
        .unwrap();
        log.append(e).await.unwrap().0
    }

    async fn launch(log: &SqliteEventLog, project: &str, task: &str, cause: Cause) {
        let ev = SupervisorEvent::Launched {
            generation: "g1".into(),
            session: SessionId("s".into()),
            backend: "tmux".into(),
            cause,
            dir: PathBuf::from("/tmp"),
            harness: "claude".into(),
            model: Some("opus".into()),
            effort: None,
        };
        let e = NewEvent::typed(
            HostId::from("local"),
            ProjectId::new(project),
            Some(TaskId::from(task)),
            ev.kind().as_str(),
            &ev,
        )
        .unwrap();
        log.append(e).await.unwrap();
    }

    fn ask(key: &str, question: &str) -> TaskEvent {
        TaskEvent::DecisionNeeded {
            key: key.into(),
            question: question.into(),
        }
    }

    fn answer(key: &str) -> TaskEvent {
        TaskEvent::DecisionAnswered { key: key.into() }
    }

    fn started() -> TaskEvent {
        TaskEvent::Started {
            generation: "g1".into(),
        }
    }

    #[tokio::test]
    async fn digests_everything_logged_after_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let log = SqliteEventLog::open(dir.path().join("events.db")).unwrap();
        let before = OffsetDateTime::now_utc() - Duration::minutes(1);
        add(&log, "p1", "b", ask("api", "which shape?")).await;
        add(&log, "p1", "b", answer("api")).await;
        let mut m = OverviewModel::new(log.clone());
        m.catch_up().await.unwrap();
        let all = m.digest_after("p1", before);
        assert_eq!((all.decisions_opened, all.decisions_resolved), (1, 1));
        let after = OffsetDateTime::now_utc() + Duration::minutes(1);
        assert_eq!(m.digest_after("p1", after).highlights.len(), 0);
        assert_eq!(m.digest_after("nope", before).events, 0);
    }

    #[tokio::test]
    async fn folds_live_status_and_digest_since_a_seq() {
        let dir = tempfile::tempdir().unwrap();
        let log = SqliteEventLog::open(dir.path().join("events.db")).unwrap();
        launch(&log, "p1", "a", Cause::Spawn).await;
        add(&log, "p1", "a", started()).await;
        let seen = add(&log, "p1", "b", ask("api", "which shape?")).await;
        add(&log, "p2", "x", started()).await;
        add(&log, "p1", "b", answer("api")).await;
        add(
            &log,
            "p1",
            "a",
            TaskEvent::InReview {
                pull_request: Some("https://github.com/o/r/pull/7".into()),
            },
        )
        .await;
        add(&log, "p1", "c", ask("db", "sqlite or postgres?")).await;

        let mut m = OverviewModel::new(log.clone());
        m.catch_up().await.unwrap();
        let now = OffsetDateTime::now_utc();
        let o = m.overview("p1", Some(seen), now);
        assert_eq!(o.head, 7);
        let c = o.live.counts;
        assert_eq!((c.working, c.needs_decision, c.done), (1, 1, 1));
        let ids: Vec<_> = o
            .live
            .tasks
            .iter()
            .map(|t| t.engine_task.as_str())
            .collect();
        assert_eq!(ids, ["c", "b", "a"], "open tasks first, newest first");
        let a = &o.live.tasks[2];
        assert_eq!(a.harness.as_deref(), Some("claude"));
        assert_eq!(
            a.pull_request.as_deref(),
            Some("https://github.com/o/r/pull/7")
        );
        assert_eq!(o.live.tasks[0].open_decisions, ["db"]);
        assert_eq!(o.live.tasks[0].note, "sqlite or postgres?");

        let d = o.digest.unwrap();
        assert_eq!(d.events, 3, "only p1's events after `seen`");
        assert_eq!(
            (
                d.done,
                d.pull_requests,
                d.decisions_resolved,
                d.decisions_opened
            ),
            (1, 1, 1, 1)
        );
        let kinds: Vec<_> = d.highlights.iter().map(|h| h.kind).collect();
        assert_eq!(
            kinds,
            [
                DigestKind::DecisionOpened,
                DigestKind::Done,
                DigestKind::DecisionResolved
            ]
        );

        // Catching up again reads only what is new.
        add(&log, "p1", "c", answer("db")).await;
        m.catch_up().await.unwrap();
        let o = m.overview("p1", Some(o.head), now);
        assert_eq!(o.live.counts.needs_decision, 0);
        assert_eq!(o.digest.unwrap().decisions_resolved, 1);

        // Finished tasks drop out of the live list after a day.
        let later = now + FINISHED_FOR + Duration::minutes(1);
        let o = m.overview("p1", None, later);
        assert!(o.live.tasks.iter().all(|t| t.engine_task != "a"));
        assert!(o.digest.is_none());
    }

    #[tokio::test]
    async fn a_merge_after_review_is_not_a_second_done() {
        let dir = tempfile::tempdir().unwrap();
        let log = SqliteEventLog::open(dir.path().join("events.db")).unwrap();
        launch(&log, "p1", "a", Cause::Spawn).await;
        launch(&log, "p1", "a", Cause::Relaunch).await;
        add(
            &log,
            "p1",
            "a",
            TaskEvent::InReview {
                pull_request: Some("https://github.com/o/r/pull/1".into()),
            },
        )
        .await;
        add(&log, "p1", "a", TaskEvent::Completed).await;
        add(&log, "p1", "b", TaskEvent::Completed).await;
        let mut m = OverviewModel::new(log);
        m.catch_up().await.unwrap();
        let o = m.overview("p1", Some(0), OffsetDateTime::now_utc());
        assert_eq!(o.live.counts.done, 2);
        let d = o.digest.unwrap();
        assert_eq!((d.spawned, d.done, d.pull_requests), (1, 2, 1));
    }
}
