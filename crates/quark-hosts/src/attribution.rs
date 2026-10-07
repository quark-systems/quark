//! Which processes and paths belong to which Project and task.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use quark_core::{ProjectId, Result, TaskId};

use crate::probe::ProcessInfo;

/// Something running for a Project: a task's worker, or the Project's
/// coordinator (`task: None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workload {
    pub project: ProjectId,
    pub task: Option<TaskId>,
    /// Root processes, such as the harness in the worker's pane. Their
    /// descendants count too.
    pub pids: Vec<u32>,
    /// The worktree whose size is this workload's disk use.
    pub worktree: Option<PathBuf>,
}

/// The workloads running on this host right now.
#[async_trait]
pub trait Workloads: Send + Sync {
    async fn current(&self) -> Result<Vec<Workload>>;
}

/// A list the caller keeps up to date.
#[derive(Debug, Default)]
pub struct StaticWorkloads(Mutex<Vec<Workload>>);

impl StaticWorkloads {
    pub fn new(workloads: Vec<Workload>) -> Self {
        Self(Mutex::new(workloads))
    }

    pub fn set(&self, workloads: Vec<Workload>) {
        *self.0.lock().unwrap() = workloads;
    }
}

#[async_trait]
impl Workloads for StaticWorkloads {
    async fn current(&self) -> Result<Vec<Workload>> {
        Ok(self.0.lock().unwrap().clone())
    }
}

/// CPU share and resident memory per workload, in the order given.
///
/// A process counts toward the workload whose root is its nearest ancestor
/// (or itself), so nested roots never count a process twice. A pid listed as
/// a root by two workloads goes to the first.
pub fn attribute(processes: &[ProcessInfo], workloads: &[Workload]) -> Vec<(f64, u64)> {
    let mut roots: HashMap<u32, usize> = HashMap::new();
    for (i, w) in workloads.iter().enumerate() {
        for pid in &w.pids {
            roots.entry(*pid).or_insert(i);
        }
    }
    let parents: HashMap<u32, Option<u32>> = processes.iter().map(|p| (p.pid, p.parent)).collect();

    let mut totals = vec![(0.0, 0u64); workloads.len()];
    for p in processes {
        if let Some(i) = owner(p.pid, &roots, &parents) {
            totals[i].0 += p.cpu;
            totals[i].1 += p.memory_bytes;
        }
    }
    for t in &mut totals {
        t.0 = t.0.min(1.0);
    }
    totals
}

fn owner(
    pid: u32,
    roots: &HashMap<u32, usize>,
    parents: &HashMap<u32, Option<u32>>,
) -> Option<usize> {
    let mut cur = pid;
    // Bounded so a parent cycle in a racy process listing cannot hang.
    for _ in 0..128 {
        if let Some(i) = roots.get(&cur) {
            return Some(*i);
        }
        cur = (*parents.get(&cur)?)?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, parent: Option<u32>, cpu: f64, mem: u64) -> ProcessInfo {
        ProcessInfo {
            pid,
            parent,
            cpu,
            memory_bytes: mem,
        }
    }

    fn workload(project: &str, task: Option<&str>, pids: &[u32]) -> Workload {
        Workload {
            project: project.into(),
            task: task.map(Into::into),
            pids: pids.to_vec(),
            worktree: None,
        }
    }

    #[test]
    fn sums_descendants_to_the_nearest_root() {
        // 1 init
        // ├── 10 tmux
        // │   ├── 20 coordinator ── 21 child
        // │   └── 30 worker ── 31 cargo ── 32 rustc
        // └── 40 unrelated
        let procs = [
            proc(1, None, 0.01, 1),
            proc(10, Some(1), 0.01, 10),
            proc(20, Some(10), 0.05, 200),
            proc(21, Some(20), 0.05, 210),
            proc(30, Some(10), 0.10, 300),
            proc(31, Some(30), 0.20, 310),
            proc(32, Some(31), 0.30, 320),
            proc(40, Some(1), 0.50, 400),
        ];
        let w = [
            workload("p", None, &[20]),
            workload("p", Some("t1"), &[30]),
            workload("q", Some("t2"), &[999]),
        ];
        let got = attribute(&procs, &w);
        assert!((got[0].0 - 0.10).abs() < 1e-9);
        assert_eq!(got[0].1, 410);
        assert!((got[1].0 - 0.60).abs() < 1e-9);
        assert_eq!(got[1].1, 930);
        assert_eq!(got[2], (0.0, 0));
    }

    #[test]
    fn nested_roots_do_not_double_count() {
        let procs = [
            proc(20, None, 0.1, 100),
            proc(30, Some(20), 0.2, 200),
            proc(31, Some(30), 0.3, 300),
        ];
        let w = [workload("p", None, &[20]), workload("p", Some("t"), &[30])];
        let got = attribute(&procs, &w);
        assert_eq!(got[0].1, 100);
        assert_eq!(got[1].1, 500);
    }

    #[test]
    fn survives_parent_cycles() {
        let procs = [proc(5, Some(6), 0.1, 1), proc(6, Some(5), 0.1, 1)];
        let got = attribute(&procs, &[workload("p", None, &[7])]);
        assert_eq!(got[0], (0.0, 0));
    }
}
