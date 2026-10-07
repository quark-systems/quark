//! Raw readings from the operating system.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use quark_core::{CoreError, Result};
use sysinfo::{
    CpuRefreshKind, Disks, MemoryRefreshKind, ProcessRefreshKind, ProcessesToUpdate, System,
    UpdateKind, MINIMUM_CPU_UPDATE_INTERVAL,
};

use crate::pressure;

/// One process, as attribution needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub parent: Option<u32>,
    /// Share of the whole host, 0.0 to 1.0 across all cores.
    pub cpu: f64,
    /// Resident memory.
    pub memory_bytes: u64,
}

/// Everything a sample reads from the OS in one go.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    /// 0.0 to 1.0 across all cores, since the previous reading.
    pub cpu: f64,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    /// `None` when the OS does not report pressure.
    pub memory_pressure: Option<f64>,
    /// Free space on the filesystem holding the disk root.
    pub disk_free_bytes: u64,
    pub processes: Vec<ProcessInfo>,
}

/// Reads the host. Blocking; the sampler calls it off the async runtime.
pub trait Probe: Send + Sync + 'static {
    fn read(&self, disk_root: &Path) -> Result<Reading>;
}

/// The real host, through `sysinfo`, plus Linux PSI or the macOS pressure
/// level for memory pressure.
pub struct SystemProbe {
    state: Mutex<State>,
}

struct State {
    system: System,
    disks: Disks,
    last: Instant,
}

impl SystemProbe {
    /// Takes a first CPU reading so the next `read` has a baseline.
    pub fn new() -> Self {
        let mut system = System::new();
        system.refresh_cpu_usage();
        system.refresh_processes_specifics(ProcessesToUpdate::All, true, process_kind());
        Self {
            state: Mutex::new(State {
                system,
                disks: Disks::new_with_refreshed_list(),
                last: Instant::now(),
            }),
        }
    }
}

impl Default for SystemProbe {
    fn default() -> Self {
        Self::new()
    }
}

fn process_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing()
        .with_cpu()
        .with_memory()
        .without_tasks()
        .with_exe(UpdateKind::Never)
}

impl Probe for SystemProbe {
    fn read(&self, disk_root: &Path) -> Result<Reading> {
        let mut st = self.state.lock().unwrap();
        // CPU usage is measured between two refreshes; too short a gap
        // reads as noise.
        let since = st.last.elapsed();
        if since < MINIMUM_CPU_UPDATE_INTERVAL {
            std::thread::sleep(MINIMUM_CPU_UPDATE_INTERVAL - since);
        }
        st.system
            .refresh_cpu_specifics(CpuRefreshKind::nothing().with_cpu_usage());
        st.system
            .refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
        st.system
            .refresh_processes_specifics(ProcessesToUpdate::All, true, process_kind());
        st.disks.refresh(true);
        st.last = Instant::now();

        let cores = st.system.cpus().len().max(1) as f64;
        let cpu = (f64::from(st.system.global_cpu_usage()) / 100.0).clamp(0.0, 1.0);
        let processes = st
            .system
            .processes()
            .iter()
            .map(|(pid, p)| ProcessInfo {
                pid: pid.as_u32(),
                parent: p.parent().map(|pp| pp.as_u32()),
                cpu: (f64::from(p.cpu_usage()) / 100.0 / cores).clamp(0.0, 1.0),
                memory_bytes: p.memory(),
            })
            .collect();

        Ok(Reading {
            cpu,
            memory_used_bytes: st.system.used_memory(),
            memory_total_bytes: st.system.total_memory(),
            memory_pressure: pressure::read(),
            disk_free_bytes: free_space(&st.disks, disk_root)?,
            processes,
        })
    }
}

/// Free space on the disk whose mount point is the longest prefix of
/// `root`.
fn free_space(disks: &Disks, root: &Path) -> Result<u64> {
    let root: PathBuf = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    disks
        .list()
        .iter()
        .filter(|d| root.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().components().count())
        .map(|d| d.available_space())
        .ok_or_else(|| CoreError::Backend(format!("no disk mounted at {}", root.display())))
}
