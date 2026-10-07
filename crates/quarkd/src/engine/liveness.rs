//! Slice 4's session liveness check: firstmate's panes against the native
//! session backend.
//!
//! The native supervisor decides that a worker exited when its session
//! backend stops listing the session alive (`quark_supervisor`'s tick), and
//! then recovers it. Firstmate decides the same from its own pane probe
//! (`fm-crew-state.sh`: "backend target gone"). [`SessionLiveness`] reads
//! each of firstmate's tmux panes through the native
//! [`TmuxBackend`](quark_sessions::tmux::TmuxBackend) on the same shared
//! server and records a `session_liveness` divergence when the two
//! disagree about whether the pane is there: the native supervisor would
//! have relaunched a worker firstmate kept, or kept one firstmate lost.
//!
//! Only tasks whose liveness firstmate actually read are compared: a state
//! from the pane or the status log means the pane answered, and "backend
//! target gone" means it is missing. A task firstmate judged from a
//! validation run, or could not reach, is left out. So is "agent gone, pane
//! shell remains": firstmate types its agent into a shell, while a native
//! session runs the agent itself, so that pane has no native counterpart.

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use quark_core::session::SessionBackend;
use quark_sessions::tmux::server::Server;
use quark_sessions::tmux::TmuxBackend;
use serde_json::json;

use super::{EngineError, EngineTask, FleetSnapshot, WorkspaceRef};

/// Operation the divergences are recorded under.
pub const OPERATION: &str = "session_liveness";

/// What `fm-crew-state.sh` writes when the pane is missing.
const GONE: &str = "backend target gone:";
/// Its suffix when the pane is there but the agent in it has exited.
const AGENT_GONE: &str = "agent gone";

/// Firstmate's tmux panes, as the native session backend sees them.
pub struct SessionLiveness {
    server: Server,
    sessions: TmuxBackend,
}

impl SessionLiveness {
    /// Read through a native backend on `server`, the shared tmux server
    /// firstmate's panes run on.
    pub fn new(server: Server) -> Self {
        Self {
            sessions: TmuxBackend::new(server.clone()),
            server,
        }
    }

    /// The `session:window` targets and window ids of every window the
    /// native backend lists alive.
    async fn live(&self) -> Result<BTreeSet<String>, EngineError> {
        let alive: BTreeSet<String> = self
            .sessions
            .list()
            .await
            .map_err(|e| EngineError::Command(e.to_string()))?
            .into_iter()
            .filter(|s| s.alive)
            .map(|s| s.id.0)
            .collect();
        // A native session is a window, named by its first pane; firstmate
        // names it by `session:window`.
        let panes = self
            .server
            .list_panes()
            .await
            .map_err(|e| EngineError::Command(e.to_string()))?;
        Ok(panes
            .into_iter()
            .filter(|p| alive.contains(&p.pane_id))
            .flat_map(|p| {
                let session = p.target.split(':').next().unwrap_or_default().to_string();
                [p.target, format!("{session}:{}", p.window_id)]
            })
            .collect())
    }
}

#[async_trait]
impl super::shadow::SnapshotCheck for SessionLiveness {
    fn slice(&self) -> quark_core::Slice {
        quark_core::Slice::Supervision
    }

    fn operation(&self) -> &'static str {
        OPERATION
    }

    async fn diverges(
        &self,
        _ws: &WorkspaceRef,
        bash: &FleetSnapshot,
    ) -> Result<Option<(serde_json::Value, serde_json::Value)>, EngineError> {
        if !bash.tasks.iter().any(|t| firstmate_alive(t).is_some()) {
            return Ok(None);
        }
        Ok(liveness_view(bash, &self.live().await?))
    }
}

/// Whether firstmate read the task's pane as there (`Some(true)`) or
/// missing (`Some(false)`); `None` when it did not read it, or the task has
/// no local tmux pane.
pub fn firstmate_alive(t: &EngineTask) -> Option<bool> {
    let target = t.terminal.as_deref()?;
    // A window index is not stable enough to match.
    let window = target.rsplit(':').next()?;
    if window.is_empty() || window.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match t.state_source.as_deref() {
        Some("pane" | "status-log") => Some(true),
        Some("none") => {
            let note = t.state_note.as_deref()?;
            (note.starts_with(GONE) && !note.contains(AGENT_GONE)).then_some(false)
        }
        _ => None,
    }
}

/// The tasks whose liveness differs, with what each side says, given the
/// windows the native backend lists alive.
pub fn liveness_view(
    bash: &FleetSnapshot,
    live: &BTreeSet<String>,
) -> Option<(serde_json::Value, serde_json::Value)> {
    let mut ours = BTreeMap::new();
    let mut theirs = BTreeMap::new();
    for t in &bash.tasks {
        let Some(fm) = firstmate_alive(t) else {
            continue;
        };
        let target = t.terminal.as_deref().unwrap_or_default();
        let native = live.contains(target);
        if native != fm {
            theirs.insert(t.id.clone(), json!({"target": target, "alive": fm}));
            ours.insert(t.id.clone(), json!({"target": target, "alive": native}));
        }
    }
    (!ours.is_empty()).then(|| (json!(theirs), json!(ours)))
}

#[cfg(test)]
mod tests {
    use quark_sessions::tmux::server::WindowSpec;
    use quark_systems::TaskState;

    use super::super::shadow::SnapshotCheck;
    use super::*;

    fn task(id: &str, target: &str, source: &str, note: Option<&str>) -> EngineTask {
        EngineTask {
            id: id.into(),
            title: id.into(),
            kind: None,
            state: TaskState::Unknown,
            state_note: note.map(str::to_string),
            state_source: Some(source.into()),
            harness: None,
            pull_request_url: None,
            terminal: Some(target.into()),
            worktree: None,
        }
    }

    #[test]
    fn reads_firstmates_liveness_only_where_it_looked() {
        let gone = "backend target gone: fm:fm-a";
        assert_eq!(
            firstmate_alive(&task("a", "fm:fm-a", "pane", None)),
            Some(true)
        );
        assert_eq!(
            firstmate_alive(&task("a", "fm:fm-a", "status-log", None)),
            Some(true)
        );
        assert_eq!(
            firstmate_alive(&task("a", "fm:fm-a", "none", Some(gone))),
            Some(false)
        );
        let shell = "backend target gone: fm:fm-a (agent gone, pane shell remains)";
        assert_eq!(
            firstmate_alive(&task("a", "fm:fm-a", "none", Some(shell))),
            None
        );
        let unreachable = "backend unreachable (tmux endpoint state: unknown)";
        assert_eq!(
            firstmate_alive(&task("a", "fm:fm-a", "none", Some(unreachable))),
            None
        );
        assert_eq!(
            firstmate_alive(&task("a", "fm:fm-a", "run-step", None)),
            None
        );
        assert_eq!(firstmate_alive(&task("a", "fm:3", "pane", None)), None);
    }

    #[test]
    fn records_only_the_tasks_that_differ() {
        let bash = FleetSnapshot {
            tasks: vec![
                task("a", "fm:fm-a", "pane", None),
                task("b", "fm:fm-b", "none", Some("backend target gone: fm:fm-b")),
                task("c", "fm:fm-c", "pane", None),
            ],
        };
        let live: BTreeSet<String> = ["fm:fm-a", "fm:fm-b"].map(String::from).into();
        let (b, n) = liveness_view(&bash, &live).unwrap();
        assert_eq!(b["b"]["alive"], false);
        assert_eq!(n["b"]["alive"], true);
        assert_eq!(b["c"]["alive"], true);
        assert_eq!(n["c"]["alive"], false);
        assert!(b.get("a").is_none());

        let live: BTreeSet<String> = ["fm:fm-a", "fm:fm-c"].map(String::from).into();
        assert!(liveness_view(&bash, &live).is_none());
    }

    #[tokio::test]
    async fn reads_panes_through_the_native_tmux_backend() {
        let installed = std::process::Command::new("tmux")
            .arg("-V")
            .output()
            .is_ok_and(|o| o.status.success());
        if !installed {
            eprintln!("tmux is not installed; skipping");
            return;
        }
        let dir = tempfile::Builder::new()
            .prefix("ql")
            .tempdir_in("/tmp")
            .unwrap();
        let server = Server::new("tmux", dir.path().join("quark")).unwrap();
        let target = server
            .start_window(&WindowSpec {
                name: "fm-a".into(),
                argv: vec!["/bin/sh".into()],
                ..Default::default()
            })
            .await
            .unwrap();
        let check = SessionLiveness::new(server.clone());
        let ws = WorkspaceRef {
            project_id: "p".into(),
            root: dir.path().into(),
        };
        let live = check.live().await.unwrap();
        assert!(live.contains(&target), "{live:?}");

        let gone = format!("backend target gone: {}", target.replace("fm-a", "fm-b"));
        let bash = FleetSnapshot {
            tasks: vec![
                task("a", &target, "pane", None),
                task("b", &target.replace("fm-a", "fm-b"), "none", Some(&gone)),
            ],
        };
        assert!(check.diverges(&ws, &bash).await.unwrap().is_none());

        server.kill().await;
        let (b, n) = check.diverges(&ws, &bash).await.unwrap().unwrap();
        assert_eq!(b["a"]["alive"], true);
        assert_eq!(n["a"]["alive"], false);
    }
}
