//! [`EventHosts`], the [`HostRegistry`] kept in the event log.
//!
//! Registering a host and every health change is a [`HOST`] event, so the
//! host list survives restarts and the dashboard can show when a host went
//! unhealthy and came back.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use quark_core::host::{Health, Host, HostRegistry};
use quark_core::{CoreError, EventLog, HostId, NewEvent, ProjectId, Result, Seq};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

/// Event kind of a host registration or health change. Payload:
/// [`HostChange`]. Recorded under the engine project.
pub const HOST: &str = "host.change";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostChange {
    /// A host was added, or its description replaced.
    Registered {
        host: Host,
    },
    Health {
        host: HostId,
        health: Health,
    },
}

#[derive(Default)]
struct State {
    cursor: Seq,
    hosts: BTreeMap<HostId, Host>,
}

/// Every host Quark knows, folded from the log.
pub struct EventHosts {
    log: Arc<dyn EventLog>,
    /// The host recording the events.
    me: HostId,
    state: Mutex<State>,
}

impl EventHosts {
    pub fn new(log: Arc<dyn EventLog>, me: HostId) -> Self {
        Self {
            log,
            me,
            state: Mutex::new(State::default()),
        }
    }

    async fn refresh(&self, s: &mut State) -> Result<()> {
        loop {
            let batch = self.log.read(s.cursor, 1000).await?;
            if batch.is_empty() {
                return Ok(());
            }
            for e in batch {
                s.cursor = e.seq;
                if e.kind.as_str() != HOST {
                    continue;
                }
                match e.decode::<HostChange>() {
                    Ok(HostChange::Registered { host }) => {
                        s.hosts.insert(host.id.clone(), host);
                    }
                    Ok(HostChange::Health { host, health }) => {
                        if let Some(h) = s.hosts.get_mut(&host) {
                            h.health = health;
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "unreadable host event"),
                }
            }
        }
    }

    async fn append(&self, s: &mut State, change: &HostChange) -> Result<()> {
        let e = NewEvent::typed(self.me.clone(), ProjectId::engine(), None, HOST, change)?;
        self.log.append(e).await?;
        self.refresh(s).await
    }
}

#[async_trait]
impl HostRegistry for EventHosts {
    async fn hosts(&self) -> Result<Vec<Host>> {
        let mut s = self.state.lock().await;
        self.refresh(&mut s).await?;
        Ok(s.hosts.values().cloned().collect())
    }

    /// Records the host unless it is already registered exactly so.
    async fn register(&self, host: Host) -> Result<()> {
        let mut s = self.state.lock().await;
        self.refresh(&mut s).await?;
        if s.hosts.get(&host.id) == Some(&host) {
            return Ok(());
        }
        self.append(&mut s, &HostChange::Registered { host }).await
    }

    /// Records a health change; the same health again records nothing.
    async fn set_health(&self, id: &HostId, health: Health) -> Result<()> {
        let mut s = self.state.lock().await;
        self.refresh(&mut s).await?;
        let h = s
            .hosts
            .get(id)
            .ok_or_else(|| CoreError::NotFound(format!("host {id}")))?;
        if h.health == health {
            return Ok(());
        }
        self.append(
            &mut s,
            &HostChange::Health {
                host: id.clone(),
                health,
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quark_core::fake::MemoryEventLog;
    use quark_core::host::{Capacity, Platform, RuntimeKind};

    fn host(id: &str) -> Host {
        Host {
            id: id.into(),
            name: id.into(),
            runtime: RuntimeKind::Local,
            platform: Platform {
                os: "linux".into(),
                arch: "x86_64".into(),
            },
            capacity: Capacity::default(),
            health: Health::Healthy,
            projects: Vec::new(),
            tasks: Vec::new(),
        }
    }

    #[tokio::test]
    async fn survives_a_restart_and_records_changes_once() {
        let log = MemoryEventLog::new();
        let r = EventHosts::new(Arc::new(log.clone()), "me".into());
        r.register(host("a")).await.unwrap();
        r.register(host("a")).await.unwrap();
        let sick = Health::Unreachable {
            reason: "ssh".into(),
        };
        r.set_health(&"a".into(), sick.clone()).await.unwrap();
        r.set_health(&"a".into(), sick.clone()).await.unwrap();
        assert!(r.set_health(&"b".into(), Health::Healthy).await.is_err());
        assert_eq!(log.events().len(), 2);

        let again = EventHosts::new(Arc::new(log), "me".into());
        let hosts = again.hosts().await.unwrap();
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].health, sick);
    }
}
