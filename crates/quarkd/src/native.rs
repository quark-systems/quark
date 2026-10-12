//! The native supervisor (slice 4), run beside the bash engine.
//!
//! Until slice 4 switches on, firstmate still spawns and steers every
//! worker, so the native supervisor starts only when [`ENV`] is `1`. It
//! then:
//!
//! - starts the PTY supervisor (`quark-ptyd` from [`PTYD_ENV`], else this
//!   binary as `quarkd ptyd`) on
//!   `run/ptyd.sock`, or reuses the one already listening, so worker
//!   sessions outlive a daemon crash;
//! - builds a [`Supervisor`] over the event log, treehouse worktrees
//!   (shadowed by the native pool with `QUARK_NATIVE_WORKTREES=1`), the
//!   harness manifests in `<home>/harnesses`, and the host's sandbox;
//! - adopts the sessions that survived the last daemon and supervises on a
//!   timer, resuming a lost worker's conversation when its harness logged
//!   one ([`Transcripts`]);
//! - serves the worker protocol (MCP and hooks) under [`WORKER_PREFIX`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use quark_core::harness::HarnessManifest;
use quark_core::{EventLog, WorktreeProvider};
use quark_harness::ManifestRegistry;
use quark_isolation::HostIsolation;
use quark_sessions::pty::PtyClient;
use quark_supervisor::{Conversations, Supervisor, WorkerUrls};
use quark_transcript::{locate, SessionFormat, SessionRoots};
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

/// The PTY supervisor to start: the program and the arguments before
/// `--socket <path>`. [`PTYD_ENV`] names a `quark-ptyd` binary; otherwise
/// this binary runs it as `quarkd ptyd`, so `cargo run -p quarkd` needs no
/// second binary built.
pub fn ptyd_command() -> (PathBuf, Vec<&'static str>) {
    if let Some(p) = std::env::var_os(PTYD_ENV) {
        return (PathBuf::from(p), Vec::new());
    }
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("quarkd"));
    (exe, vec!["ptyd"])
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
        let (ptyd, args) = ptyd_command();
        let sessions = PtyClient::start_with(&socket, &ptyd, &args)
            .await
            .with_context(|| format!("starting the PTY supervisor ({})", ptyd.display()))?;
        let pool = crate::native_worktrees::pool(log.clone(), host.clone()).await;
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
            .await?
            .with_conversations(Arc::new(Transcripts)),
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

/// Finds a worker's conversation in its harness's own session logs, as the
/// harness would to continue it: the manifest's transcript format, under the
/// account's config directory when the worker runs on one, else the
/// harness's default.
pub struct Transcripts;

impl Conversations for Transcripts {
    fn exists(&self, m: &HarnessManifest, cwd: &Path, env: &BTreeMap<String, String>) -> bool {
        let Some(format) = m
            .transcript
            .as_ref()
            .and_then(|t| session_format(&t.format))
        else {
            return false;
        };
        let mut roots = SessionRoots::from_env();
        let account = m
            .account
            .as_ref()
            .and_then(|a| a.env.as_ref())
            .and_then(|var| env.get(var));
        if let Some(dir) = account {
            let list = match format {
                SessionFormat::Claude => &mut roots.claude,
                SessionFormat::Codex => &mut roots.codex,
                SessionFormat::Pi => &mut roots.pi,
            };
            list.insert(0, PathBuf::from(dir));
        }
        locate(format, cwd, &roots).is_some()
    }
}

/// The session log format of a manifest's `transcript.format`.
fn session_format(format: &str) -> Option<SessionFormat> {
    match format {
        "claude-jsonl" => Some(SessionFormat::Claude),
        "codex-rollout" => Some(SessionFormat::Codex),
        "pi-session" => Some(SessionFormat::Pi),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_workers_conversation_under_its_account() {
        let dir = tempfile::tempdir().unwrap();
        let claude = quark_harness::builtin()
            .iter()
            .find(|m| m.id == "claude-code")
            .unwrap();
        let worktree = dir.path().join("wt");
        let account = dir.path().join("account");
        let env = BTreeMap::from([(
            "CLAUDE_CONFIG_DIR".to_string(),
            account.to_string_lossy().into_owned(),
        )]);
        assert!(!Transcripts.exists(claude, &worktree, &env));

        let slug: String = worktree
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let logs = account.join("projects").join(slug);
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("s1.jsonl"), "{}\n").unwrap();
        assert!(Transcripts.exists(claude, &worktree, &env));

        // A harness with no transcript format has nothing to continue.
        let mut none = (**claude).clone();
        none.transcript = None;
        assert!(!Transcripts.exists(&none, &worktree, &env));
    }

    #[test]
    fn quarkd_runs_the_pty_supervisor_itself_by_default() {
        if std::env::var_os(PTYD_ENV).is_none() {
            let (program, args) = ptyd_command();
            assert_eq!(program, std::env::current_exe().unwrap());
            assert_eq!(args, ["ptyd"]);
        }
    }
}
