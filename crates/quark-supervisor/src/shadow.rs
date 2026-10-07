//! Slice 4's shadow: firstmate's workers, as the native supervisor would
//! have tracked them.
//!
//! [`SupervisedFleet`] is a read model over the events the slice 1 bridge
//! mirrors from firstmate (`quark_eventlog::firstmate`). It treats each of
//! firstmate's tasks as if the supervisor ran it:
//!
//! - a `firstmate.spawn` is a launch (`Queued` and `Started` for a new
//!   task, `Started` for a new generation);
//! - each status line the worker wrote is read by the native file protocol
//!   (`quark_worker::parse_status_line`) and turned into transitions by
//!   [`crate::rules::message_transitions`], the supervisor's own rule;
//! - a `resolved` or `captain-held` line, which firstmate appends when it
//!   answers, is an answer ([`crate::rules::answer_transitions`]).
//!
//! Every transition goes through the reference task machine, and the first
//! one it refuses ends that message, as in the supervisor. quarkd compares
//! the resulting states with firstmate's (`docs/shadow-readiness.md`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use async_trait::async_trait;
use quark_core::task::ReferenceMachine;
use quark_core::{Event, ProjectId, ReadModel, Result, Seq, TaskEvent, TaskId, TaskMachine};
use quark_eventlog::firstmate::{kinds, StatusPayload, TasksPayload};
use quark_systems::TaskState;
use serde::{Deserialize, Serialize};

use crate::rules;

/// One of firstmate's tasks under native supervision rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisedTask {
    pub task: TaskId,
    pub kind: Option<String>,
    pub generation: Option<String>,
    pub state: TaskState,
    pub open_decisions: BTreeSet<String>,
    /// Transitions the machine refused.
    pub refused: u32,
}

impl SupervisedTask {
    fn new(task: TaskId) -> Self {
        Self {
            task,
            kind: None,
            generation: None,
            state: TaskState::Unknown,
            open_decisions: BTreeSet::new(),
            refused: 0,
        }
    }

    /// Apply `events` in order until the machine refuses one.
    fn apply(&mut self, events: Vec<TaskEvent>) {
        for e in events {
            match ReferenceMachine.apply(self.state, &e) {
                Ok(next) => {
                    self.state = next;
                    match &e {
                        TaskEvent::DecisionNeeded { key, .. } => {
                            self.open_decisions.insert(key.clone());
                        }
                        TaskEvent::DecisionAnswered { key } => {
                            self.open_decisions.remove(key);
                        }
                        TaskEvent::Started { generation } => {
                            self.generation = Some(generation.clone())
                        }
                        _ => {}
                    }
                }
                Err(_) => {
                    self.refused += 1;
                    return;
                }
            }
        }
    }

    fn spawned(&mut self, generation: &str) {
        if self.generation.as_deref() == Some(generation) {
            return;
        }
        let mut events = Vec::new();
        if self.state == TaskState::Unknown {
            events.push(TaskEvent::Queued {
                title: self.task.to_string(),
            });
        }
        events.push(TaskEvent::Started {
            generation: generation.to_string(),
        });
        self.apply(events);
        self.generation = Some(generation.to_string());
    }

    fn status(&mut self, line: &StatusPayload) {
        if matches!(line.verb.as_str(), "resolved" | "captain-held") {
            if let Some(key) = &line.key {
                if let Some(events) =
                    rules::answer_transitions(self.state, &self.open_decisions, key)
                {
                    self.apply(events);
                }
            }
            return;
        }
        if let Ok(Some(message)) = quark_worker::parse_status_line(&line.raw) {
            let events = rules::message_transitions(self.state, &self.open_decisions, &message);
            self.apply(events);
        }
    }
}

#[derive(Default)]
struct Inner {
    applied: Seq,
    tasks: BTreeMap<(ProjectId, TaskId), SupervisedTask>,
    live: BTreeMap<ProjectId, BTreeSet<TaskId>>,
}

/// Every firstmate task, supervised natively in the log's imagination.
#[derive(Default)]
pub struct SupervisedFleet {
    inner: Mutex<Inner>,
}

impl SupervisedFleet {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The project's tasks that still have a firstmate record, by id.
    pub fn tasks(&self, project: &ProjectId) -> Vec<SupervisedTask> {
        let inner = self.lock();
        let live = inner.live.get(project);
        inner
            .tasks
            .iter()
            .filter(|((p, t), _)| p == project && live.is_none_or(|l| l.contains(t)))
            .map(|(_, t)| t.clone())
            .collect()
    }

    fn apply_event(&self, event: &Event) {
        let mut inner = self.lock();
        if event.seq <= inner.applied {
            return;
        }
        inner.applied = event.seq;
        let kind = event.kind.as_str();
        if kind == kinds::TASKS {
            if let Ok(p) = event.decode::<TasksPayload>() {
                let now = p.live.into_iter().map(TaskId::from).collect();
                inner.live.insert(event.project.clone(), now);
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
        // An id back after its record was removed is a new task.
        let gone = inner
            .live
            .get(&event.project)
            .is_some_and(|l| !l.contains(&task));
        if gone
            && inner
                .tasks
                .get(&key)
                .is_some_and(|t| matches!(t.state, TaskState::Done | TaskState::Failed))
        {
            inner.tasks.remove(&key);
        }
        let record = inner
            .tasks
            .entry(key)
            .or_insert_with(|| SupervisedTask::new(task));
        if kind == kinds::SPAWN {
            if let Ok(spawn) = event.decode::<SpawnFields>() {
                record.kind = spawn.kind.or(record.kind.take());
                record.spawned(&spawn.generation);
            }
        } else if let Ok(line) = event.decode::<StatusPayload>() {
            record.status(&line);
        }
    }
}

/// The fields of a `firstmate.spawn` payload the shadow reads.
#[derive(Deserialize)]
struct SpawnFields {
    generation: String,
    #[serde(default)]
    kind: Option<String>,
}

#[async_trait]
impl ReadModel for SupervisedFleet {
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
    use serde::Serialize;

    use super::*;

    async fn push(log: &MemoryEventLog, task: &str, kind: &str, payload: impl Serialize) {
        let e = NewEvent::typed(
            HostId::from("h"),
            ProjectId::from("p"),
            Some(TaskId::from(task)),
            kind,
            &payload,
        )
        .unwrap();
        log.append(e).await.unwrap();
    }

    async fn line(log: &MemoryEventLog, task: &str, raw: &str) {
        let e = quark_worker::parse_status_line(raw).ok().flatten();
        let verb = raw.split([':', ' ', '[']).next().unwrap().to_string();
        let key = match &e {
            Some(quark_core::worker::WorkerMessage::Ask { key, .. }) => Some(key.clone()),
            _ => raw
                .split("[key=")
                .nth(1)
                .and_then(|r| r.split(']').next())
                .map(str::to_string),
        };
        let p = StatusPayload {
            verb,
            key,
            corr: None,
            note: String::new(),
            raw: raw.into(),
            offset: 0,
        };
        push(log, task, kinds::STATUS, p).await;
    }

    async fn fleet(log: &MemoryEventLog) -> SupervisedFleet {
        let f = SupervisedFleet::new();
        replay(log, &f, 10).await.unwrap();
        f
    }

    #[tokio::test]
    async fn firstmates_workers_under_native_rules() {
        let log = MemoryEventLog::new();
        push(
            &log,
            "t",
            kinds::SPAWN,
            serde_json::json!({"generation": "g1", "harness": "claude", "kind": "ship"}),
        )
        .await;
        line(&log, "t", "working: go").await;
        line(&log, "t", "needs-decision [key=api]: which").await;
        let t = &fleet(&log).await.tasks(&"p".into())[0];
        assert_eq!(t.state, TaskState::NeedsDecision);
        assert!(t.open_decisions.contains("api"));

        line(&log, "t", "resolved [key=api]: b").await;
        line(&log, "t", "blocked: token").await;
        line(&log, "t", "working: again").await;
        let t = &fleet(&log).await.tasks(&"p".into())[0];
        assert_eq!(
            t.state,
            TaskState::Running,
            "a report after a block resumes"
        );

        line(&log, "t", "done: PR https://github.com/o/r/pull/2").await;
        let t = &fleet(&log).await.tasks(&"p".into())[0];
        assert_eq!(t.state, TaskState::InReview);
        assert_eq!(t.refused, 0);
    }

    #[tokio::test]
    async fn an_answer_while_blocked_leaves_the_decision_open() {
        let log = MemoryEventLog::new();
        push(
            &log,
            "t",
            kinds::SPAWN,
            serde_json::json!({"generation": "g1", "harness": "claude"}),
        )
        .await;
        line(&log, "t", "needs-decision [key=a]: q").await;
        line(&log, "t", "blocked: stuck").await;
        line(&log, "t", "resolved [key=a]: yes").await;
        let t = &fleet(&log).await.tasks(&"p".into())[0];
        assert_eq!(t.state, TaskState::Blocked);
        assert!(t.open_decisions.contains("a"));
    }
}
