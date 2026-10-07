//! Firstmate's fleet as the native read path sees it (slice 1).
//!
//! [`FirstmateFleet`] is a replay-safe read model over the events the
//! [`crate::firstmate`] bridge appends. For every task still holding a
//! firstmate metadata record it keeps:
//!
//! - the worker it runs (`firstmate.spawn`);
//! - its status lines, from the start of the current status file
//!   (`firstmate.status`), so a status tail can be served from the log;
//! - the open keyed decisions, folded with firstmate's rule
//!   (`status_open_decisions` in `bin/fm-classify-lib.sh`): `needs-decision`
//!   and `blocked` open a key, `resolved` and `captain-held` close it, and a
//!   ship or scout's `done` or `failed` clears them all;
//! - the current status declaration (`status_current_line`): the newest open
//!   decision, else the newest state-bearing line;
//! - a task state driven through the reference [`TaskMachine`]: each new
//!   declaration moves it toward the state firstmate's status log gives
//!   (`map_log_state` in `bin/fm-crew-state.sh`), one legal transition at a
//!   time, and a transition the machine refuses is counted, not forced.
//!
//! Firstmate's own current state also reads the worker's pane and its
//! validation run, which the log does not carry yet, so only the status-log
//! part is reproduced here.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use async_trait::async_trait;
use quark_core::task::ReferenceMachine;
use quark_core::{Event, ProjectId, ReadModel, Result, Seq, TaskEvent, TaskId, TaskMachine};
use quark_engine::meta::SpawnMeta;
use quark_systems::TaskState;
use serde::{Deserialize, Serialize};

use crate::firstmate::{kinds, StatusPayload, TasksPayload};

/// Key namespaces only their owner may open or close (firstmate's
/// `FM_CLASSIFY_RESERVED_KEY_PREFIXES_DEFAULT`).
const RESERVED_KEY_PREFIXES: [&str; 1] = ["pending-reply-"];

/// A keyed decision opened in the status log and not yet closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenDecision {
    pub key: String,
    /// `needs-decision` or `blocked`.
    pub verb: String,
    pub note: String,
}

/// The status declaration firstmate would read as the task's state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declaration {
    pub verb: String,
    pub note: String,
}

impl Declaration {
    /// Firstmate's state word for it (`map_log_state`): `None` for verbs
    /// that are not a state, such as `resolved` or `note`.
    pub fn state_word(&self) -> Option<&'static str> {
        Some(match self.verb.as_str() {
            "working" => "working",
            "needs-decision" => "parked",
            "blocked" => "blocked",
            "paused" => "paused",
            "done" => "done",
            "failed" => "failed",
            _ => return None,
        })
    }
}

/// One task as the log says it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetTask {
    pub project: ProjectId,
    pub task: TaskId,
    /// `ship`, `scout`, `secondmate`; `None` before the first spawn record.
    pub kind: Option<String>,
    pub harness: Option<String>,
    pub generation: Option<String>,
    /// The task machine's state.
    pub state: TaskState,
    /// What the status log says now, if it says anything.
    pub current: Option<Declaration>,
    /// Open decisions, oldest opened first.
    pub open: Vec<OpenDecision>,
    /// The pull request a `done` line named.
    pub pull_request: Option<String>,
    /// Transitions the machine refused.
    pub rejected: u32,
    /// Status lines of the current status file, oldest first.
    #[serde(skip)]
    pub lines: Vec<StatusPayload>,
    #[serde(skip)]
    last_event: Option<Declaration>,
}

impl FleetTask {
    fn new(project: ProjectId, task: TaskId) -> Self {
        Self {
            project,
            task,
            kind: None,
            harness: None,
            generation: None,
            state: TaskState::Unknown,
            current: None,
            open: Vec::new(),
            pull_request: None,
            rejected: 0,
            lines: Vec::new(),
            last_event: None,
        }
    }

    /// The kind firstmate folds with: a record without one is a ship.
    fn fold_kind(&self) -> &str {
        self.kind.as_deref().unwrap_or("ship")
    }

    fn apply(&mut self, event: TaskEvent) -> bool {
        match ReferenceMachine.apply(self.state, &event) {
            Ok(next) => {
                self.state = next;
                true
            }
            Err(_) => {
                self.rejected += 1;
                false
            }
        }
    }

    fn generation(&self) -> String {
        self.generation.clone().unwrap_or_default()
    }

    /// Move to `Running` by the shortest legal path.
    fn resume_running(&mut self) -> bool {
        let steps = match self.state {
            TaskState::Running => return true,
            TaskState::Unknown => vec![
                TaskEvent::Queued {
                    title: self.task.to_string(),
                },
                TaskEvent::Started {
                    generation: self.generation(),
                },
            ],
            TaskState::Queued | TaskState::InReview | TaskState::Failed => {
                vec![TaskEvent::Started {
                    generation: self.generation(),
                }]
            }
            TaskState::Blocked | TaskState::Paused => vec![TaskEvent::Resumed],
            TaskState::NeedsDecision => vec![TaskEvent::DecisionAnswered {
                key: "default".into(),
            }],
            _ => vec![TaskEvent::Started {
                generation: self.generation(),
            }],
        };
        steps.into_iter().all(|e| self.apply(e))
    }

    /// Drive the machine toward what the current declaration says.
    fn follow(&mut self) {
        let Some(current) = self.current.clone() else {
            return;
        };
        let Some(word) = current.state_word() else {
            return;
        };
        let (target, event) = match word {
            "working" => (TaskState::Running, None),
            "parked" => (
                TaskState::NeedsDecision,
                Some(TaskEvent::DecisionNeeded {
                    key: self.open.last().map_or("default".into(), |d| d.key.clone()),
                    question: current.note.clone(),
                }),
            ),
            "blocked" => (
                TaskState::Blocked,
                Some(TaskEvent::Blocked {
                    reason: current.note.clone(),
                }),
            ),
            "paused" => (
                TaskState::Paused,
                Some(TaskEvent::Paused {
                    reason: current.note.clone(),
                }),
            ),
            "done" => (
                TaskState::InReview,
                Some(TaskEvent::InReview {
                    pull_request: self.pull_request.clone(),
                }),
            ),
            _ => (
                TaskState::Failed,
                Some(TaskEvent::Failed {
                    reason: current.note.clone(),
                }),
            ),
        };
        if self.state == target {
            return;
        }
        let Some(event) = event else {
            self.resume_running();
            return;
        };
        // Blocked and paused are reachable from a waiting decision too;
        // failure from anywhere live.
        let direct = matches!(
            (self.state, &event),
            (
                TaskState::NeedsDecision,
                TaskEvent::Blocked { .. } | TaskEvent::Paused { .. }
            )
        ) || matches!(event, TaskEvent::Failed { .. });
        if (direct || self.resume_running()) && self.state != target {
            self.apply(event);
        }
    }

    fn absorb_status(&mut self, line: &StatusPayload) {
        // A new status file starts over, as firstmate re-reads it whole.
        if line.offset == 0 {
            self.lines.clear();
            self.open.clear();
            self.last_event = None;
        }
        self.lines.push(line.clone());
        let has_colon = line.raw.contains(':');
        if has_colon && is_event_verb(&line.verb) {
            self.last_event = Some(Declaration {
                verb: line.verb.clone(),
                note: line.note.clone(),
            });
        }
        self.fold_decision(line, has_colon);
        if line.verb == "done" {
            if let Some(pr) = pull_request_in(&line.note) {
                self.pull_request = Some(pr);
            }
        }
        self.current = match self.open.last() {
            Some(d) => Some(Declaration {
                verb: d.verb.clone(),
                note: d.note.clone(),
            }),
            None => self.last_event.clone(),
        };
        self.follow();
    }

    /// Firstmate's `_fm_decision_fold_line`.
    fn fold_decision(&mut self, line: &StatusPayload, has_colon: bool) {
        if !has_colon && !line.raw.contains("[key=") {
            return;
        }
        if has_colon
            && matches!(line.verb.as_str(), "done" | "failed")
            && matches!(self.fold_kind(), "ship" | "scout")
        {
            self.open.clear();
            return;
        }
        let opens = matches!(line.verb.as_str(), "needs-decision" | "blocked");
        let closes = matches!(line.verb.as_str(), "resolved" | "captain-held");
        if !opens && !closes {
            return;
        }
        let Some(key) = line.key.clone() else {
            return;
        };
        if !transition_allowed(&key, &line.note) {
            return;
        }
        self.open.retain(|d| d.key != key);
        if opens {
            self.open.push(OpenDecision {
                key,
                verb: line.verb.clone(),
                note: line.note.clone(),
            });
        }
    }

    fn absorb_spawn(&mut self, spawn: &SpawnMeta) {
        self.harness = Some(spawn.harness.clone());
        self.kind = spawn.kind.clone().or(self.kind.take());
        let new = self.generation.as_deref() != Some(spawn.generation.as_str());
        self.generation = Some(spawn.generation.clone());
        // A relaunch starts a new worker wherever the machine allows it.
        if new && self.state != TaskState::Unknown && self.state != TaskState::NeedsDecision {
            self.apply(TaskEvent::Started {
                generation: spawn.generation.clone(),
            });
            self.follow();
        }
    }

    /// Firstmate removed the task's record: it landed or was cleaned up.
    fn absorb_removed(&mut self) {
        match self.state {
            TaskState::InReview | TaskState::Running => {
                self.apply(TaskEvent::Completed);
            }
            TaskState::Done | TaskState::Failed | TaskState::Unknown => {}
            _ => {
                self.apply(TaskEvent::Cancelled);
            }
        }
        self.lines.clear();
    }
}

/// Verbs `last_status_line` treats as events (`_fm_status_event_scan`).
fn is_event_verb(verb: &str) -> bool {
    matches!(
        verb,
        "working"
            | "needs-decision"
            | "blocked"
            | "done"
            | "failed"
            | "note"
            | "paused"
            | "resolved"
            | "captain-held"
    )
}

/// Firstmate's `_fm_decision_key_transition_allowed`.
fn transition_allowed(key: &str, note: &str) -> bool {
    RESERVED_KEY_PREFIXES.iter().all(|prefix| {
        !key.starts_with(prefix)
            || note
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.contains(':'))
    })
}

/// The first pull request URL in a note.
fn pull_request_in(note: &str) -> Option<String> {
    note.split_whitespace()
        .map(|w| w.trim_matches(|c: char| matches!(c, ',' | '.' | ')' | '(' | ';')))
        .find(|w| w.starts_with("https://") && w.contains("/pull/"))
        .map(str::to_string)
}

type Key = (ProjectId, TaskId);

#[derive(Default)]
struct Inner {
    applied: Seq,
    tasks: BTreeMap<Key, FleetTask>,
    /// Each project's tasks with a metadata record, once the bridge has
    /// said.
    live: BTreeMap<ProjectId, BTreeSet<TaskId>>,
}

/// Every firstmate task, rebuilt from the log.
#[derive(Default)]
pub struct FirstmateFleet {
    inner: Mutex<Inner>,
}

impl FirstmateFleet {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The project's tasks that still have a metadata record, by id.
    pub fn tasks(&self, project: &ProjectId) -> Vec<FleetTask> {
        let inner = self.lock();
        let live = inner.live.get(project);
        inner
            .tasks
            .range((project.clone(), TaskId::from(""))..)
            .take_while(|((p, _), _)| p == project)
            .filter(|((_, t), _)| live.is_none_or(|l| l.contains(t)))
            .map(|(_, t)| t.clone())
            .collect()
    }

    /// One task, live or not.
    pub fn get(&self, project: &ProjectId, task: &TaskId) -> Option<FleetTask> {
        self.lock()
            .tasks
            .get(&(project.clone(), task.clone()))
            .cloned()
    }

    fn apply_event(&self, event: &Event) {
        let mut inner = self.lock();
        if event.seq <= inner.applied {
            return;
        }
        inner.applied = event.seq;
        let kind = event.kind.as_str();
        if kind == kinds::TASKS {
            let Ok(p) = event.decode::<TasksPayload>() else {
                return;
            };
            let now: BTreeSet<TaskId> = p.live.into_iter().map(TaskId::from).collect();
            let before = inner.live.insert(event.project.clone(), now.clone());
            for gone in before.unwrap_or_default().difference(&now) {
                if let Some(t) = inner.tasks.get_mut(&(event.project.clone(), gone.clone())) {
                    t.absorb_removed();
                }
            }
            return;
        }
        if kind != kinds::STATUS && kind != kinds::SPAWN {
            return;
        }
        let Some(task) = event.task.clone() else {
            return;
        };
        let key = (event.project.clone(), task.clone());
        // A task id back after its record was removed is a new task.
        let returned = inner
            .live
            .get(&event.project)
            .is_some_and(|l| !l.contains(&task))
            && inner
                .tasks
                .get(&key)
                .is_some_and(|t| matches!(t.state, TaskState::Done | TaskState::Failed));
        if returned {
            inner.tasks.remove(&key);
        }
        let record = inner
            .tasks
            .entry(key)
            .or_insert_with(|| FleetTask::new(event.project.clone(), task));
        if kind == kinds::SPAWN {
            if let Ok(spawn) = event.decode::<SpawnMeta>() {
                record.absorb_spawn(&spawn);
            }
        } else if let Ok(line) = event.decode::<StatusPayload>() {
            record.absorb_status(&line);
        }
    }
}

#[async_trait]
impl ReadModel for FirstmateFleet {
    async fn applied_through(&self) -> Result<Seq> {
        Ok(self.lock().applied)
    }

    async fn apply(&self, event: &Event) -> Result<()> {
        self.apply_event(event);
        Ok(())
    }

    async fn reset(&self) -> Result<()> {
        *self.lock() = Inner::default();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use quark_core::fake::MemoryEventLog;
    use quark_core::{replay, EventLog, HostId, NewEvent};

    use super::*;

    struct Feed {
        log: MemoryEventLog,
        offsets: BTreeMap<String, u64>,
    }

    impl Feed {
        fn new() -> Self {
            Self {
                log: MemoryEventLog::new(),
                offsets: BTreeMap::new(),
            }
        }

        async fn push(&self, task: Option<&str>, kind: &str, payload: impl Serialize) {
            let e = NewEvent::typed(
                HostId::from("h"),
                ProjectId::from("p"),
                task.map(TaskId::from),
                kind,
                &payload,
            )
            .unwrap();
            self.log.append(e).await.unwrap();
        }

        async fn spawn(&self, task: &str, generation: &str, kind: &str) {
            let m = quark_engine::meta::parse(&format!(
                "spawn_gen={generation}\nharness=claude\nkind={kind}\n"
            ))
            .unwrap();
            self.push(Some(task), kinds::SPAWN, m).await;
        }

        async fn status(&mut self, task: &str, raw: &str) {
            let e = quark_engine::status::parse_line(raw).unwrap();
            let offset = self.offsets.entry(task.into()).or_default();
            let p = StatusPayload {
                verb: e.verb.clone(),
                key: e.key.fold_key().map(str::to_string),
                corr: e.corr.clone(),
                note: e.note.clone(),
                raw: e.raw.clone(),
                offset: *offset,
            };
            *offset += raw.len() as u64 + 1;
            self.push(Some(task), kinds::STATUS, p).await;
        }

        async fn live(&self, ids: &[&str]) {
            let p = TasksPayload {
                live: ids.iter().map(|s| s.to_string()).collect(),
            };
            self.push(None, kinds::TASKS, p).await;
        }

        async fn fleet(&self) -> FirstmateFleet {
            let f = FirstmateFleet::new();
            replay(&self.log, &f, 3).await.unwrap();
            f
        }
    }

    fn word(t: &FleetTask) -> Option<&'static str> {
        t.current.as_ref().and_then(|c| c.state_word())
    }

    #[tokio::test]
    async fn decisions_survive_later_lines_and_close_by_key() {
        let mut f = Feed::new();
        f.spawn("t1", "s1.1.1", "ship").await;
        f.status("t1", "working: started").await;
        f.status("t1", "needs-decision [key=api]: which shape")
            .await;
        f.status("t1", "blocked: [key=creds] need a token").await;
        f.status("t1", "working: still going").await;
        f.live(&["t1"]).await;
        let t = &f.fleet().await.tasks(&"p".into())[0];
        let keys: Vec<_> = t.open.iter().map(|d| d.key.as_str()).collect();
        assert_eq!(keys, ["api", "creds"]);
        assert_eq!(t.open[1].note, "need a token");
        assert_eq!(word(t), Some("blocked"));
        assert_eq!(t.state, TaskState::Blocked);

        f.status("t1", "resolved [key=creds]: given").await;
        let t = &f.fleet().await.tasks(&"p".into())[0];
        assert_eq!(word(t), Some("parked"));
        assert_eq!(t.state, TaskState::NeedsDecision);
        assert_eq!(t.rejected, 0);

        f.status("t1", "resolved [key=api]: shape b").await;
        let t = &f.fleet().await.tasks(&"p".into())[0];
        assert!(t.open.is_empty());
        // The newest event is a resolution, which is not a state.
        assert_eq!(word(t), None);
    }

    #[tokio::test]
    async fn the_machine_follows_the_log() {
        let mut f = Feed::new();
        f.spawn("t1", "s1.1.1", "ship").await;
        f.status("t1", "working: go").await;
        let t = f.fleet().await.get(&"p".into(), &"t1".into()).unwrap();
        assert_eq!(t.state, TaskState::Running);
        f.status("t1", "needs-decision: pick").await;
        f.status("t1", "resolved: a").await;
        f.status("t1", "paused: waiting on CI until 2026-10-07T06:00Z")
            .await;
        let t = f.fleet().await.get(&"p".into(), &"t1".into()).unwrap();
        assert_eq!(t.state, TaskState::Paused);
        f.status("t1", "done: PR https://github.com/o/r/pull/9 checks green")
            .await;
        let t = f.fleet().await.get(&"p".into(), &"t1".into()).unwrap();
        assert_eq!(t.state, TaskState::InReview);
        assert_eq!(
            t.pull_request.as_deref(),
            Some("https://github.com/o/r/pull/9")
        );
        assert_eq!(t.rejected, 0);
        // Fixing a review comment after done reopens it.
        f.status("t1", "working: fixing review").await;
        f.spawn("t1", "s2.1.1", "ship").await;
        let t = f.fleet().await.get(&"p".into(), &"t1".into()).unwrap();
        assert_eq!(t.state, TaskState::Running);
        assert_eq!(t.generation.as_deref(), Some("s2.1.1"));
    }

    #[tokio::test]
    async fn terminal_lines_clear_a_ships_decisions_not_a_secondmates() {
        let mut f = Feed::new();
        f.spawn("ship", "s1.1.1", "ship").await;
        f.spawn("mate", "s1.1.2", "secondmate").await;
        for t in ["ship", "mate"] {
            f.status(t, "needs-decision [key=k]: q").await;
            f.status(t, "done: finished").await;
        }
        let fleet = f.fleet().await;
        assert!(fleet
            .get(&"p".into(), &"ship".into())
            .unwrap()
            .open
            .is_empty());
        let mate = fleet.get(&"p".into(), &"mate".into()).unwrap();
        assert_eq!(mate.open.len(), 1);
        assert_eq!(word(&mate), Some("parked"));
    }

    #[tokio::test]
    async fn reserved_keys_need_their_owners_vocabulary() {
        let mut f = Feed::new();
        f.spawn("t", "s1.1.1", "secondmate").await;
        f.status("t", "needs-decision [key=pending-reply-ab]: hijack")
            .await;
        f.status(
            "t",
            "needs-decision [key=pending-reply-cd]: pending-reply-missed: no reply",
        )
        .await;
        let t = f.fleet().await.get(&"p".into(), &"t".into()).unwrap();
        let keys: Vec<_> = t.open.iter().map(|d| d.key.as_str()).collect();
        assert_eq!(keys, ["pending-reply-cd"]);
    }

    #[tokio::test]
    async fn removed_tasks_leave_and_a_new_status_file_starts_over() {
        let mut f = Feed::new();
        f.spawn("a", "s1.1.1", "ship").await;
        f.spawn("b", "s1.1.2", "ship").await;
        f.status("a", "done: PR https://github.com/o/r/pull/1")
            .await;
        f.status("b", "needs-decision: q").await;
        f.live(&["a", "b"]).await;
        f.live(&["b"]).await;
        let fleet = f.fleet().await;
        let ids: Vec<_> = fleet
            .tasks(&"p".into())
            .iter()
            .map(|t| t.task.to_string())
            .collect();
        assert_eq!(ids, ["b"]);
        assert_eq!(
            fleet.get(&"p".into(), &"a".into()).unwrap().state,
            TaskState::Done
        );

        // b's status file was replaced.
        f.offsets.insert("b".into(), 0);
        f.status("b", "working: fresh").await;
        let b = f.fleet().await.get(&"p".into(), &"b".into()).unwrap();
        assert!(b.open.is_empty());
        assert_eq!(b.lines.len(), 1);
        assert_eq!(word(&b), Some("working"));

        // The id comes back as a new task.
        f.offsets.insert("a".into(), 0);
        f.spawn("a", "s9.1.1", "ship").await;
        f.status("a", "working: again").await;
        f.live(&["a", "b"]).await;
        let a = f.fleet().await.get(&"p".into(), &"a".into()).unwrap();
        assert_eq!(a.state, TaskState::Running);
        assert_eq!(a.rejected, 0);
    }
}
