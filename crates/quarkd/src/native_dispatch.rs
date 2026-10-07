//! Native dispatch (slice 5) in shadow beside firstmate.
//!
//! Slice 5 cannot switch on before slices 1 to 4, so firstmate still
//! resolves and spawns every worker. With [`ENV`] set to `1`, quarkd also:
//!
//! - registers this host in the event log's host registry and records a
//!   telemetry sample every [`SAMPLE_EVERY`], which admission control will
//!   read;
//! - re-runs each dispatch resolution firstmate makes on the native
//!   resolver, with the classifier answer firstmate got and a fresh quota
//!   snapshot, and appends a `shadow.divergence` event (slice `dispatch`,
//!   operation `resolve_dispatch`) whenever the decisions differ.
//!
//! Nothing here spawns, holds or changes a worker.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use quark_core::event::kinds;
use quark_core::host::{Capacity, Health, Host, HostRegistry, Platform, RuntimeKind};
use quark_core::slice::Divergence;
use quark_core::{EventLog, HostId, NewEvent, ProjectId, Slice, TaskId};
use quark_dispatch::shadow::{self, Observed};
use quark_dispatch::{ClassifierSettings, DispatchConfig, ProviderFamilies, QuotaAxi, QuotaSource};
use quark_hosts::{EventHosts, HostSampler, Probe, SamplerConfig, SystemProbe, Workloads};
use quark_systems::DispatchStatus;

use crate::config::Config;
use crate::engine::{EngineResolution, WorkspaceRef};

/// Set to `1` to run native dispatch in shadow; unset, it follows
/// `QUARK_SHADOWS=all`.
pub const ENV: &str = "QUARK_NATIVE_DISPATCH";
/// Most workers admission control will place on this host; unset or `0`
/// for no cap.
pub const MAX_WORKERS_ENV: &str = "QUARK_MAX_WORKERS";
/// How often this host is sampled.
pub const SAMPLE_EVERY: Duration = Duration::from_secs(60);
/// How often directory sizes are walked again.
pub const DISK_EVERY: Duration = Duration::from_secs(300);

pub fn enabled() -> bool {
    crate::shadows::opt_in(ENV)
}

/// Compares firstmate's dispatch resolutions with the native resolver's.
pub struct DispatchShadow {
    log: Arc<dyn EventLog>,
    host: HostId,
    quota: Arc<dyn QuotaSource>,
    families: ProviderFamilies,
}

impl DispatchShadow {
    pub fn new(config: &Config, log: Arc<dyn EventLog>) -> Self {
        let mut quota = QuotaAxi::new(&config.quota_axi);
        // firstmate's local readers for providers quota-axi does not report.
        let bin = config.engine_root().join("bin");
        for (provider, script) in [("bob", "fm-bob-quota.sh"), ("kiro", "fm-kiro-quota.sh")] {
            quota.local.insert(
                provider.into(),
                vec![
                    bin.join(script).to_string_lossy().into_owned(),
                    "reading".into(),
                ],
            );
        }
        let (custom, _) = quark_harness::load_dir(&config.home.join("harnesses"));
        let families = ProviderFamilies::from_manifests(
            quark_harness::builtin()
                .iter()
                .map(|m| m.as_ref())
                .chain(custom.iter()),
        );
        Self {
            log,
            host: crate::event_ingest::host(),
            quota: Arc::new(quota),
            families,
        }
    }

    /// Re-run `bash`'s resolution natively and record any disagreement.
    pub async fn compare(&self, ws: &WorkspaceRef, task: Option<&str>, bash: &EngineResolution) {
        let observed = observed(bash);
        let path = ws.root.join("config").join("crew-dispatch.json");
        let config = match tokio::fs::read_to_string(&path).await {
            Ok(text) => DispatchConfig::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DispatchConfig::default()),
            Err(e) => Err(e.to_string()),
        };
        let native = match config {
            Err(e) => quark_dispatch::Resolution::error(format!("dispatch rules: {e}")),
            Ok(config) => {
                let key =
                    std::env::var(quark_dispatch::classify::KEY_ENV).is_ok_and(|k| !k.is_empty());
                let settings = ClassifierSettings::from_config(&config, key);
                let classification = settings
                    .on
                    .then(|| shadow::classification(&observed))
                    .flatten();
                let quota = match &classification {
                    Some(_) if !config.rules.is_empty() => {
                        let providers = quark_dispatch::resolve::providers(&config, &self.families);
                        self.quota.snapshot(&providers).await
                    }
                    _ => Ok(Default::default()),
                };
                quark_dispatch::resolve(
                    &config,
                    &settings,
                    classification.as_ref(),
                    quota.as_ref().map_err(String::as_str),
                    &self.families,
                )
            }
        };
        let Some((mut b, mut n)) = shadow::compare(&observed, &native) else {
            return;
        };
        b["reason"] = observed.reason.clone().into();
        n["reason"] = native.reason.clone().into();
        n["notes"] = native.notes.clone().into();
        let d = Divergence {
            slice: Slice::Dispatch,
            operation: "resolve_dispatch".into(),
            bash: b,
            native: n,
        };
        let event = NewEvent::typed(
            self.host.clone(),
            ProjectId::new(&ws.project_id),
            task.map(TaskId::from),
            kinds::SHADOW_DIVERGENCE,
            &d,
        );
        let result = match event {
            Ok(e) => self.log.append(e).await.map(drop),
            Err(e) => Err(e),
        };
        match result {
            Ok(()) => tracing::info!(project = %ws.project_id, task, "native dispatch diverged"),
            Err(e) => tracing::warn!(error = %e, "could not record a dispatch divergence"),
        }
    }
}

fn observed(r: &EngineResolution) -> Observed {
    let status = match r.status {
        DispatchStatus::Clear => "clear",
        DispatchStatus::Ambiguous => "ambiguous",
        DispatchStatus::Escalate => "escalate",
        DispatchStatus::Error | DispatchStatus::NotConsulted => "error",
        DispatchStatus::Off => "off",
    };
    Observed {
        status: status.into(),
        reason: r.reason.clone(),
        consulted: r.classifier_consulted,
        rule: r.rule.as_ref().map(|x| x.id.clone()),
        confidence: r.confidence,
        classifier_model: r.classifier_model.clone(),
        fallback: r.fallback.clone(),
        profile: r
            .profile
            .as_ref()
            .map(|p| (p.harness.clone(), p.model.clone(), p.effort.clone())),
        candidates: r
            .candidates
            .iter()
            .map(|c| (c.harness.clone(), c.model.clone(), c.passed))
            .collect(),
    }
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
        let host = crate::event_ingest::host();
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

#[cfg(test)]
mod tests {
    use super::*;
    use quark_core::fake::MemoryEventLog;
    use quark_systems::{DispatchCandidate, DispatchChoice, DispatchRule};

    fn bash(harness: &str) -> EngineResolution {
        EngineResolution {
            status: DispatchStatus::Clear,
            rule: Some(DispatchRule {
                id: "rule_1".into(),
                when: Some("x".into()),
            }),
            reason: None,
            notes: Vec::new(),
            candidates: vec![DispatchCandidate {
                harness: "claude".into(),
                model: None,
                passed: true,
                reason: "eligible".into(),
                evidence: None,
            }],
            profile: Some(DispatchChoice {
                harness: harness.into(),
                model: None,
                effort: None,
                account: None,
            }),
            fallback: None,
            classifier_consulted: true,
            classifier_model: Some("jev-1".into()),
            confidence: Some(0.9),
            output: None,
        }
    }

    #[tokio::test]
    async fn records_only_disagreements() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("config")).unwrap();
        std::fs::write(
            dir.path().join("config/crew-dispatch.json"),
            r#"{"classifier":{"provider":"system1"},"rules":[{"when":"x","use":{"harness":"claude","pricing":"budget"}}]}"#,
        )
        .unwrap();
        let log = MemoryEventLog::new();
        let shadow = DispatchShadow {
            log: Arc::new(log.clone()),
            host: "local".into(),
            quota: Arc::new(quark_dispatch::FixedQuota(Some(Default::default()))),
            families: ProviderFamilies::builtin(),
        };
        let ws = WorkspaceRef {
            project_id: "p".into(),
            root: dir.path().to_path_buf(),
        };
        shadow.compare(&ws, Some("t1"), &bash("claude")).await;
        assert!(log.events().is_empty());
        shadow.compare(&ws, Some("t1"), &bash("codex")).await;
        let events = log.events();
        assert_eq!(events.len(), 1);
        let d: Divergence = events[0].decode().unwrap();
        assert_eq!(d.slice, Slice::Dispatch);
        assert_eq!(d.bash["profile"]["harness"], "codex");
        assert_eq!(d.native["profile"]["harness"], "claude");
    }
}
