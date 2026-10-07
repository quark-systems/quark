//! Processes in a worktree, read the way treehouse 3.1.2 reads them: any
//! process whose working directory is inside the slot, minus this process
//! and its ancestors (which only look at it).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System, UpdateKind};

/// One process found in a worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub name: String,
}

fn snapshot() -> System {
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cwd(UpdateKind::Always),
    );
    sys
}

fn resolve(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// `cwd` is `worktree` or inside it.
pub fn within(worktree: &Path, cwd: &Path) -> bool {
    !cwd.as_os_str().is_empty() && resolve(cwd).starts_with(resolve(worktree))
}

/// The process table at one moment, for several worktree questions.
pub struct Snapshot {
    sys: System,
    protected: HashSet<Pid>,
}

impl Snapshot {
    pub fn take() -> Self {
        let sys = snapshot();
        let protected = protected(&sys);
        Self { sys, protected }
    }

    /// Every process working inside `worktree`, minus this process and its
    /// ancestors.
    pub fn in_worktree(&self, worktree: &Path) -> Vec<Proc> {
        let root = resolve(worktree);
        let mut out: Vec<Proc> = self
            .sys
            .processes()
            .iter()
            .filter(|(pid, _)| !self.protected.contains(pid))
            .filter(|(_, p)| p.cwd().is_some_and(|c| resolve(c).starts_with(&root)))
            .map(|(pid, p)| Proc {
                pid: pid.as_u32(),
                name: p.name().to_string_lossy().into_owned(),
            })
            .collect();
        out.sort_by_key(|p| p.pid);
        out
    }
}

/// Every process working inside `worktree` now (see [`Snapshot`]).
pub fn in_worktree(worktree: &Path) -> Vec<Proc> {
    Snapshot::take().in_worktree(worktree)
}

fn protected(sys: &System) -> HashSet<Pid> {
    let mut set = HashSet::new();
    let mut pid = Some(Pid::from_u32(std::process::id()));
    while let Some(p) = pid {
        if !set.insert(p) {
            break;
        }
        pid = sys.process(p).and_then(|p| p.parent());
    }
    set
}

/// The owner reservation `(pid, started_at)` treehouse recorded (start time
/// in milliseconds, from gopsutil) still names a live process. Start times
/// are compared to the second: both sides derive them from the same kernel
/// clock but round differently.
pub fn owner_alive(pid: i64, started_at_ms: i64) -> bool {
    if pid <= 0 || started_at_ms == 0 {
        return false;
    }
    let mut sys = System::new();
    let pid = Pid::from_u32(pid as u32);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    sys.process(pid)
        .is_some_and(|p| (p.start_time() as i64 - started_at_ms / 1000).abs() <= 1)
}

/// SIGTERM every process in `worktree`, SIGKILL whatever is left after
/// `grace`, and report what was stopped and what still runs there.
pub fn terminate(worktree: &Path, grace: Duration) -> (Vec<Proc>, Vec<Proc>) {
    let procs = in_worktree(worktree);
    if procs.is_empty() {
        return (procs, Vec::new());
    }
    let signal = |sig| {
        let sys = snapshot();
        for p in &procs {
            if let Some(proc_) = sys.process(Pid::from_u32(p.pid)) {
                proc_.kill_with(sig);
            }
        }
    };
    signal(Signal::Term);
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline && !in_worktree(worktree).is_empty() {
        std::thread::sleep(Duration::from_millis(100));
    }
    if !in_worktree(worktree).is_empty() {
        signal(Signal::Kill);
        std::thread::sleep(Duration::from_millis(200));
    }
    let left = in_worktree(worktree);
    (procs, left)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_and_stops_a_process_in_a_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .current_dir(tmp.path())
            .spawn()
            .unwrap();
        let pid = child.id();
        let found = in_worktree(tmp.path());
        if !found.iter().any(|p| p.pid == pid) {
            // No readable cwd for other processes here (no /proc access).
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        let (stopped, left) = terminate(tmp.path(), Duration::from_secs(2));
        assert!(stopped.iter().any(|p| p.pid == pid));
        assert!(left.is_empty(), "{left:?}");
        let _ = child.wait();
    }

    #[test]
    fn this_process_is_alive_and_protected() {
        let me = std::process::id() as i64;
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );
        let started = sys.process(Pid::from_u32(me as u32)).unwrap().start_time() as i64;
        assert!(owner_alive(me, started * 1000 + 400));
        assert!(!owner_alive(me, (started - 60) * 1000));
        assert!(!owner_alive(0, 1));
        let cwd = std::env::current_dir().unwrap();
        assert!(!in_worktree(&cwd).iter().any(|p| p.pid == me as u32));
    }
}
