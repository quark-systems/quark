//! The native supervisor (slice 4), run beside the bash engine.
//!
//! Until slice 4 switches on, firstmate still spawns and steers every
//! worker, so the native supervisor starts only when [`ENV`] is `1`. It
//! then:
//!
//! - starts `quark-ptyd` (from [`PTYD_ENV`], else next to this binary) on
//!   `run/ptyd.sock`, or reuses the one already listening, so worker
//!   sessions outlive a daemon crash;
//! - builds a [`Supervisor`] over the event log, treehouse worktrees
//!   (shadowed by the native pool with `QUARK_NATIVE_WORKTREES=1`), the
//!   harness manifests in `<home>/harnesses`, and the host's sandbox;
//! - adopts the sessions that survived the last daemon and supervises on a
//!   timer;
//! - serves the worker protocol (MCP and hooks) under [`WORKER_PREFIX`].

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use quark_core::{EventLog, WorktreeProvider};
use quark_harness::ManifestRegistry;
use quark_isolation::HostIsolation;
use quark_sessions::pty::PtyClient;
use quark_supervisor::{Supervisor, WorkerUrls};
use quark_worktree::TreehouseProvider;

use crate::config::Config;

/// Set to `1` to run the native supervisor.
pub const ENV: &str = "QUARK_NATIVE_SUPERVISOR";
/// Path of the `quark-ptyd` binary, when it is not next to `quarkd`.
pub const PTYD_ENV: &str = "QUARK_PTYD";
/// Where the worker protocol's routes are nested.
pub const WORKER_PREFIX: &str = "/v1/worker";
/// How often the supervisor looks at its workers.
pub const TICK: Duration = Duration::from_secs(2);

pub fn enabled() -> bool {
    std::env::var(ENV).is_ok_and(|v| v == "1")
}

/// The `quark-ptyd` binary to start.
pub fn ptyd_path() -> PathBuf {
    if let Some(p) = std::env::var_os(PTYD_ENV) {
        return PathBuf::from(p);
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join("quark-ptyd")))
        .unwrap_or_else(|| PathBuf::from("quark-ptyd"))
}

/// A running native supervisor.
pub struct NativeSupervision {
    pub supervisor: Arc<Supervisor>,
    task: tokio::task::JoinHandle<()>,
}

impl NativeSupervision {
    /// Start everything, recover, and begin supervising.
    pub async fn start(config: &Config, log: Arc<dyn EventLog>) -> anyhow::Result<Self> {
        let host = crate::event_ingest::host();
        let socket = config.run_dir().join("ptyd.sock");
        let ptyd = ptyd_path();
        let sessions = PtyClient::start(&socket, &ptyd)
            .await
            .with_context(|| format!("starting {}", ptyd.display()))?;
        let pool = crate::native_worktrees::pool(log.clone(), host.clone());
        let worktrees: Arc<dyn WorktreeProvider> =
            Arc::new(TreehouseProvider::new(pool).with_events(log.clone(), host.clone()));
        let (manifests, errors) = ManifestRegistry::load(&config.home.join("harnesses"));
        for e in errors {
            tracing::warn!(path = %e.path.display(), error = %e.error, "harness manifest skipped");
        }
        let mut sup_config = quark_supervisor::Config::new(host, config.home.join("workers"));
        sup_config.worker_urls = Some(WorkerUrls {
            base: format!("http://{}{WORKER_PREFIX}", config.listen),
        });
        let supervisor = Arc::new(
            Supervisor::open(
                log,
                Arc::new(manifests),
                worktrees,
                Arc::new(sessions),
                Arc::new(HostIsolation::detect().await),
                sup_config,
            )
            .await?,
        );
        let orphans = supervisor.recover().await?;
        tracing::info!(socket = %socket.display(), orphans, "native supervisor running");
        let task = tokio::spawn(supervisor.clone().run(TICK));
        Ok(Self { supervisor, task })
    }

    /// The worker protocol's routes, to nest under [`WORKER_PREFIX`].
    pub fn worker_router(&self) -> axum::Router {
        quark_worker::router(self.supervisor.recorder())
    }

    /// Stop supervising. Worker sessions keep running in `quark-ptyd`.
    pub fn stop(self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ptyd_sits_next_to_quarkd_by_default() {
        if std::env::var_os(PTYD_ENV).is_none() {
            assert_eq!(ptyd_path().file_name().unwrap(), "quark-ptyd");
        }
    }
}
