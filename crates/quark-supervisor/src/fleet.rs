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

use async_trait::async_trait;
use quark_core::event::kinds;
use quark_core::session::SessionId;
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
    pub current: Option<Generation>,
    /// Generations started by the supervisor itself since the last spawn
    /// or relaunch a caller asked for.
    pub recoveries: u32,
    pub steers: Vec<Steer>,
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

    fn apply(&mut self, event: SupervisorEvent) {
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
                });
            }
            SupervisorEvent::Steer { id, text } => {
                if !self.steers.iter().any(|s| s.id == id) {
                    self.steers.push(Steer {
                        id,
                        text,
                        delivered_to: None,
                    });
                }
            }
            SupervisorEvent::Delivered { id, generation } => {
                if let Some(s) = self.steers.iter_mut().find(|s| s.id == id) {
                    s.delivered_to.get_or_insert(generation);
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
            (Some(w), e) => w.apply(e),
            (None, SupervisorEvent::Assigned { assignment }) => {
                inner.workers.insert(
                    key,
                    Worker {
                        project: event.project.clone(),
                        task,
                        assignment,
                        worktree: None,
                        current: None,
                        recoveries: 0,
                        steers: Vec::new(),
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
