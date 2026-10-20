//! This host in the native event log's host registry, sampled on a timer
//! for the Hosts view and admission control ([`HostTelemetry`]), and the
//! provider families native dispatch reads quota for ([`families`]).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use quark_core::host::{Capacity, Health, Host, HostRegistry, Platform, RuntimeKind};
use quark_core::EventLog;
use quark_dispatch::local::{BobUsage, KiroUsage};
use quark_dispatch::{ProviderFamilies, QuotaAxi};
use quark_hosts::{EventHosts, HostSampler, Probe, SamplerConfig, SystemProbe, Workloads};

use crate::config::Config;

/// Most workers admission control will place on this host; unset or `0`
/// for no cap.
pub const MAX_WORKERS_ENV: &str = "QUARK_MAX_WORKERS";
/// How often this host is sampled.
pub const SAMPLE_EVERY: Duration = Duration::from_secs(60);
/// How often directory sizes are walked again.
pub const DISK_EVERY: Duration = Duration::from_secs(300);

/// Each harness's quota provider family: the built-in manifests and those
/// in `<home>/harnesses`.
pub fn families(config: &Config) -> ProviderFamilies {
    let (custom, _) = quark_harness::load_dir(&config.home.join("harnesses"));
    ProviderFamilies::from_manifests(
        quark_harness::builtin()
            .iter()
            .map(|m| m.as_ref())
            .chain(custom.iter()),
    )
}

/// Native dispatch's quota evidence: `quota-axi --json`, plus IBM Bob's and
/// Kiro CLI's own readings, which quota-axi does not report. Their settings
/// live in `<home>/quota` (`docs/engine/dispatch.md`).
pub fn quota(config: &Config) -> QuotaAxi {
    let user_home = std::env::var_os("HOME").map(PathBuf::from);
    QuotaAxi::new(&config.quota_axi)
        .with_reading(Arc::new(BobUsage::for_home(
            &config.home,
            user_home.as_deref(),
        )))
        .with_reading(Arc::new(KiroUsage::for_home(&config.home)))
}

/// This host in the registry, sampled on a timer.
pub struct HostTelemetry {
    task: tokio::task::JoinHandle<()>,
}

impl HostTelemetry {
    /// Registers this host and samples it, attributing usage with
    /// `workloads`.
    pub async fn start(
        config: &Config,
        log: Arc<dyn EventLog>,
        workloads: impl Workloads + 'static,
    ) -> anyhow::Result<Self> {
        let host = crate::host();
        let home = config.home.clone();
        let (probe, memory) = tokio::task::spawn_blocking(move || {
            let probe = SystemProbe::new();
            let memory = probe.read(&home).map(|r| r.memory_total_bytes).unwrap_or(0);
            (probe, memory)
        })
        .await?;
        let max_workers = std::env::var(MAX_WORKERS_ENV)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let registry = EventHosts::new(log.clone(), host.clone());
        registry
            .register(Host {
                id: host.clone(),
                name: hostname(),
                runtime: RuntimeKind::Local,
                platform: Platform {
                    os: std::env::consts::OS.into(),
                    arch: std::env::consts::ARCH.into(),
                },
                capacity: Capacity {
                    cpus: std::thread::available_parallelism().map_or(0, |n| n.get() as u32),
                    memory_bytes: memory,
                    disk_bytes: 0,
                    max_workers,
                },
                health: Health::Healthy,
                projects: Vec::new(),
                tasks: Vec::new(),
            })
            .await?;
        let mut sc = SamplerConfig::new(host, config.home.clone());
        // Sizing task worktrees walks every file in them.
        sc.disk_every = DISK_EVERY;
        sc.quark.logs = vec![config.home.join("logs")];
        let events = config.events_path();
        sc.quark.event_log = ["", "-wal", "-shm"]
            .iter()
            .map(|s| PathBuf::from(format!("{}{s}", events.display())))
            .collect();
        let sampler = HostSampler::new(sc, probe, workloads);
        let task = tokio::spawn(async move {
            quark_hosts::recorder::run(&sampler, log.as_ref(), SAMPLE_EVERY, std::future::pending())
                .await
        });
        Ok(Self { task })
    }

    pub fn stop(self) {
        self.task.abort();
    }
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|h| h.trim().to_string())
                .filter(|h| !h.is_empty())
        })
        .unwrap_or_else(|| "this machine".into())
}
