//! Coordinator recovery: every ready Project's coordinator comes back after
//! the tmux server it runs on is lost, or after a reboot.
//!
//! Coordinators run on the daemon's shared tmux server (see
//! [`crate::sessions`]). When that server dies (`tmux kill-server`, an OOM
//! kill, a tmux upgrade), or the machine restarts, every coordinator's
//! window goes with it while the engine's records of them stay. Each pass
//! here:
//!
//! 1. starts the server again when it is not running, so terminals and new
//!    Projects work without a daemon restart;
//! 2. reads each coordinator's recorded window from the engine and, for a
//!    ready Project whose window the server confidently does not have,
//!    starts the coordinator again under the account it already holds,
//!    resuming its latest conversation. When the engine cannot resume it,
//!    the coordinator starts fresh from its charter, which beats leaving the
//!    Project without one.
//!
//! A Project is retried at most once per [`RETRY_AFTER`], so a coordinator
//! that cannot start does not relaunch in a loop. A window that is listed
//! is never touched, whatever runs in it: only a missing window is proof
//! the agent is gone, so recovery never starts a second coordinator.
//! Worker windows are the coordinator's to recover once it is back.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use quark_systems::{Project, ProjectStatus};

use crate::accounts::{Accounts, Holder};
use crate::engine::{EngineAdapter, WorkspaceRef};
use crate::sessions::Sessions;
use crate::store::Store;

/// How often recovery looks for a lost server or coordinator.
pub const INTERVAL: Duration = Duration::from_secs(3);
/// How long a Project waits before its coordinator is started again after
/// an attempt.
pub const RETRY_AFTER: Duration = Duration::from_secs(60);

pub struct CoordinatorRecovery {
    store: Arc<Store>,
    engine: Arc<dyn EngineAdapter>,
    accounts: Arc<Accounts>,
    sessions: Sessions,
    command: PathBuf,
    attempts: Mutex<HashMap<String, Instant>>,
}

impl CoordinatorRecovery {
    pub fn new(
        store: Arc<Store>,
        engine: Arc<dyn EngineAdapter>,
        accounts: Arc<Accounts>,
        sessions: Sessions,
        command: PathBuf,
    ) -> Self {
        Self {
            store,
            engine,
            accounts,
            sessions,
            command,
            attempts: Mutex::new(HashMap::new()),
        }
    }

    pub async fn run(self, interval: Duration) {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            self.pass().await;
        }
    }

    /// One look: restart a lost server, then relaunch missing coordinators.
    pub async fn pass(&self) {
        let Ok(server) = self.sessions.server() else {
            return;
        };
        if !server.is_running().await {
            tracing::warn!("the tmux server is gone; starting it again");
            if let Err(e) = self.sessions.ensure_server().await {
                tracing::warn!(error = %e, "could not start the tmux server again");
                return;
            }
        }
        let store = self.store.clone();
        let projects = match tokio::task::spawn_blocking(move || store.list_projects()).await {
            Ok(Ok(p)) => p,
            res => {
                tracing::warn!(?res, "coordinator recovery could not list Projects");
                return;
            }
        };
        let ready: Vec<Project> = projects
            .into_iter()
            .filter(|p| {
                p.status == ProjectStatus::Ready
                    && p.workspace_path.is_some()
                    && p.agent_config.is_some()
            })
            .collect();
        if ready.is_empty() {
            return;
        }
        let recorded = match self.engine.coordinator_terminals(&self.command).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "coordinator recovery could not read coordinator windows");
                return;
            }
        };
        // A failed listing proves nothing, so nothing is relaunched on one.
        let listed: HashSet<String> = match server.list_panes().await {
            Ok(panes) => panes.into_iter().map(|p| p.target).collect(),
            Err(e) => {
                tracing::warn!(error = %e, "coordinator recovery could not list tmux windows");
                return;
            }
        };
        for project in ready {
            let Some(target) = recorded.get(&project.id) else {
                continue;
            };
            if listed.contains(target) || !self.due(&project.id) {
                continue;
            }
            self.relaunch(&project, target).await;
        }
    }

    /// Records an attempt for `project_id` unless one was made within
    /// [`RETRY_AFTER`].
    fn due(&self, project_id: &str) -> bool {
        let mut attempts = self.attempts.lock().unwrap();
        let now = Instant::now();
        if let Some(at) = attempts.get(project_id) {
            if now.duration_since(*at) < RETRY_AFTER {
                return false;
            }
        }
        attempts.insert(project_id.to_string(), now);
        true
    }

    async fn relaunch(&self, project: &Project, target: &str) {
        let (Some(agent), Some(root)) = (&project.agent_config, &project.workspace_path) else {
            return;
        };
        let ws = WorkspaceRef {
            project_id: project.id.clone(),
            root: PathBuf::from(root),
        };
        tracing::warn!(project = %project.id, %target, "coordinator window is gone; starting the coordinator again");
        // The lease the coordinator already holds, so it keeps its account.
        let env = match self
            .accounts
            .lease(&Holder::Coordinator(project.id.clone()), agent)
            .await
        {
            Ok(lease) => lease.map(|l| l.env).unwrap_or_default(),
            Err(e) => {
                tracing::warn!(project = %project.id, error = %e, "coordinator recovery could not choose an account");
                return;
            }
        };
        match self
            .engine
            .start_coordinator(&self.command, &ws, agent, &env, true)
            .await
        {
            Ok(()) => {
                tracing::info!(project = %project.id, "coordinator relaunched in its conversation");
                return;
            }
            Err(e) => {
                tracing::warn!(project = %project.id, error = %e, "coordinator could not resume; starting it fresh");
            }
        }
        match self
            .engine
            .start_coordinator(&self.command, &ws, agent, &env, false)
            .await
        {
            Ok(()) => tracing::info!(project = %project.id, "coordinator relaunched fresh"),
            Err(e) => {
                tracing::warn!(project = %project.id, error = %e, "coordinator relaunch failed")
            }
        }
    }
}
