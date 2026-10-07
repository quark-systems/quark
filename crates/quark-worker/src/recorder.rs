//! Records worker messages in the event log.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use quark_core::event::kinds;
use quark_core::worker::{Transport, WorkerEnvelope, WorkerMessage, WorkerProtocol};
use quark_core::{CoreError, EventId, EventLog, HostId, NewEvent, ProjectId, Result, TaskId};

/// Who is talking: a task and the generation of the worker on it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WorkerIdentity {
    pub task: TaskId,
    pub generation: String,
}

impl WorkerIdentity {
    pub fn new(task: impl Into<TaskId>, generation: impl Into<String>) -> Self {
        Self {
            task: task.into(),
            generation: generation.into(),
        }
    }

    /// The envelope for `message` arriving on `via`.
    pub fn envelope(&self, via: Transport, message: WorkerMessage) -> WorkerEnvelope {
        WorkerEnvelope {
            task: self.task.clone(),
            generation: self.generation.clone(),
            via,
            message,
        }
    }
}

/// What the recorder needs to know about a live task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub project: ProjectId,
    /// The current worker's generation; messages from any other are refused.
    pub generation: String,
    /// The harness manifest's `hooks.events`: harness hook name to neutral
    /// signal (`busy`, `turn_end`, `report`).
    pub hook_events: BTreeMap<String, String>,
}

/// Looks up the task a message names. quarkd implements it from its task
/// store (later the event log's read model); [`MemoryTasks`] is the fake.
#[async_trait]
pub trait TaskDirectory: Send + Sync {
    /// The task's binding, or `None` when no such task exists.
    async fn binding(&self, task: &TaskId) -> Result<Option<Binding>>;
}

/// An in-memory [`TaskDirectory`].
#[derive(Debug, Clone, Default)]
pub struct MemoryTasks {
    inner: Arc<Mutex<HashMap<TaskId, Binding>>>,
}

impl MemoryTasks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a task or replaces its binding (a relaunch).
    pub fn bind(&self, task: impl Into<TaskId>, binding: Binding) {
        self.inner.lock().unwrap().insert(task.into(), binding);
    }

    pub fn unbind(&self, task: &TaskId) {
        self.inner.lock().unwrap().remove(task);
    }
}

#[async_trait]
impl TaskDirectory for MemoryTasks {
    async fn binding(&self, task: &TaskId) -> Result<Option<Binding>> {
        Ok(self.inner.lock().unwrap().get(task).cloned())
    }
}

/// The [`WorkerProtocol`] every transport feeds.
///
/// `receive` appends a `worker.message` event and only then answers, so an
/// `Ok` means the message is in the log. A message for an unknown task is
/// refused with [`CoreError::NotFound`] and not recorded (there is no
/// Project to file it under). A message from a replaced generation is
/// recorded, then answered with [`CoreError::Refused`].
pub struct Recorder {
    log: Arc<dyn EventLog>,
    tasks: Arc<dyn TaskDirectory>,
    host: HostId,
}

impl Recorder {
    pub fn new(log: Arc<dyn EventLog>, tasks: Arc<dyn TaskDirectory>, host: HostId) -> Self {
        Self { log, tasks, host }
    }

    /// The task's binding, or `NotFound`.
    pub async fn binding(&self, task: &TaskId) -> Result<Binding> {
        self.tasks
            .binding(task)
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("task {task}")))
    }

    /// Like [`WorkerProtocol::receive`] with a caller-chosen event id, so a
    /// transport that may deliver the same message twice (a rescanned status
    /// file, a retried POST) records it once.
    pub async fn receive_as(&self, id: EventId, envelope: WorkerEnvelope) -> Result<EventId> {
        validate(&envelope.message)?;
        let binding = self.binding(&envelope.task).await?;
        let current = binding.generation == envelope.generation;
        let mut event = NewEvent::typed(
            self.host.clone(),
            binding.project,
            Some(envelope.task.clone()),
            kinds::WORKER,
            &envelope,
        )?;
        event.id = id;
        self.log.append(event).await?;
        if !current {
            return Err(CoreError::Refused(format!(
                "worker generation {} on task {} was replaced; stop working on it",
                envelope.generation, envelope.task
            )));
        }
        Ok(id)
    }
}

#[async_trait]
impl WorkerProtocol for Recorder {
    async fn receive(&self, envelope: WorkerEnvelope) -> Result<EventId> {
        self.receive_as(EventId::new(), envelope).await
    }
}

/// Rejects messages missing the text that makes them useful.
fn validate(message: &WorkerMessage) -> Result<()> {
    let empty = |field: &str, value: &str| {
        if value.trim().is_empty() {
            Err(CoreError::Invalid(format!("{field} is empty")))
        } else {
            Ok(())
        }
    };
    match message {
        WorkerMessage::Report { state, .. } => empty("state", state),
        WorkerMessage::Ask { key, question } => {
            empty("key", key)?;
            empty("question", question)
        }
        WorkerMessage::Learned { fact } => empty("fact", fact),
        WorkerMessage::Done { summary, .. } => empty("summary", summary),
        WorkerMessage::Signal { signal } => empty("signal", signal),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use quark_core::fake::MemoryEventLog;

    pub(crate) fn setup() -> (Arc<Recorder>, MemoryEventLog, MemoryTasks) {
        let log = MemoryEventLog::new();
        let tasks = MemoryTasks::new();
        tasks.bind(
            "t1",
            Binding {
                project: ProjectId::from("p"),
                generation: "g2".into(),
                hook_events: BTreeMap::from([
                    ("Stop".to_string(), "turn_end".to_string()),
                    ("UserPromptSubmit".to_string(), "busy".to_string()),
                    ("Notify".to_string(), "report".to_string()),
                ]),
            },
        );
        let recorder = Recorder::new(
            Arc::new(log.clone()),
            Arc::new(tasks.clone()),
            HostId::from("h"),
        );
        (Arc::new(recorder), log, tasks)
    }

    fn learned(generation: &str) -> WorkerEnvelope {
        WorkerIdentity::new("t1", generation).envelope(
            Transport::Mcp,
            WorkerMessage::Learned {
                fact: "tests need tmux".into(),
            },
        )
    }

    #[tokio::test]
    async fn records_before_answering() {
        let (rec, log, _) = setup();
        let id = rec.receive(learned("g2")).await.unwrap();
        let events = log.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, id);
        assert_eq!(events[0].kind.as_str(), kinds::WORKER);
        assert_eq!(events[0].project.as_str(), "p");
        assert_eq!(events[0].task.as_ref().unwrap().as_str(), "t1");
        assert_eq!(events[0].decode::<WorkerEnvelope>().unwrap(), learned("g2"));
    }

    #[tokio::test]
    async fn old_generation_is_recorded_then_refused() {
        let (rec, log, _) = setup();
        let err = rec.receive(learned("g1")).await.unwrap_err();
        assert!(matches!(err, CoreError::Refused(_)), "{err}");
        assert_eq!(log.events().len(), 1);
    }

    #[tokio::test]
    async fn unknown_task_is_not_recorded() {
        let (rec, log, tasks) = setup();
        tasks.unbind(&TaskId::from("t1"));
        let err = rec.receive(learned("g2")).await.unwrap_err();
        assert!(matches!(err, CoreError::NotFound(_)), "{err}");
        assert!(log.events().is_empty());
    }

    #[tokio::test]
    async fn empty_text_is_invalid() {
        let (rec, log, _) = setup();
        let env = WorkerIdentity::new("t1", "g2")
            .envelope(Transport::Mcp, WorkerMessage::Learned { fact: " ".into() });
        assert!(matches!(
            rec.receive(env).await.unwrap_err(),
            CoreError::Invalid(_)
        ));
        assert!(log.events().is_empty());
    }

    #[tokio::test]
    async fn same_id_records_once() {
        let (rec, log, _) = setup();
        let id = EventId::new();
        rec.receive_as(id, learned("g2")).await.unwrap();
        rec.receive_as(id, learned("g2")).await.unwrap();
        assert_eq!(log.events().len(), 1);
    }
}
