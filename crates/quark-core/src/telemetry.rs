//! Sampled host resource telemetry, attributed per Project and task.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{HostId, ProjectId, Result, TaskId};

/// One sample of a host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostSample {
    pub host: HostId,
    #[serde(with = "time::serde::rfc3339")]
    pub ts: OffsetDateTime,
    /// 0.0 to 1.0 across all cores.
    pub cpu: f64,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    /// 0.0 (none) to 1.0 (critical), as the OS reports pressure.
    pub memory_pressure: f64,
    pub disk_free_bytes: u64,
    /// Disk used by Quark, broken down.
    pub quark_disk: QuarkDisk,
    pub usage: Vec<Usage>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuarkDisk {
    pub worktrees_bytes: u64,
    pub logs_bytes: u64,
    pub caches_bytes: u64,
    pub event_log_bytes: u64,
}

/// One Project's or task's share of a sample, from the worker's process
/// tree and its worktree path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub project: ProjectId,
    /// `None` for the Project's coordinator.
    pub task: Option<TaskId>,
    pub cpu: f64,
    pub memory_bytes: u64,
    pub disk_bytes: u64,
}

/// Samples a host. Admission control reads the latest sample; the
/// dashboard reads the series from the event log.
#[async_trait]
pub trait Telemetry: Send + Sync {
    async fn sample(&self) -> Result<HostSample>;
}
