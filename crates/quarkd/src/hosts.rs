//! The Hosts view and each Project's slice of its hosts, read from the
//! native event log.
//!
//! [`HostsModel`] folds three event kinds from `events.db`, catching up from
//! where it stopped on every read:
//!
//! - `host.change` ([`quark_hosts::registry::HOST`]): registrations and
//!   health changes;
//! - `telemetry.sample` ([`quark_hosts::recorder::SAMPLE`]): one sample of a
//!   host with each Project's and task's share, kept for [`RETAIN`];
//! - `host.worktrees` ([`WORKTREES`]): the host's worktree pools, recorded
//!   by [`crate::host_sources`] whenever they change.
//!
//! Everything about a host comes from the log, so a host another daemon
//! records into a replicated log shows up the same way as this one.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use quark_core::host::{Health, Host, RuntimeKind};
use quark_core::telemetry::{HostSample, Usage};
use quark_core::worktree::{Holder, SlotState};
use quark_core::{Event, EventLog, Result, Seq};
use quark_eventlog::SqliteEventLog;
use quark_hosts::registry::HostChange;
use quark_systems::{
    HostCapacity, HostHealth, HostHealthStatus, HostPoint, HostReading, HostRuntime, HostView,
    HostWorktrees, HostsView, ProjectHostSlice, ProjectHosts, ProjectUsage, QuarkDiskUse,
    UsagePart, UsagePoint, WorktreeSlotState, WorktreeSlotView, SERIES_POINTS,
};
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

/// Event kind of a host's worktree pool status. Payload: [`PoolReport`].
/// Recorded under the engine project, only when the status changed.
pub const WORKTREES: &str = "host.worktrees";
/// Samples older than this (behind a host's newest) are dropped.
pub const RETAIN: Duration = Duration::hours(48);
/// Longest window a read can ask for, in hours.
pub const MAX_HOURS: u32 = 48;
const READ_BATCH: usize = 1000;

/// Payload of a [`WORKTREES`] event.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolReport {
    pub slots: Vec<PoolSlot>,
    /// Why some pool could not be read; `slots` then lists only those that
    /// could.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolSlot {
    pub path: PathBuf,
    pub repo: PathBuf,
    pub state: SlotState,
    pub holder: Option<Holder>,
    /// The Project whose workspace the pool belongs to.
    pub project: Option<String>,
}

#[derive(Default)]
struct HostFold {
    host: Option<Host>,
    health_since: Option<OffsetDateTime>,
    samples: VecDeque<HostSample>,
    pools: Option<(OffsetDateTime, PoolReport)>,
}

/// Every host's fold of one event log.
pub struct HostsModel {
    log: SqliteEventLog,
    after: Seq,
    hosts: BTreeMap<String, HostFold>,
}

/// Names the API layer fills in: Project names, and Quark task ids and
/// titles by Project and engine task id.
#[derive(Default)]
pub struct Names {
    pub projects: HashMap<String, String>,
    pub tasks: HashMap<(String, String), (String, String)>,
}

impl Names {
    fn part(&self, project: &str, u: &Usage) -> UsagePart {
        let engine_task = u.task.as_ref().map(|t| t.as_str().to_string());
        let known = engine_task
            .as_ref()
            .and_then(|t| self.tasks.get(&(project.to_string(), t.clone())));
        UsagePart {
            task_id: known.map(|k| k.0.clone()),
            title: known.map(|k| k.1.clone()),
            engine_task,
            cpu: u.cpu,
            memory_bytes: u.memory_bytes,
            disk_bytes: u.disk_bytes,
        }
    }
}

impl HostsModel {
    pub fn new(log: SqliteEventLog) -> Self {
        Self {
            log,
            after: Seq::ZERO,
            hosts: BTreeMap::new(),
        }
    }

    /// Fold everything appended since the last call.
    pub async fn catch_up(&mut self) -> Result<()> {
        loop {
            let events = self.log.read(self.after, READ_BATCH).await?;
            let Some(last) = events.last() else {
                return Ok(());
            };
            self.after = last.seq;
            for e in &events {
                self.apply(e);
            }
        }
    }

    fn apply(&mut self, e: &Event) {
        match e.kind.as_str() {
            quark_hosts::registry::HOST => match e.decode::<HostChange>() {
                Ok(HostChange::Registered { host }) => {
                    let f = self.hosts.entry(host.id.as_str().to_string()).or_default();
                    if f.host.as_ref().map(|h| &h.health) != Some(&host.health) {
                        f.health_since = Some(e.ts);
                    }
                    f.host = Some(host);
                }
                Ok(HostChange::Health { host, health }) => {
                    let f = self.hosts.entry(host.as_str().to_string()).or_default();
                    if let Some(h) = &mut f.host {
                        if h.health != health {
                            f.health_since = Some(e.ts);
                        }
                        h.health = health;
                    }
                }
                Err(err) => tracing::warn!(error = %err, "unreadable host event"),
            },
            quark_hosts::recorder::SAMPLE => match e.decode::<HostSample>() {
                Ok(s) => {
                    let f = self.hosts.entry(s.host.as_str().to_string()).or_default();
                    // Samples arrive in time order from one recorder; one
                    // that does not is placed where it belongs.
                    let at = f.samples.partition_point(|x| x.ts <= s.ts);
                    f.samples.insert(at, s);
                    let newest = f.samples.back().map(|x| x.ts).unwrap();
                    while f.samples.front().is_some_and(|x| x.ts < newest - RETAIN) {
                        f.samples.pop_front();
                    }
                }
                Err(err) => tracing::warn!(error = %err, "unreadable telemetry sample"),
            },
            WORKTREES => match e.decode::<PoolReport>() {
                Ok(r) => {
                    let f = self.hosts.entry(e.host.as_str().to_string()).or_default();
                    f.pools = Some((e.ts, r));
                }
                Err(err) => tracing::warn!(error = %err, "unreadable worktree report"),
            },
            _ => {}
        }
    }

    /// The Hosts view over the last `hours`.
    pub fn view(&self, hours: u32, now: OffsetDateTime, names: &Names) -> HostsView {
        let hours = hours.clamp(1, MAX_HOURS);
        let from = now - Duration::hours(hours.into());
        let hosts = self
            .hosts
            .iter()
            .map(|(id, f)| {
                let latest = f.samples.back();
                let mut projects: Vec<ProjectUsage> = latest
                    .map(|s| {
                        let mut ids: Vec<&str> =
                            s.usage.iter().map(|u| u.project.as_str()).collect();
                        ids.sort_unstable();
                        ids.dedup();
                        ids.into_iter()
                            .filter_map(|p| project_usage(s, p, names))
                            .collect()
                    })
                    .unwrap_or_default();
                projects.sort_by_key(|p| std::cmp::Reverse(p.memory_bytes));
                let window: Vec<&HostSample> = f.samples.iter().filter(|s| s.ts >= from).collect();
                HostView {
                    id: id.clone(),
                    name: f
                        .host
                        .as_ref()
                        .map_or_else(|| id.clone(), |h| h.name.clone()),
                    runtime: f
                        .host
                        .as_ref()
                        .map_or(HostRuntime::Local, |h| runtime(h.runtime)),
                    os: f
                        .host
                        .as_ref()
                        .map(|h| h.platform.os.clone())
                        .unwrap_or_default(),
                    arch: f
                        .host
                        .as_ref()
                        .map(|h| h.platform.arch.clone())
                        .unwrap_or_default(),
                    capacity: capacity(f),
                    health: health(f),
                    latest: latest.map(reading),
                    series: thin(&window, host_point),
                    projects,
                    worktrees: f.pools.as_ref().map(|(at, r)| worktrees(*at, r)),
                }
            })
            .collect();
        HostsView {
            hours,
            hosts,
            error: None,
        }
    }

    /// `project`'s slice of every host it used in the last `hours` or has
    /// worktrees on.
    pub fn project(
        &self,
        project: &str,
        hours: u32,
        now: OffsetDateTime,
        names: &Names,
    ) -> ProjectHosts {
        let hours = hours.clamp(1, MAX_HOURS);
        let from = now - Duration::hours(hours.into());
        let hosts = self
            .hosts
            .iter()
            .filter_map(|(id, f)| {
                let window: Vec<&HostSample> = f.samples.iter().filter(|s| s.ts >= from).collect();
                let used = window
                    .iter()
                    .any(|s| s.usage.iter().any(|u| u.project.as_str() == project));
                let slots: Vec<WorktreeSlotView> = f
                    .pools
                    .iter()
                    .flat_map(|(_, r)| r.slots.iter())
                    .filter(|s| s.project.as_deref() == Some(project))
                    .map(slot_view)
                    .collect();
                if !used && slots.is_empty() {
                    return None;
                }
                let latest = f.samples.back();
                Some(ProjectHostSlice {
                    host_id: id.clone(),
                    name: f
                        .host
                        .as_ref()
                        .map_or_else(|| id.clone(), |h| h.name.clone()),
                    health: health(f),
                    capacity: capacity(f),
                    now: latest.and_then(|s| project_usage(s, project, names)),
                    host: latest.map(reading),
                    series: thin(&window, |s: &HostSample| {
                        let (cpu, memory_bytes, disk_bytes) = share(s, project);
                        UsagePoint {
                            at: rfc3339(s.ts),
                            cpu,
                            memory_bytes,
                            disk_bytes,
                        }
                    }),
                    worktrees: slots,
                })
            })
            .collect();
        ProjectHosts {
            project_id: project.to_string(),
            hours,
            hosts,
            error: None,
        }
    }
}

/// The model for `log`, shared by every request in this process that reads
/// the same file, so each one only folds what is new. An in-memory log gets
/// a fresh model each time.
pub fn shared(log: &SqliteEventLog) -> Arc<tokio::sync::Mutex<HostsModel>> {
    type Models = Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<HostsModel>>>>;
    static MODELS: OnceLock<Models> = OnceLock::new();
    let fresh = || Arc::new(tokio::sync::Mutex::new(HostsModel::new(log.clone())));
    let path = log.path();
    if path.as_os_str().is_empty() || path == Path::new(":memory:") {
        return fresh();
    }
    MODELS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .entry(path.to_path_buf())
        .or_insert_with(fresh)
        .clone()
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_default()
}

fn runtime(k: RuntimeKind) -> HostRuntime {
    match k {
        RuntimeKind::Local => HostRuntime::Local,
        RuntimeKind::Ssh => HostRuntime::Ssh,
        RuntimeKind::Hosted => HostRuntime::Hosted,
        RuntimeKind::PrivateCloud => HostRuntime::PrivateCloud,
    }
}

fn capacity(f: &HostFold) -> HostCapacity {
    let c = f.host.as_ref().map(|h| h.capacity).unwrap_or_default();
    HostCapacity {
        cpus: c.cpus,
        memory_bytes: c.memory_bytes,
        disk_bytes: c.disk_bytes,
        max_workers: c.max_workers,
    }
}

fn health(f: &HostFold) -> HostHealth {
    let since = f.health_since.map(rfc3339);
    match f.host.as_ref().map(|h| &h.health) {
        Some(Health::Healthy) => HostHealth {
            status: HostHealthStatus::Healthy,
            reason: None,
            since,
        },
        Some(Health::Degraded { reason }) => HostHealth {
            status: HostHealthStatus::Degraded,
            reason: Some(reason.clone()),
            since,
        },
        Some(Health::Unreachable { reason }) => HostHealth {
            status: HostHealthStatus::Unreachable,
            reason: Some(reason.clone()),
            since,
        },
        // Sampled but never registered: it reports, so it is reachable.
        None => HostHealth {
            status: HostHealthStatus::Healthy,
            reason: Some("not registered".into()),
            since: None,
        },
    }
}

fn reading(s: &HostSample) -> HostReading {
    HostReading {
        at: rfc3339(s.ts),
        cpu: s.cpu,
        memory_used_bytes: s.memory_used_bytes,
        memory_total_bytes: s.memory_total_bytes,
        memory_pressure: s.memory_pressure,
        disk_free_bytes: s.disk_free_bytes,
        quark_disk: QuarkDiskUse {
            worktrees_bytes: s.quark_disk.worktrees_bytes,
            logs_bytes: s.quark_disk.logs_bytes,
            caches_bytes: s.quark_disk.caches_bytes,
            event_log_bytes: s.quark_disk.event_log_bytes,
        },
    }
}

fn host_point(s: &HostSample) -> HostPoint {
    HostPoint {
        at: rfc3339(s.ts),
        cpu: s.cpu,
        memory_used_bytes: s.memory_used_bytes,
        memory_pressure: s.memory_pressure,
        disk_free_bytes: s.disk_free_bytes,
    }
}

/// `project`'s total CPU, memory and disk in `s`.
fn share(s: &HostSample, project: &str) -> (f64, u64, u64) {
    s.usage
        .iter()
        .filter(|u| u.project.as_str() == project)
        .fold((0.0, 0, 0), |(c, m, d), u| {
            (c + u.cpu, m + u.memory_bytes, d + u.disk_bytes)
        })
}

fn project_usage(s: &HostSample, project: &str, names: &Names) -> Option<ProjectUsage> {
    let mut parts: Vec<UsagePart> = s
        .usage
        .iter()
        .filter(|u| u.project.as_str() == project)
        .map(|u| names.part(project, u))
        .collect();
    if parts.is_empty() {
        return None;
    }
    parts.sort_by(|a, b| {
        (a.engine_task.is_some(), std::cmp::Reverse(a.memory_bytes))
            .cmp(&(b.engine_task.is_some(), std::cmp::Reverse(b.memory_bytes)))
    });
    let (cpu, memory_bytes, disk_bytes) = share(s, project);
    Some(ProjectUsage {
        project_id: project.to_string(),
        project_name: names.projects.get(project).cloned(),
        cpu: cpu.min(1.0),
        memory_bytes,
        disk_bytes,
        parts,
    })
}

/// At most [`SERIES_POINTS`] points: consecutive samples are grouped evenly
/// and each group becomes the mean of its points, stamped with its last.
fn thin<T: Mean>(samples: &[&HostSample], point: impl Fn(&HostSample) -> T) -> Vec<T> {
    if samples.is_empty() {
        return Vec::new();
    }
    let per = samples.len().div_ceil(SERIES_POINTS);
    samples
        .chunks(per)
        .map(|c| T::mean(c.iter().map(|s| point(s)).collect()))
        .collect()
}

trait Mean: Sized {
    /// The mean of `points`, at the last one's time. Never empty.
    fn mean(points: Vec<Self>) -> Self;
}

fn avg_u(xs: impl Iterator<Item = u64>, n: usize) -> u64 {
    (xs.map(u128::from).sum::<u128>() / n as u128) as u64
}

impl Mean for HostPoint {
    fn mean(mut p: Vec<Self>) -> Self {
        let n = p.len();
        let fl = n as f64;
        let cpu = p.iter().map(|x| x.cpu).sum::<f64>() / fl;
        let memory_pressure = p.iter().map(|x| x.memory_pressure).sum::<f64>() / fl;
        let memory_used_bytes = avg_u(p.iter().map(|x| x.memory_used_bytes), n);
        let disk_free_bytes = avg_u(p.iter().map(|x| x.disk_free_bytes), n);
        let at = p.pop().unwrap().at;
        HostPoint {
            at,
            cpu,
            memory_used_bytes,
            memory_pressure,
            disk_free_bytes,
        }
    }
}

impl Mean for UsagePoint {
    fn mean(mut p: Vec<Self>) -> Self {
        let n = p.len();
        let cpu = p.iter().map(|x| x.cpu).sum::<f64>() / n as f64;
        let memory_bytes = avg_u(p.iter().map(|x| x.memory_bytes), n);
        let disk_bytes = avg_u(p.iter().map(|x| x.disk_bytes), n);
        let at = p.pop().unwrap().at;
        UsagePoint {
            at,
            cpu,
            memory_bytes,
            disk_bytes,
        }
    }
}

fn slot_state(s: SlotState) -> WorktreeSlotState {
    match s {
        SlotState::Idle => WorktreeSlotState::Idle,
        SlotState::InUse => WorktreeSlotState::InUse,
        SlotState::Dirty => WorktreeSlotState::Dirty,
        SlotState::Leased => WorktreeSlotState::Leased,
        SlotState::Quarantined => WorktreeSlotState::Quarantined,
    }
}

fn slot_view(s: &PoolSlot) -> WorktreeSlotView {
    WorktreeSlotView {
        path: s.path.display().to_string(),
        repo: s.repo.display().to_string(),
        state: slot_state(s.state),
        holder: s.holder.as_ref().map(|h| match h {
            Holder::Task { task } => task.as_str().to_string(),
            Holder::Lease { owner } => owner.clone(),
        }),
        project_id: s.project.clone(),
    }
}

fn worktrees(at: OffsetDateTime, r: &PoolReport) -> HostWorktrees {
    let count = |st: SlotState| r.slots.iter().filter(|s| s.state == st).count() as u32;
    HostWorktrees {
        at: rfc3339(at),
        idle: count(SlotState::Idle),
        in_use: count(SlotState::InUse),
        dirty: count(SlotState::Dirty),
        leased: count(SlotState::Leased),
        quarantined: count(SlotState::Quarantined),
        error: r.error.clone(),
        slots: r.slots.iter().map(slot_view).collect(),
    }
}

#[cfg(test)]
mod tests {
    use quark_core::host::{Capacity, Platform};
    use quark_core::telemetry::QuarkDisk;
    use quark_core::{HostId, NewEvent, ProjectId};

    use super::*;

    fn at(min: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap() + Duration::minutes(min)
    }

    fn sample(host: &str, min: i64, usage: Vec<Usage>) -> HostSample {
        HostSample {
            host: host.into(),
            ts: at(min),
            cpu: 0.5,
            memory_used_bytes: 4 << 30,
            memory_total_bytes: 16 << 30,
            memory_pressure: 0.1,
            disk_free_bytes: 100 << 30,
            quark_disk: QuarkDisk {
                worktrees_bytes: 10,
                ..Default::default()
            },
            usage,
        }
    }

    fn usage(project: &str, task: Option<&str>, mem: u64) -> Usage {
        Usage {
            project: ProjectId::new(project),
            task: task.map(Into::into),
            cpu: 0.1,
            memory_bytes: mem,
            disk_bytes: 5,
        }
    }

    fn host(id: &str, health: Health) -> Host {
        Host {
            id: id.into(),
            name: format!("{id} machine"),
            runtime: RuntimeKind::Local,
            platform: Platform {
                os: "macos".into(),
                arch: "aarch64".into(),
            },
            capacity: Capacity {
                cpus: 8,
                memory_bytes: 16 << 30,
                disk_bytes: 0,
                max_workers: 4,
            },
            health,
            projects: Vec::new(),
            tasks: Vec::new(),
        }
    }

    async fn model(events: Vec<NewEvent>) -> HostsModel {
        let dir = tempfile::tempdir().unwrap();
        let log = SqliteEventLog::open(dir.path().join("events.db")).unwrap();
        for e in events {
            log.append(e).await.unwrap();
        }
        let mut m = HostsModel::new(log);
        m.catch_up().await.unwrap();
        m
    }

    fn change(c: &HostChange) -> NewEvent {
        NewEvent::typed(
            HostId::from("a"),
            ProjectId::engine(),
            None,
            quark_hosts::registry::HOST,
            c,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn folds_hosts_samples_and_pools() {
        let mut events = vec![
            change(&HostChange::Registered {
                host: host("a", Health::Healthy),
            }),
            change(&HostChange::Health {
                host: "a".into(),
                health: Health::Degraded {
                    reason: "disk low".into(),
                },
            }),
        ];
        for m in 0..300 {
            let mut u = vec![usage("p1", None, 100), usage("p1", Some("t1"), 300)];
            if m % 2 == 0 {
                u.push(usage("p2", Some("t9"), 50));
            }
            events.push(quark_hosts::recorder::sample_event(&sample("a", m, u)).unwrap());
        }
        let report = PoolReport {
            slots: vec![
                PoolSlot {
                    path: "/w/1".into(),
                    repo: "/r".into(),
                    state: SlotState::Idle,
                    holder: None,
                    project: Some("p1".into()),
                },
                PoolSlot {
                    path: "/w/2".into(),
                    repo: "/r".into(),
                    state: SlotState::InUse,
                    holder: Some(Holder::Task { task: "t1".into() }),
                    project: Some("p1".into()),
                },
            ],
            error: None,
        };
        events.push(
            NewEvent::typed(
                HostId::from("a"),
                ProjectId::engine(),
                None,
                WORKTREES,
                &report,
            )
            .unwrap(),
        );
        let m = model(events).await;
        let mut names = Names::default();
        names.projects.insert("p1".into(), "One".into());
        names.tasks.insert(
            ("p1".into(), "t1".into()),
            ("task_1".into(), "Fix it".into()),
        );

        let v = m.view(48, at(300), &names);
        assert_eq!(v.hosts.len(), 1);
        let h = &v.hosts[0];
        assert_eq!(h.name, "a machine");
        assert_eq!(h.health.status, HostHealthStatus::Degraded);
        assert_eq!(h.health.reason.as_deref(), Some("disk low"));
        assert_eq!(h.capacity.cpus, 8);
        assert!(h.series.len() <= SERIES_POINTS && !h.series.is_empty());
        assert_eq!(h.latest.as_ref().unwrap().at, rfc3339(at(299)));
        // The newest sample (minute 299) has no p2.
        assert_eq!(h.projects.len(), 1);
        let p = &h.projects[0];
        assert_eq!(p.project_name.as_deref(), Some("One"));
        assert_eq!(p.memory_bytes, 400);
        assert_eq!(p.parts[0].engine_task, None, "coordinator first");
        assert_eq!(p.parts[1].task_id.as_deref(), Some("task_1"));
        assert_eq!(p.parts[1].title.as_deref(), Some("Fix it"));
        let w = h.worktrees.as_ref().unwrap();
        assert_eq!((w.idle, w.in_use), (1, 1));

        // An hour's window covers the last 60 samples.
        let v = m.view(1, at(300), &names);
        assert_eq!(v.hosts[0].series.len(), 60);

        let s = m.project("p2", 1, at(300), &names);
        assert_eq!(s.hosts.len(), 1);
        assert!(s.hosts[0].now.is_none());
        assert_eq!(s.hosts[0].series.len(), 60);
        assert!(s.hosts[0].series.iter().any(|x| x.memory_bytes == 50));
        assert!(s.hosts[0].series.iter().any(|x| x.memory_bytes == 0));

        let s = m.project("p1", 1, at(300), &names);
        assert_eq!(s.hosts[0].worktrees.len(), 2);
        assert_eq!(s.hosts[0].now.as_ref().unwrap().memory_bytes, 400);

        assert!(m.project("nobody", 48, at(300), &names).hosts.is_empty());
    }

    #[tokio::test]
    async fn keeps_only_the_retained_window() {
        let events = (0..(RETAIN.whole_minutes() + 30))
            .map(|m| quark_hosts::recorder::sample_event(&sample("b", m, Vec::new())).unwrap())
            .collect();
        let m = model(events).await;
        let f = &m.hosts["b"];
        assert_eq!(f.samples.len() as i64, RETAIN.whole_minutes() + 1);
        let v = m.view(48, at(RETAIN.whole_minutes() + 30), &Names::default());
        // Never registered: named by its id, reachable since it reports.
        assert_eq!(v.hosts[0].name, "b");
        assert_eq!(v.hosts[0].health.status, HostHealthStatus::Healthy);
        assert_eq!(v.hosts[0].series.len(), SERIES_POINTS);
    }

    #[test]
    fn thinning_means_each_group() {
        let samples: Vec<HostSample> = (0..240)
            .map(|m| {
                let mut s = sample("c", m, Vec::new());
                s.cpu = if m % 2 == 0 { 0.0 } else { 1.0 };
                s
            })
            .collect();
        let refs: Vec<&HostSample> = samples.iter().collect();
        let pts = thin(&refs, host_point);
        assert_eq!(pts.len(), 120);
        assert!(pts.iter().all(|p| (p.cpu - 0.5).abs() < 1e-9));
        assert_eq!(pts[119].at, rfc3339(at(239)));
    }
}
