//! The Hosts view and each Project's slice of its hosts, read from host
//! registrations, telemetry samples and worktree pool status in the native
//! event log.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Every host Quark knows, with its latest readings and what runs on it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct HostsView {
    /// Hours of history in each `series`, ending now.
    pub hours: u32,
    pub hosts: Vec<HostView>,
    /// Set when the event log could not be read; `hosts` is then empty.
    pub error: Option<String>,
}

/// How a host is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HostRuntime {
    Local,
    Ssh,
    Hosted,
    PrivateCloud,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HostHealthStatus {
    Healthy,
    Degraded,
    Unreachable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HostHealth {
    pub status: HostHealthStatus,
    /// Why it is degraded or unreachable.
    pub reason: Option<String>,
    /// When the host last changed to this health (RFC 3339 UTC).
    pub since: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HostCapacity {
    pub cpus: u32,
    pub memory_bytes: u64,
    /// 0 when not reported.
    pub disk_bytes: u64,
    /// Most workers placed here at once; 0 for no cap.
    pub max_workers: u32,
}

/// One host in the Hosts view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct HostView {
    pub id: String,
    pub name: String,
    pub runtime: HostRuntime,
    /// `macos` or `linux`.
    pub os: String,
    pub arch: String,
    pub capacity: HostCapacity,
    pub health: HostHealth,
    /// The newest sample; absent when the host was never sampled.
    pub latest: Option<HostReading>,
    /// Samples in the window, oldest first, thinned to at most
    /// [`SERIES_POINTS`] points (each the mean of the samples it covers).
    pub series: Vec<HostPoint>,
    /// Each Project's share of the newest sample, largest memory first.
    pub projects: Vec<ProjectUsage>,
    /// The worktree pools on this host; absent when never reported.
    pub worktrees: Option<HostWorktrees>,
}

/// Most points one series carries.
pub const SERIES_POINTS: usize = 120;

/// One sample of a host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct HostReading {
    /// RFC 3339 UTC.
    pub at: String,
    /// 0 to 1 across all cores.
    pub cpu: f64,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    /// 0 (none) to 1 (critical).
    pub memory_pressure: f64,
    pub disk_free_bytes: u64,
    /// Disk Quark itself fills on this host.
    pub quark_disk: QuarkDiskUse,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct QuarkDiskUse {
    pub worktrees_bytes: u64,
    pub logs_bytes: u64,
    pub caches_bytes: u64,
    pub event_log_bytes: u64,
}

/// A point of a host's time series.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct HostPoint {
    /// RFC 3339 UTC; the last sample the point covers.
    pub at: String,
    pub cpu: f64,
    pub memory_used_bytes: u64,
    pub memory_pressure: f64,
    pub disk_free_bytes: u64,
}

/// One Project's share of a host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ProjectUsage {
    pub project_id: String,
    /// The Project's name, when the daemon knows the Project.
    pub project_name: Option<String>,
    /// 0 to 1 of the host's CPU.
    pub cpu: f64,
    pub memory_bytes: u64,
    /// Size of the Project's task worktrees.
    pub disk_bytes: u64,
    /// The coordinator first (`engine_task` absent), then tasks, largest
    /// memory first.
    pub parts: Vec<UsagePart>,
}

/// A coordinator's or task's share of a host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct UsagePart {
    /// The engine's task id; absent for the Project's coordinator.
    pub engine_task: Option<String>,
    /// The Quark task, when the daemon knows it.
    pub task_id: Option<String>,
    pub title: Option<String>,
    pub cpu: f64,
    pub memory_bytes: u64,
    pub disk_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeSlotState {
    Idle,
    InUse,
    Dirty,
    Leased,
    /// Set aside after a crash until someone looks at it.
    Quarantined,
}

/// The worktree pools on one host, as last reported.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct HostWorktrees {
    /// When the pools were last read (RFC 3339 UTC).
    pub at: String,
    pub idle: u32,
    pub in_use: u32,
    pub dirty: u32,
    pub leased: u32,
    pub quarantined: u32,
    /// Why the pools could not be read, when they could not; the counts
    /// are then from the last good read.
    pub error: Option<String>,
    pub slots: Vec<WorktreeSlotView>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct WorktreeSlotView {
    pub path: String,
    pub repo: String,
    pub state: WorktreeSlotState,
    /// Who holds it: a task's engine id, or a lease owner.
    pub holder: Option<String>,
    /// The Project holding it, when known.
    pub project_id: Option<String>,
}

/// One Project's slice of every host it runs on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ProjectHosts {
    pub project_id: String,
    /// Hours of history in each `series`, ending now.
    pub hours: u32,
    /// Hosts where the Project used anything in the window.
    pub hosts: Vec<ProjectHostSlice>,
    /// Set when the event log could not be read.
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ProjectHostSlice {
    pub host_id: String,
    pub name: String,
    pub health: HostHealth,
    pub capacity: HostCapacity,
    /// The Project's share of the host's newest sample; absent when the
    /// Project uses nothing there now.
    pub now: Option<ProjectUsage>,
    /// The host's own newest reading, to compare the share against.
    pub host: Option<HostReading>,
    /// The Project's share over the window, oldest first, thinned like
    /// [`HostView::series`].
    pub series: Vec<UsagePoint>,
    /// The Project's worktrees in this host's pools.
    pub worktrees: Vec<WorktreeSlotView>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct UsagePoint {
    /// RFC 3339 UTC; the last sample the point covers.
    pub at: String,
    pub cpu: f64,
    pub memory_bytes: u64,
    pub disk_bytes: u64,
}
