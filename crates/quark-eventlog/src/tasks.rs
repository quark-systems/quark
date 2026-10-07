//! Task state as a read model of the log, and the writer that keeps every
//! transition an event.
//!
//! [`TaskStates`] folds `task.transition` events through a
//! [`TaskMachine`] (the reference table by default). It is replay-safe: an
//! event at or below the position it has applied is skipped, so recovery is
//! just [`quark_core::replay`] from wherever it stopped.
//!
//! [`TaskLedger`] is the only way the native engine changes a task: it
//! checks the transition against the machine first, appends the event, and
//! only then updates the model. An illegal transition appends nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use quark_core::event::kinds;
use quark_core::task::ReferenceMachine;
use quark_core::{
    replay, Event, EventLog, HostId, NewEvent, ProjectId, ReadModel, Result, Seq, TaskEvent,
    TaskId, TaskMachine,
};
use quark_systems::TaskState;
use serde::{Deserialize, Serialize};

/// Events read per batch during replay.
const REPLAY_BATCH: usize = 512;

/// One task as the log says it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub project: ProjectId,
    pub task: TaskId,
    pub state: TaskState,
    pub title: Option<String>,
    /// The current worker's generation, from the last `Started`.
    pub generation: Option<String>,
    pub pull_request: Option<String>,
    /// Keys of decisions asked and not yet answered.
    pub open_decisions: BTreeSet<String>,
    /// The seq of the last transition applied.
    pub seq: Seq,
    /// Transitions in the log the machine refused (left the state alone).
    pub rejected: u32,
}

impl TaskRecord {
    fn new(project: ProjectId, task: TaskId) -> Self {
        Self {
            project,
            task,
            state: TaskState::Unknown,
            title: None,
            generation: None,
            pull_request: None,
            open_decisions: BTreeSet::new(),
            seq: Seq::ZERO,
            rejected: 0,
        }
    }

    fn absorb(&mut self, event: &TaskEvent) {
        match event {
            TaskEvent::Queued { title } => self.title = Some(title.clone()),
            TaskEvent::Started { generation } => self.generation = Some(generation.clone()),
            TaskEvent::DecisionNeeded { key, .. } => {
                self.open_decisions.insert(key.clone());
            }
            TaskEvent::DecisionAnswered { key } => {
                self.open_decisions.remove(key);
            }
            TaskEvent::InReview { pull_request } => self.pull_request = pull_request.clone(),
            _ => {}
        }
    }
}

type Key = (ProjectId, TaskId);

#[derive(Default)]
struct Inner {
    applied: Seq,
    tasks: BTreeMap<Key, TaskRecord>,
}

/// Every task's state, rebuilt from the log.
pub struct TaskStates<M = ReferenceMachine> {
    machine: M,
    inner: Mutex<Inner>,
}

impl Default for TaskStates<ReferenceMachine> {
    fn default() -> Self {
        Self::new(ReferenceMachine)
    }
}

impl<M: TaskMachine> TaskStates<M> {
    pub fn new(machine: M) -> Self {
        Self {
            machine,
            inner: Mutex::new(Inner::default()),
        }
    }

    pub fn machine(&self) -> &M {
        &self.machine
    }

    /// The task, if the log has mentioned it.
    pub fn get(&self, project: &ProjectId, task: &TaskId) -> Option<TaskRecord> {
        self.lock()
            .tasks
            .get(&(project.clone(), task.clone()))
            .cloned()
    }

    /// The task's state; `Unknown` before its first event.
    pub fn state(&self, project: &ProjectId, task: &TaskId) -> TaskState {
        self.get(project, task)
            .map_or(TaskState::Unknown, |r| r.state)
    }

    /// Every task, ordered by project then task.
    pub fn all(&self) -> Vec<TaskRecord> {
        self.lock().tasks.values().cloned().collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn apply_event(&self, event: &Event) {
        let mut inner = self.lock();
        if event.seq <= inner.applied {
            return;
        }
        inner.applied = event.seq;
        if event.kind.as_str() != kinds::TASK {
            return;
        }
        // A transition without a task has nothing to change.
        let Some(task) = event.task.clone() else {
            return;
        };
        let record = inner
            .tasks
            .entry((event.project.clone(), task.clone()))
            .or_insert_with(|| TaskRecord::new(event.project.clone(), task));
        let next = event
            .decode::<TaskEvent>()
            .and_then(|e| self.machine.apply(record.state, &e).map(|s| (s, e)));
        match next {
            Ok((state, e)) => {
                record.state = state;
                record.absorb(&e);
                record.seq = event.seq;
            }
            // The log is the truth even when it holds a transition this
            // machine refuses: keep replaying and count it.
            Err(_) => record.rejected += 1,
        }
    }
}

#[async_trait]
impl<M: TaskMachine> ReadModel for TaskStates<M> {
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

/// Changes tasks only through the log.
pub struct TaskLedger<M = ReferenceMachine> {
    log: Arc<dyn EventLog>,
    states: Arc<TaskStates<M>>,
    host: HostId,
    write: tokio::sync::Mutex<()>,
}

impl TaskLedger<ReferenceMachine> {
    /// A ledger on the reference machine, recovered from `log`.
    pub async fn open(log: Arc<dyn EventLog>, host: HostId) -> Result<Self> {
        Self::with_machine(log, host, ReferenceMachine).await
    }
}

impl<M: TaskMachine + 'static> TaskLedger<M> {
    /// A ledger on `machine`, recovered by replaying `log`.
    pub async fn with_machine(log: Arc<dyn EventLog>, host: HostId, machine: M) -> Result<Self> {
        let states = Arc::new(TaskStates::new(machine));
        replay(log.as_ref(), states.as_ref(), REPLAY_BATCH).await?;
        Ok(Self {
            log,
            states,
            host,
            write: tokio::sync::Mutex::new(()),
        })
    }

    /// The read model this ledger keeps current.
    pub fn states(&self) -> Arc<TaskStates<M>> {
        self.states.clone()
    }

    /// Catch up with events other writers appended.
    pub async fn refresh(&self) -> Result<Seq> {
        replay(self.log.as_ref(), self.states.as_ref(), REPLAY_BATCH).await
    }

    /// Record `event` for `task`: refused with
    /// [`quark_core::CoreError::IllegalTransition`] (and nothing appended)
    /// when the machine does not allow it from the task's current state.
    pub async fn record(
        &self,
        project: ProjectId,
        task: TaskId,
        event: TaskEvent,
    ) -> Result<(Seq, TaskState)> {
        let _write = self.write.lock().await;
        self.refresh().await?;
        let current = self.states.state(&project, &task);
        let next = self.states.machine().apply(current, &event)?;
        let new = NewEvent::typed(self.host.clone(), project, Some(task), kinds::TASK, &event)?;
        let seq = self.log.append(new).await?;
        // Apply everything up to and including ours, in order.
        self.refresh().await?;
        debug_assert!(self.states.lock().applied >= seq);
        Ok((seq, next))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quark_core::fake::MemoryEventLog;
    use quark_core::CoreError;

    fn ids() -> (ProjectId, TaskId) {
        (ProjectId::from("p"), TaskId::from("t1"))
    }

    #[tokio::test]
    async fn ledger_records_and_recovers() {
        let log = Arc::new(MemoryEventLog::new());
        let (p, t) = ids();
        let ledger = TaskLedger::open(log.clone(), HostId::from("h"))
            .await
            .unwrap();
        ledger
            .record(
                p.clone(),
                t.clone(),
                TaskEvent::Queued { title: "x".into() },
            )
            .await
            .unwrap();
        ledger
            .record(
                p.clone(),
                t.clone(),
                TaskEvent::Started {
                    generation: "g1".into(),
                },
            )
            .await
            .unwrap();
        let (_, s) = ledger
            .record(
                p.clone(),
                t.clone(),
                TaskEvent::DecisionNeeded {
                    key: "k".into(),
                    question: "?".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(s, TaskState::NeedsDecision);

        // A fresh ledger on the same log rebuilds the same state.
        let again = TaskLedger::open(log.clone(), HostId::from("h"))
            .await
            .unwrap();
        let r = again.states().get(&p, &t).unwrap();
        assert_eq!(r.state, TaskState::NeedsDecision);
        assert_eq!(r.title.as_deref(), Some("x"));
        assert_eq!(r.generation.as_deref(), Some("g1"));
        assert!(r.open_decisions.contains("k"));
        assert_eq!(r.seq, Seq(3));
    }

    #[tokio::test]
    async fn illegal_transition_appends_nothing() {
        let log = Arc::new(MemoryEventLog::new());
        let (p, t) = ids();
        let ledger = TaskLedger::open(log.clone(), HostId::from("h"))
            .await
            .unwrap();
        let err = ledger
            .record(p.clone(), t.clone(), TaskEvent::Completed)
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::IllegalTransition(_)));
        assert!(log.events().is_empty());
        assert_eq!(ledger.states().state(&p, &t), TaskState::Unknown);
    }

    #[tokio::test]
    async fn replay_skips_applied_and_counts_refused() {
        let log = MemoryEventLog::new();
        let (p, t) = ids();
        let push = |e: &TaskEvent| {
            NewEvent::typed(
                HostId::from("h"),
                p.clone(),
                Some(t.clone()),
                kinds::TASK,
                e,
            )
            .unwrap()
        };
        log.append(push(&TaskEvent::Queued { title: "x".into() }))
            .await
            .unwrap();
        // Written by something that skipped the machine: refused on replay.
        log.append(push(&TaskEvent::Completed)).await.unwrap();
        log.append(NewEvent::new(
            HostId::from("h"),
            p.clone(),
            None,
            "worker.message",
            serde_json::json!({}),
        ))
        .await
        .unwrap();
        log.append(push(&TaskEvent::Started {
            generation: "g".into(),
        }))
        .await
        .unwrap();

        let states = TaskStates::default();
        assert_eq!(replay(&log, &states, 2).await.unwrap(), Seq(4));
        let r = states.get(&p, &t).unwrap();
        assert_eq!(r.state, TaskState::Running);
        assert_eq!(r.rejected, 1);

        // Applying an old event again changes nothing.
        let old = log.read(Seq::ZERO, 1).await.unwrap().remove(0);
        states.apply(&old).await.unwrap();
        assert_eq!(states.get(&p, &t).unwrap(), r);
    }
}
