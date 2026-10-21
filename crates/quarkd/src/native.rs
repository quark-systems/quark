//! The native supervisor (slice 4), which the native engine runs on.
//!
//! With `--engine native`, quarkd:
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

/// Path of the `quark-ptyd` binary, when it is not next to `quarkd`.
pub const PTYD_ENV: &str = "QUARK_PTYD";
/// Where the worker protocol's routes are nested.
pub const WORKER_PREFIX: &str = "/v1/worker";
/// How often the supervisor looks at its workers.
pub const TICK: Duration = Duration::from_secs(2);

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
        let host = crate::host();
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
            .with_conversations(Arc::new(Transcripts::from_env()))
            .with_secrets(Arc::new(crate::secrets::Keychain::default())),
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
/// harness's default. IBM Bob keeps its conversations in a database
/// instead, `~/.bob/db/bob.db`; Kiro CLI one directory per session under
/// `~/.kiro/sessions`.
pub struct Transcripts {
    pub bob_db: PathBuf,
    /// Each harness's default session roots.
    pub roots: SessionRoots,
}

impl Transcripts {
    /// Bob's database under `$HOME`.
    pub fn from_env() -> Self {
        Self {
            bob_db: std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".bob").join("db").join("bob.db"))
                .unwrap_or_default(),
            roots: SessionRoots::from_env(),
        }
    }
}

impl Conversations for Transcripts {
    fn exists(&self, m: &HarnessManifest, cwd: &Path, env: &BTreeMap<String, String>) -> bool {
        if quark_harness::engine_name(m) == "bob" {
            return quark_dispatch::local::bob::has_task(&self.bob_db, cwd);
        }
        // Kiro CLI has no account directory, so no `transcript` locator: its
        // sessions are found under `$KIRO_HOME` by working directory.
        let Some(format) = m
            .transcript
            .as_ref()
            .and_then(|t| session_format(&t.format))
            .or_else(|| (quark_harness::engine_name(m) == "kiro").then_some(SessionFormat::Kiro))
        else {
            return false;
        };
        let mut roots = self.roots.clone();
        let account = m
            .account
            .as_ref()
            .and_then(|a| a.env.as_ref())
            .and_then(|var| env.get(var));
        if let Some(dir) = account {
            roots.for_format_mut(format).insert(0, PathBuf::from(dir));
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
        let t = Transcripts {
            bob_db: dir.path().join("bob.db"),
            roots: SessionRoots::default(),
        };
        let env = BTreeMap::from([(
            "CLAUDE_CONFIG_DIR".to_string(),
            account.to_string_lossy().into_owned(),
        )]);
        assert!(!t.exists(claude, &worktree, &env));

        let slug: String = worktree
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let logs = account.join("projects").join(slug);
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("s1.jsonl"), "{}\n").unwrap();
        assert!(t.exists(claude, &worktree, &env));

        // A harness with no transcript format has nothing to continue.
        let mut none = (**claude).clone();
        none.transcript = None;
        assert!(!t.exists(&none, &worktree, &env));
    }

    #[test]
    fn finds_a_bob_task_in_the_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let bob = quark_harness::builtin()
            .iter()
            .find(|m| m.id == "bob")
            .unwrap();
        assert!(!bob.launch.resume.is_empty());
        let t = Transcripts {
            bob_db: dir.path().join("bob.db"),
            roots: SessionRoots::default(),
        };
        let wt = dir.path().join("wt");
        assert!(!t.exists(bob, &wt, &BTreeMap::new()));
        let con = rusqlite::Connection::open(&t.bob_db).unwrap();
        con.execute_batch(
            "CREATE TABLE tasks (id TEXT, parent_id TEXT, directory TEXT, task_type TEXT)",
        )
        .unwrap();
        con.execute(
            "INSERT INTO tasks VALUES ('t1', NULL, ?1, 'normal')",
            [wt.to_string_lossy()],
        )
        .unwrap();
        drop(con);
        assert!(t.exists(bob, &wt, &BTreeMap::new()));
    }

    #[test]
    fn quarkd_runs_the_pty_supervisor_itself_by_default() {
        if std::env::var_os(PTYD_ENV).is_none() {
            let (program, args) = ptyd_command();
            assert_eq!(program, std::env::current_exe().unwrap());
            assert_eq!(args, ["ptyd"]);
        }
    }

    #[test]
    fn finds_a_kiro_session_in_the_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let kiro = quark_harness::builtin()
            .iter()
            .find(|m| m.id == "kiro")
            .unwrap();
        assert!(!kiro.launch.resume.is_empty());
        let home = dir.path().join(".kiro");
        let t = Transcripts {
            bob_db: dir.path().join("bob.db"),
            roots: SessionRoots {
                kiro: vec![home.clone()],
                ..Default::default()
            },
        };
        let wt = dir.path().join("wt");
        assert!(!t.exists(kiro, &wt, &BTreeMap::new()));
        let sess = home.join("sessions").join("3f2a").join("sess_1");
        std::fs::create_dir_all(&sess).unwrap();
        std::fs::write(
            sess.join("session.json"),
            serde_json::json!({"id": "sess_1", "workspacePaths": [wt]}).to_string(),
        )
        .unwrap();
        std::fs::write(sess.join("messages.jsonl"), "").unwrap();
        assert!(t.exists(kiro, &wt, &BTreeMap::new()));
    }
}
