//! Triggers, channels and the away policy (slice 7), shadowed beside the
//! bash engine.
//!
//! Slice 7 switches on after slices 1 to 6, so firstmate still owns every
//! Project's inbox, away posture and watches. When [`ENV`] is `1`, quarkd
//! runs the native engine in shadow mode on a timer:
//!
//! - mirrors each Project's firstmate inbox notes and away posture into
//!   the event log;
//! - compares its route for each firstmate status line with firstmate's
//!   away classifier, recording disagreements as `shadow.divergence`;
//! - evaluates trigger rules and records what would have fired.
//!
//! It never wakes, notifies or steers anyone. See
//! `docs/engine/triggers.md`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use quark_core::{EventLog, ProjectId};
use quark_triggers::{Engine, Mode, NoEffects, Processes};

use crate::store::Store;

/// Set to `1` to run the slice 7 engine in shadow mode.
pub const ENV: &str = "QUARK_NATIVE_TRIGGERS";

pub fn enabled() -> bool {
    std::env::var(ENV).is_ok_and(|v| v == "1")
}

/// The shadow engine and the Projects it mirrors.
pub struct ShadowTriggers {
    engine: Engine,
    store: Arc<Store>,
}

impl ShadowTriggers {
    pub async fn open(store: Arc<Store>, log: Arc<dyn EventLog>) -> anyhow::Result<Self> {
        let engine = Engine::open(
            log,
            crate::event_ingest::host(),
            Mode::Shadow,
            Arc::new(NoEffects),
            Arc::new(Processes),
        )
        .await?;
        Ok(Self { engine, store })
    }

    /// Mirror every Project's firstmate home, then evaluate.
    pub async fn pass(&self) -> anyhow::Result<()> {
        let store = self.store.clone();
        let projects = tokio::task::spawn_blocking(move || store.list_projects()).await??;
        for project in projects {
            let Some(path) = project.workspace_path else {
                continue;
            };
            let id = ProjectId::new(project.id);
            if let Err(e) = self
                .engine
                .mirror_firstmate(&id, &PathBuf::from(path))
                .await
            {
                tracing::warn!(project = %id, error = %e, "slice 7 mirror failed");
            }
        }
        let r = self.engine.tick(time::OffsetDateTime::now_utc()).await?;
        if r.diverged > 0 {
            tracing::info!(
                compared = r.compared,
                diverged = r.diverged,
                "slice 7 shadow disagreed with firstmate"
            );
        }
        Ok(())
    }

    pub async fn run(self, interval: Duration) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if let Err(e) = self.pass().await {
                tracing::error!(error = %format!("{e:#}"), "slice 7 shadow pass failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use quark_core::Seq;
    use quark_systems::CreateProject;

    use super::*;

    #[tokio::test]
    async fn a_pass_mirrors_each_project_and_acts_on_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(home.join("state/inbox")).unwrap();
        std::fs::write(
            home.join("state/inbox/1-a.note"),
            "id=1-a\nat=2026-10-07T01:00:00Z\nsource=cli\n--\ncheck the nightly\n",
        )
        .unwrap();
        std::fs::write(home.join("state/.afk"), "quiet\n").unwrap();
        let store = Arc::new(Store::open_in_memory().unwrap());
        let project = store
            .create_project(CreateProject {
                name: "Quark".into(),
                goal: None,
                workspace_path: Some(home.display().to_string()),
                repos: vec![],
                agent_config: None,
                dispatch_preset: None,
                delivery: None,
            })
            .unwrap();
        let log = quark_eventlog::SqliteEventLog::open(dir.path().join("events.db")).unwrap();
        let shadow = ShadowTriggers::open(store, Arc::new(log.clone()))
            .await
            .unwrap();
        shadow.pass().await.unwrap();
        shadow.pass().await.unwrap();
        let kinds: Vec<String> = log
            .read(Seq::ZERO, 100)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.project.as_str() == project.id)
            .map(|e| e.kind.0)
            .collect();
        assert_eq!(kinds, ["channel.received", "away.posture"]);
    }
}
