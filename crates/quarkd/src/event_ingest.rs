//! Mirrors every Project's firstmate home into the native event log
//! (`<home>/events.db`) while slice 1 still runs on bash, so the log holds
//! today's events from day one. See `quark_eventlog::firstmate`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use quark_core::{HostId, ProjectId};
use quark_eventlog::FirstmateBridge;

use crate::store::Store;

/// This machine until host identity lands with the host registry.
pub const LOCAL_HOST: &str = "local";

pub struct EventIngest {
    store: Arc<Store>,
    bridge: FirstmateBridge,
}

impl EventIngest {
    pub fn new(store: Arc<Store>, bridge: FirstmateBridge) -> Self {
        Self { store, bridge }
    }

    /// One pass over every Project with a workspace.
    pub async fn ingest_all(&self) -> anyhow::Result<()> {
        let store = self.store.clone();
        let projects = tokio::task::spawn_blocking(move || store.list_projects()).await??;
        for project in projects {
            let Some(path) = project.workspace_path else {
                continue;
            };
            let id = ProjectId::new(project.id);
            match self.bridge.ingest(&id, &PathBuf::from(path)).await {
                Ok(r) if r.status_lines + r.spawns > 0 => {
                    tracing::debug!(project = %id, status_lines = r.status_lines, spawns = r.spawns, "event log ingest")
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(project = %id, error = %e, "event log ingest failed"),
            }
        }
        Ok(())
    }

    pub async fn run(self, interval: Duration) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if let Err(e) = self.ingest_all().await {
                tracing::error!(error = %e, "event log ingest pass failed");
            }
        }
    }
}

pub fn host() -> HostId {
    HostId::from(LOCAL_HOST)
}
