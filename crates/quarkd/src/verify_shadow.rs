//! Slice 2 in shadow mode: compares every merge and dispatch decision
//! firstmate records with what the native guard would have decided, and logs
//! each disagreement to the event log. See `quark_verify::shadow`.
//!
//! Runs only while `QUARK_ENGINE_SLICES` puts slice 2 in `shadow`; nothing
//! here acts on a Project.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use quark_core::{NewEvent, ProjectId, Result};
use quark_eventlog::SqliteEventLog;
use quark_verify::{CheckpointLog, ShadowVerifier};

use crate::store::Store;

/// The event log as the shadow's [`CheckpointLog`].
pub struct LogCheckpoints(pub SqliteEventLog);

#[async_trait]
impl CheckpointLog for LogCheckpoints {
    async fn checkpoint(&self, name: &str) -> Result<Option<String>> {
        self.0.checkpoint(name).await
    }

    async fn append_batch(
        &self,
        events: Vec<NewEvent>,
        checkpoint: Option<(String, String)>,
    ) -> Result<()> {
        self.0.append_batch(events, checkpoint).await.map(|_| ())
    }
}

pub struct VerifyShadow {
    store: Arc<Store>,
    shadow: ShadowVerifier,
}

impl VerifyShadow {
    pub fn new(store: Arc<Store>, shadow: ShadowVerifier) -> Self {
        Self { store, shadow }
    }

    /// One pass over every Project with a workspace.
    pub async fn check_all(&self) -> anyhow::Result<()> {
        let store = self.store.clone();
        let projects = tokio::task::spawn_blocking(move || store.list_projects()).await??;
        for project in projects {
            let Some(path) = project.workspace_path else {
                continue;
            };
            let id = ProjectId::new(project.id);
            match self.shadow.ingest(&id, &PathBuf::from(path)).await {
                Ok(r) if r.divergences > 0 => {
                    tracing::warn!(project = %id, decisions = r.decisions, divergences = r.divergences, "native verification disagrees with firstmate")
                }
                Ok(r) if r.decisions > 0 => {
                    tracing::debug!(project = %id, decisions = r.decisions, "verification shadow")
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(project = %id, error = %e, "verification shadow failed"),
            }
        }
        Ok(())
    }

    pub async fn run(self, interval: Duration) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if let Err(e) = self.check_all().await {
                tracing::error!(error = %e, "verification shadow pass failed");
            }
        }
    }
}
