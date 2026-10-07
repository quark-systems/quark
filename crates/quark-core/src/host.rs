//! Hosts and the runtimes that reach them.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{HostId, ProjectId, Result, TaskId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeKind {
    Local,
    Ssh,
    Hosted,
    PrivateCloud,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Platform {
    /// `macos` or `linux`.
    pub os: String,
    pub arch: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capacity {
    pub cpus: u32,
    pub memory_bytes: u64,
    pub disk_bytes: u64,
    /// Most workers admission control places here at once.
    pub max_workers: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Health {
    Healthy,
    Degraded { reason: String },
    Unreachable { reason: String },
}

/// A registered host and what runs on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Host {
    pub id: HostId,
    pub name: String,
    pub runtime: RuntimeKind,
    pub platform: Platform,
    pub capacity: Capacity,
    pub health: Health,
    pub projects: Vec<ProjectId>,
    pub tasks: Vec<TaskId>,
}

/// Reaches one host: local, over SSH, or a hosted or private-cloud runtime.
/// Session backends, worktree providers and isolation are obtained per host
/// through it by the crates that implement them.
#[async_trait]
pub trait Runtime: Send + Sync {
    fn kind(&self) -> RuntimeKind;

    fn host(&self) -> &HostId;

    async fn health(&self) -> Result<Health>;
}

/// Every host Quark knows.
#[async_trait]
pub trait HostRegistry: Send + Sync {
    async fn hosts(&self) -> Result<Vec<Host>>;

    async fn register(&self, host: Host) -> Result<()>;

    async fn set_health(&self, id: &HostId, health: Health) -> Result<()>;
}
