//! [`HostSampler`], the [`Telemetry`] implementation.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use quark_core::telemetry::{HostSample, QuarkDisk, Usage};
use quark_core::{CoreError, HostId, Result, Telemetry};
use time::OffsetDateTime;

use crate::attribution::{attribute, Workloads};
use crate::disk::tree_size;
use crate::probe::Probe;

/// The paths Quark fills, by category. Each entry may be a directory or a
/// file; missing ones count as 0.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuarkPaths {
    /// Worktree roots and pools.
    pub worktrees: Vec<PathBuf>,
    /// Logs and transcripts.
    pub logs: Vec<PathBuf>,
    pub caches: Vec<PathBuf>,
    /// The event log's database and its `-wal` and `-shm` files.
    pub event_log: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct SamplerConfig {
    pub host: HostId,
    /// Free disk is read for the filesystem holding this path, normally
    /// Quark's home.
    pub disk_root: PathBuf,
    pub quark: QuarkPaths,
    /// How long directory sizes are reused before being walked again.
    /// Walking worktrees is far slower than reading CPU and memory.
    pub disk_every: Duration,
}

impl SamplerConfig {
    /// Free disk read at `disk_root`, no Quark paths, sizes refreshed every
    /// minute.
    pub fn new(host: impl Into<HostId>, disk_root: impl Into<PathBuf>) -> Self {
        Self {
            host: host.into(),
            disk_root: disk_root.into(),
            quark: QuarkPaths::default(),
            disk_every: Duration::from_secs(60),
        }
    }
}

#[derive(Default)]
struct DiskCache {
    at: Option<Instant>,
    quark: QuarkDisk,
    worktrees: HashMap<PathBuf, u64>,
}

/// Samples one host: system readings from a [`Probe`], attribution from a
/// [`Workloads`] source.
pub struct HostSampler<P, W> {
    config: SamplerConfig,
    probe: Arc<P>,
    workloads: W,
    disk: Arc<Mutex<DiskCache>>,
}

impl<P: Probe, W: Workloads> HostSampler<P, W> {
    pub fn new(config: SamplerConfig, probe: P, workloads: W) -> Self {
        Self {
            config,
            probe: Arc::new(probe),
            workloads,
            disk: Arc::default(),
        }
    }

    /// Quark's disk use and each worktree's size, from the cache unless it
    /// is older than `disk_every`. A worktree not seen before is sized now.
    async fn disk(&self, worktrees: Vec<PathBuf>) -> Result<(QuarkDisk, HashMap<PathBuf, u64>)> {
        let cache = self.disk.clone();
        let paths = self.config.quark.clone();
        let every = self.config.disk_every;
        blocking(move || {
            let mut c = cache.lock().unwrap();
            let stale = c.at.is_none_or(|at| at.elapsed() >= every);
            if stale {
                c.quark = QuarkDisk {
                    worktrees_bytes: sum(&paths.worktrees),
                    logs_bytes: sum(&paths.logs),
                    caches_bytes: sum(&paths.caches),
                    event_log_bytes: sum(&paths.event_log),
                };
                c.worktrees.clear();
                c.at = Some(Instant::now());
            }
            let mut sizes = HashMap::new();
            for w in worktrees {
                let size = *c
                    .worktrees
                    .entry(w.clone())
                    .or_insert_with(|| tree_size(&w));
                sizes.insert(w, size);
            }
            // Forget worktrees no longer in use.
            c.worktrees.retain(|k, _| sizes.contains_key(k));
            Ok((c.quark, sizes))
        })
        .await
    }
}

fn sum(paths: &[PathBuf]) -> u64 {
    paths.iter().map(|p| tree_size(p)).sum()
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| CoreError::Backend(format!("telemetry task: {e}")))?
}

#[async_trait]
impl<P: Probe, W: Workloads> Telemetry for HostSampler<P, W> {
    async fn sample(&self) -> Result<HostSample> {
        let workloads = self.workloads.current().await?;
        let probe = self.probe.clone();
        let root = self.config.disk_root.clone();
        let reading = blocking(move || probe.read(&root)).await?;
        let worktrees = workloads
            .iter()
            .filter_map(|w| w.worktree.clone())
            .collect();
        let (quark_disk, sizes) = self.disk(worktrees).await?;

        let usage = attribute(&reading.processes, &workloads)
            .into_iter()
            .zip(workloads)
            .map(|((cpu, memory_bytes), w)| Usage {
                disk_bytes: w
                    .worktree
                    .as_ref()
                    .and_then(|p| sizes.get(p))
                    .copied()
                    .unwrap_or(0),
                project: w.project,
                task: w.task,
                cpu,
                memory_bytes,
            })
            .collect();

        Ok(HostSample {
            host: self.config.host.clone(),
            ts: OffsetDateTime::now_utc(),
            cpu: reading.cpu,
            memory_used_bytes: reading.memory_used_bytes,
            memory_total_bytes: reading.memory_total_bytes,
            // Hosts without a pressure signal report none rather than
            // guessing from used memory.
            memory_pressure: reading.memory_pressure.unwrap_or(0.0),
            disk_free_bytes: reading.disk_free_bytes,
            quark_disk,
            usage,
        })
    }
}
