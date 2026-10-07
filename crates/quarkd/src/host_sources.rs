//! What the Hosts view needs that only the engine knows: which processes
//! and worktrees belong to which Project and task, and the worktree pools.
//!
//! - [`EngineWorkloads`] is the [`Workloads`] source the host sampler
//!   attributes each sample with: every Project's coordinator window and
//!   every task's window and worktree, from the engine's snapshots, mapped
//!   to process ids through the shared tmux server.
//! - [`PoolReporter`] reads the treehouse pool of every repo clone in each
//!   Project's workspace and appends a [`crate::hosts::WORKTREES`] event
//!   whenever the result differs from the last one it recorded.
//!
//! Both only read: nothing here starts, stops or changes a worker or a
//! worktree.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use quark_core::worktree::{Holder, SlotState, WorktreeProvider};
use quark_core::{EventLog, HostId, NewEvent, ProjectId, Result, TaskId};
use quark_hosts::{Workload, Workloads};
use quark_worktree::TreehouseProvider;

use crate::engine::{EngineAdapter, WorkspaceRef};
use crate::hosts::{PoolReport, PoolSlot, WORKTREES};
use crate::sessions::Sessions;
use crate::store::Store;

/// Set to `0` to turn host telemetry and pool reports off.
pub const ENV: &str = "QUARK_HOST_TELEMETRY";
/// How often the pools are read.
pub const POOLS_EVERY: Duration = Duration::from_secs(60);
/// Longest one pool read may take before it counts as failed.
const POOL_TIMEOUT: Duration = Duration::from_secs(20);

/// On unless [`ENV`] is `0`.
pub fn enabled() -> bool {
    std::env::var(ENV).map_or(true, |v| v != "0")
}

/// A task of the engine's snapshot that holds a window or a worktree.
#[derive(Debug, Clone)]
struct Placed {
    project: String,
    task: String,
    terminal: Option<String>,
    worktree: Option<PathBuf>,
}

/// Reads the engine's view of every Project.
#[derive(Clone)]
pub struct EngineView {
    store: Arc<Store>,
    engine: Arc<dyn EngineAdapter>,
    sessions: Sessions,
    /// The command-center workspace, where coordinator windows are listed.
    command: PathBuf,
}

impl EngineView {
    pub fn new(
        store: Arc<Store>,
        engine: Arc<dyn EngineAdapter>,
        sessions: Sessions,
        command: PathBuf,
    ) -> Self {
        Self {
            store,
            engine,
            sessions,
            command,
        }
    }

    /// Every Project with a workspace, as `(id, workspace root)`.
    async fn workspaces(&self) -> Vec<WorkspaceRef> {
        let store = self.store.clone();
        let projects = tokio::task::spawn_blocking(move || store.list_projects())
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or_default();
        projects
            .into_iter()
            .filter_map(|p| {
                Some(WorkspaceRef {
                    project_id: p.id,
                    root: PathBuf::from(p.workspace_path?),
                })
            })
            .collect()
    }

    async fn placed(&self, workspaces: &[WorkspaceRef]) -> Vec<Placed> {
        let mut out = Vec::new();
        for ws in workspaces {
            let snap = match self.engine.snapshot(ws).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!(project = %ws.project_id, error = %e, "no snapshot for host attribution");
                    continue;
                }
            };
            for t in snap.tasks {
                if t.terminal.is_none() && t.worktree.is_none() {
                    continue;
                }
                out.push(Placed {
                    project: ws.project_id.clone(),
                    task: t.id,
                    terminal: t.terminal,
                    worktree: t.worktree,
                });
            }
        }
        out
    }

    /// Root process ids of every pane on the shared tmux server, by
    /// `session:window` target.
    async fn pane_pids(&self) -> HashMap<String, Vec<u32>> {
        let Ok(server) = self.sessions.server() else {
            return HashMap::new();
        };
        let out = match server
            .run(&[
                "list-panes",
                "-a",
                "-F",
                "#{session_name}:#{window_name}\t#{pane_pid}",
            ])
            .await
        {
            Ok(o) => o,
            Err(e) => {
                tracing::debug!(error = %e, "could not list panes for host attribution");
                return HashMap::new();
            }
        };
        parse_panes(&out)
    }
}

fn parse_panes(out: &str) -> HashMap<String, Vec<u32>> {
    let mut map: HashMap<String, Vec<u32>> = HashMap::new();
    for line in out.lines() {
        let Some((target, pid)) = line.rsplit_once('\t') else {
            continue;
        };
        if let Ok(pid) = pid.trim().parse() {
            map.entry(target.to_string()).or_default().push(pid);
        }
    }
    map
}

/// The [`Workloads`] on this host, from the engine and tmux.
pub struct EngineWorkloads(pub EngineView);

#[async_trait]
impl Workloads for EngineWorkloads {
    async fn current(&self) -> Result<Vec<Workload>> {
        let v = &self.0;
        let workspaces = v.workspaces().await;
        let panes = v.pane_pids().await;
        let pids = |target: &Option<String>| {
            target
                .as_ref()
                .and_then(|t| panes.get(t))
                .cloned()
                .unwrap_or_default()
        };
        let mut out = Vec::new();
        let coordinators = v
            .engine
            .coordinator_terminals(&v.command)
            .await
            .unwrap_or_default();
        for ws in &workspaces {
            let pids = pids(&coordinators.get(&ws.project_id).cloned());
            if !pids.is_empty() {
                out.push(Workload {
                    project: ProjectId::new(&ws.project_id),
                    task: None,
                    pids,
                    worktree: None,
                });
            }
        }
        for p in v.placed(&workspaces).await {
            out.push(Workload {
                project: ProjectId::new(&p.project),
                task: Some(TaskId::from(p.task.as_str())),
                pids: pids(&p.terminal),
                worktree: p.worktree,
            });
        }
        Ok(out)
    }
}

/// Records this host's worktree pools into the event log as they change.
pub struct PoolReporter {
    view: EngineView,
    log: Arc<dyn EventLog>,
    host: HostId,
    last: Option<PoolReport>,
}

impl PoolReporter {
    pub fn new(view: EngineView, log: Arc<dyn EventLog>, host: HostId) -> Self {
        Self {
            view,
            log,
            host,
            last: None,
        }
    }

    /// Reports every [`POOLS_EVERY`], forever.
    pub async fn run(mut self) {
        let mut ticks = tokio::time::interval(POOLS_EVERY);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            if let Err(e) = self.report_once().await {
                tracing::warn!(error = %e, "worktree pool report failed");
            }
        }
    }

    /// Read the pools and record them if they changed.
    pub async fn report_once(&mut self) -> Result<()> {
        let workspaces = self.view.workspaces().await;
        let placed = self.view.placed(&workspaces).await;
        let report = read_pools(&workspaces, &placed).await;
        if self.last.as_ref() == Some(&report) {
            return Ok(());
        }
        let e = NewEvent::typed(
            self.host.clone(),
            ProjectId::engine(),
            None,
            WORKTREES,
            &report,
        )?;
        self.log.append(e).await?;
        self.last = Some(report);
        Ok(())
    }
}

/// Repo clones in a workspace: each git checkout directly under
/// `<root>/projects/`.
fn clones(root: &Path) -> Vec<PathBuf> {
    let Ok(dir) = std::fs::read_dir(root.join("projects")) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = dir
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.join(".git").exists())
        .collect();
    out.sort();
    out
}

/// Every pool slot of every workspace's clones, plus task worktrees no pool
/// reported, as in use by their task.
async fn read_pools(workspaces: &[WorkspaceRef], placed: &[Placed]) -> PoolReport {
    let mut slots = Vec::new();
    let mut errors = BTreeSet::new();
    for ws in workspaces {
        let repos = clones(&ws.root);
        if repos.is_empty() {
            continue;
        }
        let provider = TreehouseProvider::treehouse();
        for r in &repos {
            provider.add_repo(r);
        }
        match tokio::time::timeout(POOL_TIMEOUT, provider.status()).await {
            Ok(Ok(status)) => slots.extend(status.slots.into_iter().map(|s| PoolSlot {
                path: s.path,
                repo: s.repo,
                state: s.state,
                holder: s.holder,
                project: Some(ws.project_id.clone()),
            })),
            Ok(Err(e)) => {
                errors.insert(e.to_string());
            }
            Err(_) => {
                errors.insert(format!(
                    "treehouse status took over {}s",
                    POOL_TIMEOUT.as_secs()
                ));
            }
        }
    }
    merge_tasks(&mut slots, placed);
    slots.sort_by(|a, b| (&a.repo, &a.path).cmp(&(&b.repo, &b.path)));
    PoolReport {
        slots,
        error: (!errors.is_empty()).then(|| errors.into_iter().collect::<Vec<_>>().join("; ")),
    }
}

/// A pool slot a task's worktree sits in names that task as its holder; a
/// task worktree outside every pool is listed as in use by its task.
fn merge_tasks(slots: &mut Vec<PoolSlot>, placed: &[Placed]) {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let mut by_path: HashMap<PathBuf, usize> = slots
        .iter()
        .enumerate()
        .map(|(i, s)| (canon(&s.path), i))
        .collect();
    for p in placed {
        let Some(wt) = &p.worktree else {
            continue;
        };
        let holder = Some(Holder::Task {
            task: TaskId::from(p.task.as_str()),
        });
        let key = canon(wt);
        match by_path.get(&key) {
            Some(&i) => {
                let s = &mut slots[i];
                if !matches!(s.holder, Some(Holder::Task { .. })) {
                    s.holder = holder;
                }
                if s.state == SlotState::Idle {
                    s.state = SlotState::InUse;
                }
                s.project.get_or_insert_with(|| p.project.clone());
            }
            None => {
                by_path.insert(key, slots.len());
                slots.push(PoolSlot {
                    path: wt.clone(),
                    repo: PathBuf::new(),
                    state: SlotState::InUse,
                    holder,
                    project: Some(p.project.clone()),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panes_by_target() {
        let m = parse_panes("quark:fm-a\t101\nquark:fm-a\t102\nbad line\nquark:x:y\t7\n");
        assert_eq!(m["quark:fm-a"], vec![101, 102]);
        assert_eq!(m["quark:x:y"], vec![7]);
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn task_worktrees_join_their_slots() {
        let mut slots = vec![
            PoolSlot {
                path: "/nowhere/pool/1".into(),
                repo: "/nowhere/repo".into(),
                state: SlotState::Leased,
                holder: Some(Holder::Lease {
                    owner: "fm:t1".into(),
                }),
                project: Some("p".into()),
            },
            PoolSlot {
                path: "/nowhere/pool/2".into(),
                repo: "/nowhere/repo".into(),
                state: SlotState::Idle,
                holder: None,
                project: Some("p".into()),
            },
        ];
        let placed = vec![
            Placed {
                project: "p".into(),
                task: "t1".into(),
                terminal: None,
                worktree: Some("/nowhere/pool/1".into()),
            },
            Placed {
                project: "p".into(),
                task: "t2".into(),
                terminal: Some("quark:t2".into()),
                worktree: Some("/nowhere/elsewhere".into()),
            },
            Placed {
                project: "p".into(),
                task: "t3".into(),
                terminal: Some("quark:t3".into()),
                worktree: None,
            },
        ];
        merge_tasks(&mut slots, &placed);
        assert_eq!(slots.len(), 3);
        assert_eq!(slots[0].holder, Some(Holder::Task { task: "t1".into() }));
        assert_eq!(slots[0].state, SlotState::Leased);
        assert_eq!(slots[1].state, SlotState::Idle);
        assert_eq!(slots[2].path, PathBuf::from("/nowhere/elsewhere"));
        assert_eq!(slots[2].state, SlotState::InUse);
    }

    #[test]
    fn clones_are_git_checkouts_under_projects() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("projects/a/.git")).unwrap();
        std::fs::create_dir_all(dir.path().join("projects/b")).unwrap();
        assert_eq!(clones(dir.path()), vec![dir.path().join("projects/a")]);
        assert!(clones(&dir.path().join("missing")).is_empty());
    }
}
