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
//! It also compares the agent, not just the pane. Firstmate types its agent
//! into a shell, so the agent can exit while the pane stays. The native
//! backend reads the pane's foreground process group
//! ([`TmuxBackend::foreground`]): nothing but shells means the agent is gone,
//! which is when the native supervisor would relaunch it (a native session
//! runs its agent as the pane's own program, so there the pane ends too).
//! Firstmate says the agent is gone with "agent gone, pane shell remains",
//! and that it runs with "harness busy". An idle pane says nothing about the
//! agent, and firstmate notices an exited agent there only when its watcher
//! reports the worker stale, so a pane whose agent the native backend has
//! seen gone for [`SHELL_GRACE`] while firstmate still keeps the worker is a
//! divergence too, while the task is active: the native supervisor would
//! have relaunched it long ago.
//!
//! Only tasks whose liveness firstmate actually read are compared: a state
//! from the pane or the status log means the pane answered, and "backend
//! target gone" means it is missing. A task firstmate judged from a
//! validation run, or could not reach, is left out.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use quark_core::session::SessionBackend;
use quark_sessions::tmux::server::Server;
use quark_sessions::tmux::{Foreground, TmuxBackend};
use quark_systems::TaskState;
use serde_json::json;

use super::{EngineError, EngineTask, FleetSnapshot, WorkspaceRef};

/// Operation the divergences are recorded under.
pub const OPERATION: &str = "session_liveness";

/// How long firstmate may keep a worker whose agent has exited before that
/// counts as a divergence: its watcher reports an idle worker stale after
/// a few minutes, and its coordinator then relaunches it.
pub const SHELL_GRACE: Duration = Duration::from_secs(10 * 60);

/// What `fm-crew-state.sh` writes when the pane is missing.
const GONE: &str = "backend target gone:";
/// Its suffix when the pane is there but the agent in it has exited.
const AGENT_GONE: &str = "agent gone";
/// What it writes when the harness in the pane is working.
const BUSY: &str = "harness busy";

/// What one side says about a task's worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    /// No pane.
    Gone,
    /// The pane is there, with nothing but its shell: the agent exited.
    Shell,
    /// The pane is there and its agent runs.
    Agent,
    /// The pane is there; nothing was learned about its agent.
    Pane,
}

impl Seen {
    fn as_str(self) -> &'static str {
        match self {
            Seen::Gone => "gone",
            Seen::Shell => "shell",
            Seen::Agent => "agent",
            Seen::Pane => "pane",
        }
    }
}

/// Firstmate's tmux panes, as the native session backend sees them.
pub struct SessionLiveness {
    server: Server,
    sessions: TmuxBackend,
    /// When the native backend first saw each target's agent gone.
    shell_since: Mutex<HashMap<String, Instant>>,
}

impl SessionLiveness {
    /// Read through a native backend on `server`, the shared tmux server
    /// firstmate's panes run on.
    pub fn new(server: Server) -> Self {
        Self {
            sessions: TmuxBackend::new(server.clone()),
            server,
            shell_since: Mutex::new(HashMap::new()),
        }
    }

    /// The first pane of every window the native backend lists alive, by
    /// `session:window` target and by window id.
    async fn live(&self) -> Result<BTreeMap<String, String>, EngineError> {
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
                [
                    (p.target, p.pane_id.clone()),
                    (format!("{session}:{}", p.window_id), p.pane_id),
                ]
            })
            .collect())
    }

    /// What the native backend says about each target firstmate read.
    async fn native(&self, bash: &FleetSnapshot) -> Result<BTreeMap<String, Seen>, EngineError> {
        let live = self.live().await?;
        let mut seen = BTreeMap::new();
        for t in &bash.tasks {
            if firstmate_seen(t).is_none() {
                continue;
            }
            let target = t.terminal.clone().unwrap_or_default();
            let s = match live.get(&target) {
                None => Seen::Gone,
                Some(pane) => match self.sessions.foreground(pane).await {
                    Foreground::Program => Seen::Agent,
                    Foreground::ShellOnly => Seen::Shell,
                    Foreground::Unknown => Seen::Pane,
                },
            };
            seen.insert(target, s);
        }
        Ok(seen)
    }

    /// Records when each target's agent was first seen gone, and returns
    /// the targets whose agent has been gone for [`SHELL_GRACE`].
    fn overdue(&self, native: &BTreeMap<String, Seen>, now: Instant) -> BTreeSet<String> {
        let mut since = self.shell_since.lock().unwrap_or_else(|p| p.into_inner());
        since.retain(|t, _| native.get(t) == Some(&Seen::Shell));
        native
            .iter()
            .filter(|(_, s)| **s == Seen::Shell)
            .filter_map(|(t, _)| {
                let first = *since.entry(t.clone()).or_insert(now);
                (now.duration_since(first) >= SHELL_GRACE).then(|| t.clone())
            })
            .collect()
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
        if !bash.tasks.iter().any(|t| firstmate_seen(t).is_some()) {
            return Ok(None);
        }
        let native = self.native(bash).await?;
        let overdue = self.overdue(&native, Instant::now());
        Ok(liveness_view(bash, &native, &overdue))
    }
}

/// What firstmate read about the task's pane and agent; `None` when it did
/// not read the pane, or the task has no local tmux pane.
pub fn firstmate_seen(t: &EngineTask) -> Option<Seen> {
    let target = t.terminal.as_deref()?;
    // A window index is not stable enough to match.
    let window = target.rsplit(':').next()?;
    if window.is_empty() || window.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let note = t.state_note.as_deref().unwrap_or_default();
    match t.state_source.as_deref() {
        Some("pane") if note.starts_with(BUSY) => Some(Seen::Agent),
        Some("pane" | "status-log") => Some(Seen::Pane),
        Some("none") if note.starts_with(GONE) => Some(if note.contains(AGENT_GONE) {
            Seen::Shell
        } else {
            Seen::Gone
        }),
        _ => None,
    }
}

/// Whether the two sides agree. `overdue` is whether the native backend
/// has seen the agent gone for [`SHELL_GRACE`].
fn agrees(firstmate: Seen, native: Seen, overdue: bool) -> bool {
    use Seen::*;
    match (firstmate, native) {
        (Gone, n) => n == Gone,
        (_, Gone) => false,
        (Shell, Agent) | (Agent, Shell) => false,
        (Pane, Shell) => !overdue,
        _ => true,
    }
}

/// The tasks whose worker the two sides see differently, with what each
/// side says, given what the native backend saw at each target and which
/// targets' agents it has seen gone for [`SHELL_GRACE`].
pub fn liveness_view(
    bash: &FleetSnapshot,
    native: &BTreeMap<String, Seen>,
    overdue: &BTreeSet<String>,
) -> Option<(serde_json::Value, serde_json::Value)> {
    let mut ours = BTreeMap::new();
    let mut theirs = BTreeMap::new();
    for t in &bash.tasks {
        let Some(fm) = firstmate_seen(t) else {
            continue;
        };
        let target = t.terminal.as_deref().unwrap_or_default();
        let n = native.get(target).copied().unwrap_or(Seen::Gone);
        // The native supervisor relaunches only an active task's worker.
        let active = matches!(
            t.state,
            TaskState::Running | TaskState::NeedsDecision | TaskState::Blocked | TaskState::Paused
        );
        if !agrees(fm, n, active && overdue.contains(target)) {
            theirs.insert(
                t.id.clone(),
                json!({"target": target, "worker": fm.as_str()}),
            );
            ours.insert(
                t.id.clone(),
                json!({"target": target, "worker": n.as_str()}),
            );
        }
    }
    (!ours.is_empty()).then(|| (json!(theirs), json!(ours)))
}

#[cfg(test)]
mod tests {
    use quark_sessions::tmux::server::WindowSpec;

    use super::super::shadow::SnapshotCheck;
    use super::*;

    fn task(id: &str, target: &str, source: &str, note: Option<&str>) -> EngineTask {
        EngineTask {
            id: id.into(),
            title: id.into(),
            kind: None,
            state: TaskState::Running,
            state_note: note.map(str::to_string),
            state_source: Some(source.into()),
            harness: None,
            pull_request_url: None,
            terminal: Some(target.into()),
            worktree: None,
        }
    }

    fn seen(pairs: &[(&str, Seen)]) -> BTreeMap<String, Seen> {
        pairs.iter().map(|(t, s)| (t.to_string(), *s)).collect()
    }

    #[test]
    fn reads_firstmates_view_only_where_it_looked() {
        let read = |source, note| firstmate_seen(&task("a", "fm:fm-a", source, note));
        assert_eq!(read("pane", None), Some(Seen::Pane));
        assert_eq!(
            read("pane", Some("harness busy (record)")),
            Some(Seen::Agent)
        );
        assert_eq!(read("status-log", None), Some(Seen::Pane));
        assert_eq!(
            read("none", Some("backend target gone: fm:fm-a")),
            Some(Seen::Gone)
        );
        let shell = "backend target gone: fm:fm-a (agent gone, pane shell remains)";
        assert_eq!(read("none", Some(shell)), Some(Seen::Shell));
        let unreachable = "backend unreachable (tmux endpoint state: unknown)";
        assert_eq!(read("none", Some(unreachable)), None);
        assert_eq!(read("run-step", None), None);
        assert_eq!(firstmate_seen(&task("a", "fm:3", "pane", None)), None);
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
        let native = seen(&[("fm:fm-a", Seen::Agent), ("fm:fm-b", Seen::Pane)]);
        let (b, n) = liveness_view(&bash, &native, &BTreeSet::new()).unwrap();
        assert_eq!(b["b"]["worker"], "gone");
        assert_eq!(n["b"]["worker"], "pane");
        assert_eq!(b["c"]["worker"], "pane");
        assert_eq!(n["c"]["worker"], "gone");
        assert!(b.get("a").is_none());

        let native = seen(&[("fm:fm-a", Seen::Pane), ("fm:fm-c", Seen::Shell)]);
        assert!(liveness_view(&bash, &native, &BTreeSet::new()).is_none());
    }

    #[test]
    fn compares_the_agent_when_firstmate_read_it() {
        let shell = "backend target gone: fm:fm-a (agent gone, pane shell remains)";
        let bash = FleetSnapshot {
            tasks: vec![
                task("a", "fm:fm-a", "none", Some(shell)),
                task("b", "fm:fm-b", "pane", Some("harness busy (record)")),
            ],
        };
        let agree = seen(&[("fm:fm-a", Seen::Shell), ("fm:fm-b", Seen::Agent)]);
        assert!(liveness_view(&bash, &agree, &BTreeSet::new()).is_none());
        // The pane could not be read natively: nothing to compare.
        let unread = seen(&[("fm:fm-a", Seen::Pane), ("fm:fm-b", Seen::Pane)]);
        assert!(liveness_view(&bash, &unread, &BTreeSet::new()).is_none());

        let swapped = seen(&[("fm:fm-a", Seen::Agent), ("fm:fm-b", Seen::Shell)]);
        let (b, n) = liveness_view(&bash, &swapped, &BTreeSet::new()).unwrap();
        assert_eq!(b["a"]["worker"], "shell");
        assert_eq!(n["a"]["worker"], "agent");
        assert_eq!(b["b"]["worker"], "agent");
        assert_eq!(n["b"]["worker"], "shell");

        // Firstmate lost the pane its shell was in; natively it is gone too.
        let gone = seen(&[("fm:fm-b", Seen::Agent)]);
        let (b, n) = liveness_view(&bash, &gone, &BTreeSet::new()).unwrap();
        assert_eq!(b["a"]["worker"], "shell");
        assert_eq!(n["a"]["worker"], "gone");
    }

    #[test]
    fn an_exited_agent_firstmate_keeps_past_the_grace_diverges() {
        let mut idle = task("a", "fm:fm-a", "status-log", Some("working: go"));
        let mut done = task("b", "fm:fm-b", "status-log", Some("done: PR"));
        done.state = TaskState::InReview;
        let bash = FleetSnapshot {
            tasks: vec![idle.clone(), done],
        };
        let native = seen(&[("fm:fm-a", Seen::Shell), ("fm:fm-b", Seen::Shell)]);
        assert!(liveness_view(&bash, &native, &BTreeSet::new()).is_none());

        let overdue: BTreeSet<String> = ["fm:fm-a", "fm:fm-b"].map(String::from).into();
        let (b, n) = liveness_view(&bash, &native, &overdue).unwrap();
        assert_eq!(b["a"]["worker"], "pane");
        assert_eq!(n["a"]["worker"], "shell");
        assert!(b.get("b").is_none(), "an inactive task is not relaunched");

        idle.state = TaskState::Done;
        let bash = FleetSnapshot { tasks: vec![idle] };
        assert!(liveness_view(&bash, &native, &overdue).is_none());
    }

    #[test]
    fn counts_the_grace_from_when_the_agent_was_first_seen_gone() {
        let check = SessionLiveness::new(Server::new("tmux", "/tmp/unused").unwrap());
        let t0 = Instant::now();
        let shell = seen(&[("fm:fm-a", Seen::Shell)]);
        assert!(check.overdue(&shell, t0).is_empty());
        assert!(check.overdue(&shell, t0 + SHELL_GRACE / 2).is_empty());
        assert!(check.overdue(&shell, t0 + SHELL_GRACE).contains("fm:fm-a"));
        // A relaunched agent starts the count again.
        let back = seen(&[("fm:fm-a", Seen::Agent)]);
        assert!(check.overdue(&back, t0 + SHELL_GRACE).is_empty());
        let later = t0 + SHELL_GRACE * 2;
        assert!(check.overdue(&shell, later).is_empty());
    }

    #[tokio::test]
    async fn reads_panes_and_agents_through_the_native_tmux_backend() {
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
        // Firstmate's way: a shell, with the agent typed into it.
        let target = server
            .start_window(&WindowSpec {
                name: "fm-a".into(),
                argv: vec!["/bin/sh".into()],
                ..Default::default()
            })
            .await
            .unwrap();
        server
            .run(&["send-keys", "-t", &target, "sleep 600", "Enter"])
            .await
            .unwrap();
        let check = SessionLiveness::new(server.clone());
        let ws = WorkspaceRef {
            project_id: "p".into(),
            root: dir.path().into(),
        };
        let live = check.live().await.unwrap();
        assert!(live.contains_key(&target), "{live:?}");

        let other = target.replace("fm-a", "fm-b");
        let gone = format!("backend target gone: {other}");
        let busy = FleetSnapshot {
            tasks: vec![
                task("a", &target, "pane", Some("harness busy (record)")),
                task("b", &other, "none", Some(&gone)),
            ],
        };
        wait_for(&check, &busy, &target, Seen::Agent).await;
        assert!(check.diverges(&ws, &busy).await.unwrap().is_none());

        // The agent exits; the shell stays.
        server
            .run(&["send-keys", "-t", &target, "C-c", ""])
            .await
            .unwrap();
        wait_for(&check, &busy, &target, Seen::Shell).await;
        let (b, n) = check.diverges(&ws, &busy).await.unwrap().unwrap();
        assert_eq!(b["a"]["worker"], "agent");
        assert_eq!(n["a"]["worker"], "shell");
        let shell = format!("backend target gone: {target} (agent gone, pane shell remains)");
        let exited = FleetSnapshot {
            tasks: vec![task("a", &target, "none", Some(&shell))],
        };
        assert!(check.diverges(&ws, &exited).await.unwrap().is_none());

        server.kill().await;
        let (b, n) = check.diverges(&ws, &exited).await.unwrap().unwrap();
        assert_eq!(b["a"]["worker"], "shell");
        assert_eq!(n["a"]["worker"], "gone");
    }

    async fn wait_for(check: &SessionLiveness, bash: &FleetSnapshot, target: &str, want: Seen) {
        for _ in 0..100 {
            if check.native(bash).await.unwrap().get(target) == Some(&want) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("{target} never read as {want:?}");
    }
}
