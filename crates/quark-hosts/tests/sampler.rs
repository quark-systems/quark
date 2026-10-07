use std::path::Path;
use std::time::Duration;

use quark_core::fake::MemoryEventLog;
use quark_core::telemetry::HostSample;
use quark_core::{EventLog, ProjectId, Result, Seq, Telemetry};
use quark_hosts::recorder::{self, SAMPLE};
use quark_hosts::{
    HostSampler, Probe, ProcessInfo, QuarkPaths, Reading, SamplerConfig, StaticWorkloads,
    SystemProbe, Workload,
};

struct FixedProbe;

impl Probe for FixedProbe {
    fn read(&self, _: &Path) -> Result<Reading> {
        let p = |pid, parent, cpu, memory_bytes| ProcessInfo {
            pid,
            parent,
            cpu,
            memory_bytes,
        };
        Ok(Reading {
            cpu: 0.4,
            memory_used_bytes: 6,
            memory_total_bytes: 16,
            memory_pressure: None,
            disk_free_bytes: 1000,
            processes: vec![
                p(1, None, 0.0, 1),
                p(20, Some(1), 0.1, 100),
                p(21, Some(20), 0.2, 50),
            ],
        })
    }
}

#[tokio::test]
async fn samples_attribute_cpu_memory_and_disk() {
    let dir = tempfile::tempdir().unwrap();
    let wt = dir.path().join("worktrees/t1");
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(wt.join("f"), [0u8; 40]).unwrap();
    std::fs::create_dir(dir.path().join("logs")).unwrap();
    std::fs::write(dir.path().join("logs/l"), [0u8; 7]).unwrap();
    std::fs::write(dir.path().join("events.db"), [0u8; 3]).unwrap();

    let mut config = SamplerConfig::new("mac", dir.path());
    config.quark = QuarkPaths {
        worktrees: vec![dir.path().join("worktrees")],
        logs: vec![dir.path().join("logs")],
        caches: vec![dir.path().join("no-caches")],
        event_log: vec![
            dir.path().join("events.db"),
            dir.path().join("events.db-wal"),
        ],
    };
    let workloads = StaticWorkloads::new(vec![Workload {
        project: "quark".into(),
        task: Some("t1".into()),
        pids: vec![20],
        worktree: Some(wt.clone()),
    }]);
    let sampler = HostSampler::new(config, FixedProbe, workloads);

    let s = sampler.sample().await.unwrap();
    assert_eq!(s.host.as_str(), "mac");
    assert_eq!(s.cpu, 0.4);
    assert_eq!(s.memory_pressure, 0.0);
    assert_eq!(s.disk_free_bytes, 1000);
    assert_eq!(s.quark_disk.worktrees_bytes, 40);
    assert_eq!(s.quark_disk.logs_bytes, 7);
    assert_eq!(s.quark_disk.caches_bytes, 0);
    assert_eq!(s.quark_disk.event_log_bytes, 3);
    assert_eq!(s.usage.len(), 1);
    let u = &s.usage[0];
    assert_eq!(u.project.as_str(), "quark");
    assert!((u.cpu - 0.3).abs() < 1e-9);
    assert_eq!(u.memory_bytes, 150);
    assert_eq!(u.disk_bytes, 40);

    // Sizes are cached between walks.
    std::fs::write(wt.join("g"), [0u8; 60]).unwrap();
    let s = sampler.sample().await.unwrap();
    assert_eq!(s.usage[0].disk_bytes, 40);
    assert_eq!(s.quark_disk.worktrees_bytes, 40);
}

#[tokio::test]
async fn refreshes_sizes_after_disk_every() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = SamplerConfig::new("mac", dir.path());
    config.quark.logs = vec![dir.path().to_path_buf()];
    config.disk_every = Duration::ZERO;
    let sampler = HostSampler::new(config, FixedProbe, StaticWorkloads::default());

    assert_eq!(sampler.sample().await.unwrap().quark_disk.logs_bytes, 0);
    std::fs::write(dir.path().join("l"), [0u8; 9]).unwrap();
    assert_eq!(sampler.sample().await.unwrap().quark_disk.logs_bytes, 9);
}

#[tokio::test]
async fn recorder_appends_samples_to_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let sampler = HostSampler::new(
        SamplerConfig::new("mac", dir.path()),
        FixedProbe,
        StaticWorkloads::default(),
    );
    let log = MemoryEventLog::new();

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let run = recorder::run(&sampler, &log, Duration::from_millis(10), async {
        let _ = rx.await;
    });
    let stop = async {
        while log.head().await.unwrap() < Seq(3) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tx.send(()).unwrap();
    };
    tokio::join!(run, stop);

    let events = log.events();
    assert!(events.len() >= 3);
    let e = &events[0];
    assert_eq!(e.kind.as_str(), SAMPLE);
    assert_eq!(e.project, ProjectId::engine());
    assert_eq!(e.host.as_str(), "mac");
    let sample: HostSample = e.decode().unwrap();
    assert_eq!(sample.ts, e.ts);
    assert_eq!(sample.disk_free_bytes, 1000);
}

/// The real host: values are in range and this test's own process is
/// attributed.
#[tokio::test]
async fn system_probe_reads_this_host() {
    let dir = tempfile::tempdir().unwrap();
    let me = std::process::id();
    let sampler = HostSampler::new(
        SamplerConfig::new("local", dir.path()),
        SystemProbe::new(),
        StaticWorkloads::new(vec![Workload {
            project: "self".into(),
            task: None,
            pids: vec![me],
            worktree: None,
        }]),
    );
    let s = sampler.sample().await.unwrap();
    assert!((0.0..=1.0).contains(&s.cpu));
    assert!((0.0..=1.0).contains(&s.memory_pressure));
    assert!(s.memory_total_bytes > 0);
    assert!(s.memory_used_bytes <= s.memory_total_bytes);
    assert!(s.disk_free_bytes > 0);
    assert!(s.usage[0].memory_bytes > 0, "{:?}", s.usage);
}
