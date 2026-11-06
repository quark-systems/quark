//! The supervisor's view of its tasks, rebuilt from the event log.
//!
//! [`Fleet`] folds `supervisor.*` events into one [`Worker`] record per
//! task. Like every read model it is replay-safe: an event at or below the
//! position it has applied is skipped. It is also the
//! [`quark_worker::TaskDirectory`] the worker protocol checks callers
//! against, so a message from a replaced generation is refused as soon as
//! the new one is launched.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use async_trait::async_trait;
use quark_core::event::kinds;
use quark_core::session::SessionId;
use quark_core::worker::{WorkerEnvelope, WorkerMessage};
use quark_core::worktree::{ReturnOutcome, Worktree};
use quark_core::{Event, ProjectId, ReadModel, Result, Seq, TaskId};
use quark_harness::ManifestRegistry;
use quark_worker::{Binding, TaskDirectory};
use serde::{Deserialize, Serialize};

use crate::events::{Assignment, Cause, SupervisorEvent, PREFIX};

/// A steering message and whether it reached a worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Steer {
    pub id: String,
    pub text: String,
    /// The generation it reached, once delivered.
    pub delivered_to: Option<String>,
    /// When it was recorded.
    #[serde(default)]
    pub sent_at: Option<SystemTime>,
    /// When it was typed into the worker's session.
    #[serde(default)]
    pub delivered_at: Option<SystemTime>,
    /// Where its delivery is in the log; only worker messages after it
    /// acknowledge it.
    #[serde(default)]
    pub delivered_seq: Option<Seq>,
    /// When the worker that got it started a turn (its harness's `busy`
    /// hook) after delivery.
    #[serde(default)]
    pub read_at: Option<SystemTime>,
    /// When the worker that got it finished a turn or reported after
    /// delivery.
    #[serde(default)]
    pub answered_at: Option<SystemTime>,
}

/// Where a steering message is, as the coordinator and the app show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SteerStatus {
    /// Recorded, waiting for a live worker.
    Queued,
    /// Typed into the worker's session; the worker has not reacted since.
    Delivered,
    /// The worker started a turn after it, but has not finished one.
    Read,
    /// The worker finished a turn or reported after it.
    Answered,
}

impl SteerStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SteerStatus::Queued => "queued",
            SteerStatus::Delivered => "delivered",
            SteerStatus::Read => "read",
            SteerStatus::Answered => "answered",
        }
    }
}

impl Steer {
    pub fn new(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            delivered_to: None,
            sent_at: None,
            delivered_at: None,
            delivered_seq: None,
            read_at: None,
            answered_at: None,
        }
    }

    pub fn status(&self) -> SteerStatus {
        if self.answered_at.is_some() {
            SteerStatus::Answered
        } else if self.read_at.is_some() {
            SteerStatus::Read
        } else if self.delivered_to.is_some() {
            SteerStatus::Delivered
        } else {
            SteerStatus::Queued
        }
    }

    /// A worker message from `generation` at `seq`: a turn starting reads
    /// the message, anything else answers it.
    fn acknowledge(&mut self, generation: &str, seq: Seq, at: SystemTime, message: &WorkerMessage) {
        if self.delivered_to.as_deref() != Some(generation)
            || self.delivered_seq.is_some_and(|d| seq <= d)
            || self.answered_at.is_some()
        {
            return;
        }
        match message {
            WorkerMessage::Signal { signal } if signal == "busy" => {
                self.read_at.get_or_insert(at);
            }
            _ => {
                self.read_at.get_or_insert(at);
                self.answered_at = Some(at);
            }
        }
    }
}

/// Why the worker of a generation cannot work until a person acts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unhealthy {
    pub generation: String,
    pub reason: String,
    pub since: SystemTime,
}

/// The worker generation currently on a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Generation {
    pub id: String,
    pub session: SessionId,
    pub backend: String,
    pub cause: Cause,
    pub dir: PathBuf,
    pub harness: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Set once its session ended, with the exit code if one is known.
    pub exited: Option<Option<i32>>,
    /// When it was launched.
    #[serde(default)]
    pub started_at: Option<SystemTime>,
}

impl Generation {
    /// The worker's status file, its file-protocol fallback.
    pub fn status_file(&self) -> PathBuf {
        self.dir.join(crate::launch::STATUS_FILE)
    }
}

/// One supervised task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worker {
    pub project: ProjectId,
    pub task: TaskId,
    pub assignment: Assignment,
    pub worktree: Option<Worktree>,
    /// When the task took its worktree. Pool slots are reused, so session
    /// logs in the directory from before this belong to an earlier task.
    #[serde(default)]
    pub worktree_at: Option<SystemTime>,
    pub current: Option<Generation>,
    /// Generations started by the supervisor itself since the last spawn
    /// or relaunch a caller asked for.
    pub recoveries: u32,
    pub steers: Vec<Steer>,
    /// Set while the current generation cannot work until a person acts.
    #[serde(default)]
    pub unhealthy: Option<Unhealthy>,
    /// How the worktree went back, once it has.
    pub released: Option<ReturnOutcome>,
}

impl Worker {
    /// Steering messages no generation has received yet, oldest first.
    pub fn undelivered(&self) -> impl Iterator<Item = &Steer> {
        self.steers.iter().filter(|s| s.delivered_to.is_none())
    }

    /// Whether the worktree has been given back to the pool.
    pub fn is_released(&self) -> bool {
        matches!(self.released, Some(ReturnOutcome::Returned))
    }

    /// The last steering message delivered to the current generation that
    /// it has not answered, if the newest delivered one is unanswered.
    pub fn unanswered(&self) -> Option<&Steer> {
        let current = self.current.as_ref().filter(|g| g.exited.is_none())?;
        self.steers
            .iter()
            .rev()
            .find(|s| s.delivered_to.is_some())
            .filter(|s| {
                s.delivered_to.as_deref() == Some(current.id.as_str()) && s.answered_at.is_none()
            })
    }

    /// Why the current generation cannot work, while it cannot.
    pub fn health(&self) -> Option<&Unhealthy> {
        let current = self.current.as_ref()?;
        self.unhealthy
            .as_ref()
            .filter(|u| u.generation == current.id)
    }

    fn apply(&mut self, event: SupervisorEvent, at: SystemTime, seq: Seq) {
        match event {
            SupervisorEvent::Assigned { assignment } => self.assignment = assignment,
            SupervisorEvent::Worktree { worktree } => {
                self.worktree = Some(worktree);
                self.released = None;
            }
            SupervisorEvent::Launched {
                generation,
                session,
                backend,
                cause,
                dir,
                harness,
                model,
                effort,
            } => {
                self.unhealthy = None;
                self.recoveries = match cause {
                    Cause::Recover => self.recoveries + 1,
                    Cause::Spawn | Cause::Relaunch => 0,
                };
                self.current = Some(Generation {
                    id: generation,
                    session,
                    backend,
                    cause,
                    dir,
                    harness,
                    model,
                    effort,
                    exited: None,
                    started_at: Some(at),
                });
            }
            SupervisorEvent::Steer { id, text } => {
                if !self.steers.iter().any(|s| s.id == id) {
                    let mut s = Steer::new(id, text);
                    s.sent_at = Some(at);
                    self.steers.push(s);
                }
            }
            SupervisorEvent::Delivered { id, generation } => {
                if let Some(s) = self.steers.iter_mut().find(|s| s.id == id) {
                    if s.delivered_to.is_none() {
                        s.delivered_to = Some(generation);
                        s.delivered_at = Some(at);
                        s.delivered_seq = Some(seq);
                    }
                }
            }
            SupervisorEvent::Unhealthy { generation, reason } => {
                if self.current.as_ref().is_some_and(|g| g.id == generation) {
                    let since = match &self.unhealthy {
                        Some(u) if u.generation == generation => u.since,
                        _ => at,
                    };
                    self.unhealthy = Some(Unhealthy {
                        generation,
                        reason,
                        since,
                    });
                }
            }
            SupervisorEvent::Healthy { generation } => {
                if self
                    .unhealthy
                    .as_ref()
                    .is_some_and(|u| u.generation == generation)
                {
                    self.unhealthy = None;
                }
            }
            SupervisorEvent::Exited { generation, code } => {
                if let Some(g) = self.current.as_mut().filter(|g| g.id == generation) {
                    g.exited = Some(code);
                }
            }
            SupervisorEvent::Released { outcome } => self.released = Some(outcome),
            SupervisorEvent::Stale { .. } | SupervisorEvent::Handled { .. } => {}
        }
    }
}

#[derive(Default)]
struct Inner {
    applied: Seq,
    /// Worker messages handled through this position.
    handled: Seq,
    workers: BTreeMap<(ProjectId, TaskId), Worker>,
}

/// Every supervised task, rebuilt from the log. Cheap to clone.
#[derive(Clone)]
pub struct Fleet {
    inner: Arc<Mutex<Inner>>,
    manifests: Arc<ManifestRegistry>,
}

impl Fleet {
    /// An empty fleet; `manifests` supplies each task's hook mapping for
    /// the worker protocol.
    pub fn new(manifests: Arc<ManifestRegistry>) -> Self {
        Self {
            inner: Arc::default(),
            manifests,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn get(&self, project: &ProjectId, task: &TaskId) -> Option<Worker> {
        self.lock()
            .workers
            .get(&(project.clone(), task.clone()))
            .cloned()
    }

    /// Every task, ordered by project then task.
    pub fn all(&self) -> Vec<Worker> {
        self.lock().workers.values().cloned().collect()
    }

    /// Worker messages up to here have been turned into transitions.
    pub fn handled_through(&self) -> Seq {
        self.lock().handled
    }

    /// The task by id alone. Task ids are unique across Projects in
    /// practice; if two collide, the one with a live generation wins.
    pub fn by_task(&self, task: &TaskId) -> Option<Worker> {
        let inner = self.lock();
        let mut found: Vec<&Worker> = inner.workers.values().filter(|w| &w.task == task).collect();
        found.sort_by_key(|w| w.current.as_ref().is_some_and(|g| g.exited.is_none()));
        found.last().map(|w| (*w).clone())
    }
}

#[async_trait]
impl ReadModel for Fleet {
    async fn applied_through(&self) -> Result<Seq> {
        Ok(self.lock().applied)
    }

    async fn apply(&self, event: &Event) -> Result<()> {
        let mut inner = self.lock();
        if event.seq <= inner.applied {
            return Ok(());
        }
        inner.applied = event.seq;
        if is_worker_message(event) {
            // A worker reacting to its steering messages.
            if let Ok(env) = event.decode::<WorkerEnvelope>() {
                let at = SystemTime::from(event.ts);
                let key = (event.project.clone(), env.task.clone());
                if let Some(w) = inner.workers.get_mut(&key) {
                    for s in &mut w.steers {
                        s.acknowledge(&env.generation, event.seq, at, &env.message);
                    }
                }
            }
            return Ok(());
        }
        if event.kind.prefix() != PREFIX {
            return Ok(());
        }
        // The log is the truth: an event this version cannot read is
        // skipped rather than stopping replay.
        let Ok(e) = event.decode::<SupervisorEvent>() else {
            tracing::warn!(seq = event.seq.0, kind = %event.kind.0, "unreadable supervisor event");
            return Ok(());
        };
        if let SupervisorEvent::Handled { through } = e {
            inner.handled = inner.handled.max(through);
            return Ok(());
        }
        let Some(task) = event.task.clone() else {
            return Ok(());
        };
        let key = (event.project.clone(), task.clone());
        match (inner.workers.get_mut(&key), e) {
            (Some(w), e) => {
                let at = SystemTime::from(event.ts);
                if matches!(e, SupervisorEvent::Worktree { .. }) {
                    w.worktree_at = Some(at);
                }
                w.apply(e, at, event.seq)
            }
            (None, SupervisorEvent::Assigned { assignment }) => {
                inner.workers.insert(
                    key,
                    Worker {
                        project: event.project.clone(),
                        task,
                        assignment,
                        worktree: None,
                        worktree_at: None,
                        current: None,
                        recoveries: 0,
                        steers: Vec::new(),
                        unhealthy: None,
                        released: None,
                    },
                );
            }
            // Anything before the assignment has no task to attach to.
            (None, _) => {}
        }
        Ok(())
    }

    async fn reset(&self) -> Result<()> {
        *self.lock() = Inner::default();
        Ok(())
    }
}

#[async_trait]
impl TaskDirectory for Fleet {
    async fn binding(&self, task: &TaskId) -> Result<Option<Binding>> {
        let Some(w) = self.by_task(task) else {
            return Ok(None);
        };
        let Some(g) = w.current else {
            return Ok(None);
        };
        let hook_events = self
            .manifests
            .resolve(&g.harness)
            .and_then(|m| m.hooks.as_ref())
            .map(|h| h.events.clone())
            .unwrap_or_default();
        Ok(Some(Binding {
            project: w.project,
            generation: g.id,
            hook_events,
        }))
    }
}

/// Whether `event` is one the supervisor turns into a task transition.
pub(crate) fn is_worker_message(event: &Event) -> bool {
    event.kind.as_str() == kinds::WORKER
}
