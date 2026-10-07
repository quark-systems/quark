//! The sub-coordinators as the log tells them: a read model folded from
//! `subcoordinator.*` events.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use quark_core::host::Health;
use quark_core::session::SessionId;
use quark_core::worker::WorkerMessage;
use quark_core::{Event, ProjectId, ReadModel, Result, Seq};

use crate::events::{Cause, Message, MessageKind, SubEvent, PREFIX};
use crate::model::{Profile, Registration};

/// One agent generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generation {
    pub id: String,
    pub session: SessionId,
    pub cause: Cause,
    pub profile: Profile,
    /// `Some(code)` once its session ended.
    pub exited: Option<Option<i32>>,
    /// Whether the engine ended it on request; an unexpected exit is
    /// recovered, a requested one is not.
    pub stopped: bool,
}

/// A message to the sub-coordinator and how far it got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub message: Message,
    pub delivered: bool,
    pub acknowledged: bool,
}

/// A line from the sub-coordinator's parent channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub seq: Seq,
    pub offset: u64,
    pub line: String,
    pub message: Option<WorkerMessage>,
}

/// One sub-coordinator.
#[derive(Debug, Clone, PartialEq)]
pub struct SubCoordinator {
    pub registration: Registration,
    pub seeded: bool,
    /// Digest of the inherited configuration last written.
    pub inherited: Option<String>,
    pub current: Option<Generation>,
    /// Recoveries since the last launch a person asked for.
    pub recoveries: u32,
    /// Last health recorded; `None` before the first check.
    pub health: Option<Health>,
    pub outgoing: Vec<Outgoing>,
    /// Bytes of the parent channel already recorded.
    pub channel: u64,
    pub reports: Vec<Report>,
    /// Questions it asked its parent that no answer has closed, by key.
    pub decisions: BTreeMap<String, String>,
    pub retired: Option<String>,
}

impl SubCoordinator {
    pub fn id(&self) -> &ProjectId {
        &self.registration.id
    }

    pub fn is_live(&self) -> bool {
        self.retired.is_none()
    }

    /// Whether its agent should be running: launched and not stopped.
    pub fn is_running(&self) -> bool {
        self.current.as_ref().is_some_and(|g| g.exited.is_none())
    }

    pub fn undelivered(&self) -> impl Iterator<Item = &Outgoing> {
        self.outgoing.iter().filter(|o| !o.delivered)
    }

    /// Handoffs its agent has not taken yet.
    pub fn pending_handoffs(&self) -> impl Iterator<Item = &Outgoing> {
        self.outgoing
            .iter()
            .filter(|o| o.message.kind == MessageKind::Handoff && !o.acknowledged)
    }

    fn apply(&mut self, seq: Seq, e: SubEvent) {
        match e {
            SubEvent::Registered { registration } => {
                *self = SubCoordinator::new(registration);
            }
            SubEvent::Seeded => self.seeded = true,
            SubEvent::Inherited { digest, .. } => self.inherited = Some(digest),
            SubEvent::Launched {
                generation,
                session,
                cause,
                profile,
            } => {
                self.recoveries = match cause {
                    Cause::Recover => self.recoveries + 1,
                    Cause::Launch | Cause::Relaunch => 0,
                };
                self.current = Some(Generation {
                    id: generation,
                    session,
                    cause,
                    profile,
                    exited: None,
                    stopped: false,
                });
            }
            SubEvent::Exited {
                generation,
                code,
                stopped,
            } => {
                if let Some(g) = self.current.as_mut().filter(|g| g.id == generation) {
                    g.exited.get_or_insert(code);
                    g.stopped |= stopped;
                }
            }
            SubEvent::Health { health } => self.health = Some(health),
            SubEvent::Sent { message } => {
                if let Some(key) = &message.answers {
                    self.decisions.remove(key);
                }
                if !self.outgoing.iter().any(|o| o.message.id == message.id) {
                    self.outgoing.push(Outgoing {
                        message,
                        delivered: false,
                        acknowledged: false,
                    });
                }
            }
            SubEvent::Delivered { id } => {
                if let Some(o) = self.outgoing.iter_mut().find(|o| o.message.id == id) {
                    o.delivered = true;
                }
            }
            SubEvent::Acknowledged { id } => {
                if let Some(o) = self.outgoing.iter_mut().find(|o| o.message.id == id) {
                    o.delivered = true;
                    o.acknowledged = true;
                }
            }
            SubEvent::Report {
                offset,
                end,
                line,
                message,
            } => {
                // A replayed or re-read line is already behind the cursor.
                if offset >= self.channel {
                    self.channel = end;
                    if let Some(WorkerMessage::Ask { key, question }) = &message {
                        self.decisions.insert(key.clone(), question.clone());
                    }
                    self.reports.push(Report {
                        seq,
                        offset,
                        line,
                        message,
                    });
                }
            }
            SubEvent::Retired { reason, .. } => {
                self.retired = Some(reason);
                if let Some(g) = self.current.as_mut() {
                    g.exited.get_or_insert(None);
                    g.stopped = true;
                }
            }
        }
    }

    fn new(registration: Registration) -> Self {
        Self {
            registration,
            seeded: false,
            inherited: None,
            current: None,
            recoveries: 0,
            health: None,
            outgoing: Vec::new(),
            channel: 0,
            reports: Vec::new(),
            decisions: BTreeMap::new(),
            retired: None,
        }
    }
}

#[derive(Default)]
struct Inner {
    applied: Seq,
    subs: BTreeMap<ProjectId, SubCoordinator>,
}

/// Every sub-coordinator, rebuilt from the log.
#[derive(Clone, Default)]
pub struct Registry {
    inner: Arc<Mutex<Inner>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn get(&self, id: &ProjectId) -> Option<SubCoordinator> {
        self.lock().subs.get(id).cloned()
    }

    /// Every sub-coordinator, retired ones included, by id.
    pub fn all(&self) -> Vec<SubCoordinator> {
        self.lock().subs.values().cloned().collect()
    }

    pub fn live(&self) -> Vec<SubCoordinator> {
        self.lock()
            .subs
            .values()
            .filter(|s| s.is_live())
            .cloned()
            .collect()
    }
}

#[async_trait]
impl ReadModel for Registry {
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
        let Ok(e) = event.decode::<SubEvent>() else {
            tracing::warn!(seq = event.seq.0, kind = %event.kind.0, "unreadable sub-coordinator event");
            return Ok(());
        };
        match (inner.subs.get_mut(&event.project), e) {
            (Some(s), e) => s.apply(event.seq, e),
            (None, SubEvent::Registered { registration }) => {
                inner
                    .subs
                    .insert(event.project.clone(), SubCoordinator::new(registration));
            }
            // Nothing to attach to before the registration.
            (None, _) => {}
        }
        Ok(())
    }

    async fn reset(&self) -> Result<()> {
        *self.lock() = Inner::default();
        Ok(())
    }
}
