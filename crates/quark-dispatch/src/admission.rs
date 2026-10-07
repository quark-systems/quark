//! Admission control: place a new worker on a host with room for it, or
//! hold it until one has.
//!
//! A host takes a new worker only when it is healthy, allowed to run the
//! Project, under its worker cap, and its latest sample shows CPU, memory
//! pressure and free disk inside the limits. A sample older than
//! `max_sample_age` is not trusted: the resource checks are skipped and
//! the placement says so, since a missing sampler should not stop work
//! the worker cap allows. Among hosts that pass, the least loaded wins,
//! preferring a host with a fresh sample, then the one with the least CPU
//! in use.
//! Per-Project limits cap a Project's workers in total and per host.
//!
//! Hosts short of disk are reported so the dispatcher can prune idle pool
//! worktrees there.

use std::collections::BTreeMap;
use std::time::Duration;

use quark_core::host::{Health, Host};
use quark_core::telemetry::HostSample;
use quark_core::{HostId, ProjectId};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Per-host thresholds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostLimits {
    /// Highest whole-host CPU share (0 to 1) a host may be at.
    pub max_cpu: f64,
    /// Highest memory pressure (0 to 1).
    pub max_memory_pressure: f64,
    /// Least free disk.
    pub min_disk_free_bytes: u64,
    /// How old a sample may be and still count.
    #[serde(with = "secs")]
    pub max_sample_age: Duration,
}

impl Default for HostLimits {
    fn default() -> Self {
        Self {
            max_cpu: 0.9,
            max_memory_pressure: 0.75,
            min_disk_free_bytes: 5 << 30,
            max_sample_age: Duration::from_secs(120),
        }
    }
}

/// Per-Project caps; `None` is no cap.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectLimits {
    pub max_workers: Option<u32>,
    pub max_per_host: Option<u32>,
}

/// Every threshold admission applies.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Limits {
    pub host: HostLimits,
    /// Overrides per host.
    #[serde(default)]
    pub hosts: BTreeMap<HostId, HostLimits>,
    /// For Projects not listed in `projects`.
    #[serde(default)]
    pub project: ProjectLimits,
    #[serde(default)]
    pub projects: BTreeMap<ProjectId, ProjectLimits>,
}

impl Limits {
    pub fn for_host(&self, host: &HostId) -> &HostLimits {
        self.hosts.get(host).unwrap_or(&self.host)
    }

    pub fn for_project(&self, project: &ProjectId) -> ProjectLimits {
        self.projects.get(project).copied().unwrap_or(self.project)
    }
}

/// One host as admission sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct HostView {
    pub host: Host,
    /// The latest sample, if any.
    pub sample: Option<HostSample>,
    /// Workers running there, per Project.
    pub running: BTreeMap<ProjectId, u32>,
}

impl HostView {
    pub fn total(&self) -> u32 {
        self.running.values().sum()
    }
}

/// Why one host cannot take the worker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Refusal {
    pub host: HostId,
    pub reason: String,
}

/// Admission's answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Admission {
    Place {
        host: HostId,
        /// Caveats, such as a sample too old to judge resources by.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        notes: Vec<String>,
    },
    Hold {
        /// One line for a person.
        reason: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        refusals: Vec<Refusal>,
    },
}

impl Admission {
    pub fn host(&self) -> Option<&HostId> {
        match self {
            Admission::Place { host, .. } => Some(host),
            Admission::Hold { .. } => None,
        }
    }
}

/// Where a new worker for `project` goes, given every host.
pub fn admit(
    project: &ProjectId,
    hosts: &[HostView],
    limits: &Limits,
    now: OffsetDateTime,
) -> Admission {
    let pl = limits.for_project(project);
    let total: u32 = hosts
        .iter()
        .map(|h| h.running.get(project).copied().unwrap_or(0))
        .sum();
    if let Some(max) = pl.max_workers {
        if total >= max {
            return Admission::Hold {
                reason: format!("Project {project} already runs {total} of its {max} workers"),
                refusals: Vec::new(),
            };
        }
    }
    if hosts.is_empty() {
        return Admission::Hold {
            reason: "no hosts are registered".into(),
            refusals: Vec::new(),
        };
    }

    let mut refusals = Vec::new();
    // (load, cpu, index, notes)
    let mut fits: Vec<(f64, f64, usize, Vec<String>)> = Vec::new();
    for (i, v) in hosts.iter().enumerate() {
        match check(project, v, limits, pl, now) {
            Ok((cpu, notes)) => {
                let running = v.total() as f64;
                let load = match v.host.capacity.max_workers {
                    0 => running,
                    cap => running / cap as f64,
                };
                fits.push((load, cpu, i, notes));
            }
            Err(reason) => refusals.push(Refusal {
                host: v.host.id.clone(),
                reason,
            }),
        }
    }
    let best = fits.into_iter().min_by(|a, b| {
        // Least loaded, then a host with a fresh sample over one without
        // (whose notes say so), then the least busy CPU.
        a.0.total_cmp(&b.0)
            .then(a.3.len().cmp(&b.3.len()))
            .then(a.1.total_cmp(&b.1))
            .then_with(|| hosts[a.2].host.id.cmp(&hosts[b.2].host.id))
    });
    match best {
        Some((_, _, i, notes)) => Admission::Place {
            host: hosts[i].host.id.clone(),
            notes,
        },
        None => {
            let reason = match refusals.as_slice() {
                [one] => format!("host {} {}", one.host, one.reason),
                many => format!("none of {} hosts has room", many.len()),
            };
            Admission::Hold { reason, refusals }
        }
    }
}

/// `Ok((cpu, notes))` when the host can take the worker.
fn check(
    project: &ProjectId,
    v: &HostView,
    limits: &Limits,
    pl: ProjectLimits,
    now: OffsetDateTime,
) -> Result<(f64, Vec<String>), String> {
    let h = &v.host;
    match &h.health {
        Health::Healthy => {}
        Health::Degraded { reason } => return Err(format!("is degraded: {reason}")),
        Health::Unreachable { reason } => return Err(format!("is unreachable: {reason}")),
    }
    if !h.projects.is_empty() && !h.projects.contains(project) {
        return Err(format!("does not run Project {project}"));
    }
    let running = v.total();
    if h.capacity.max_workers > 0 && running >= h.capacity.max_workers {
        return Err(format!(
            "is full ({running} of {} workers)",
            h.capacity.max_workers
        ));
    }
    if let Some(max) = pl.max_per_host {
        let here = v.running.get(project).copied().unwrap_or(0);
        if here >= max {
            return Err(format!(
                "already runs {here} of Project {project}'s {max} workers per host"
            ));
        }
    }
    let hl = limits.for_host(&h.id);
    let fresh = v.sample.as_ref().filter(|s| {
        let age = now - s.ts;
        age <= time::Duration::try_from(hl.max_sample_age).unwrap_or(time::Duration::MAX)
    });
    let Some(s) = fresh else {
        let note = match &v.sample {
            None => format!(
                "host {} has no telemetry sample yet; placed by worker count only",
                h.id
            ),
            Some(_) => format!(
                "host {}'s latest sample is stale; placed by worker count only",
                h.id
            ),
        };
        return Ok((0.0, vec![note]));
    };
    if s.disk_free_bytes < hl.min_disk_free_bytes {
        return Err(format!(
            "is low on disk ({} free, needs {})",
            gib(s.disk_free_bytes),
            gib(hl.min_disk_free_bytes)
        ));
    }
    if s.memory_pressure > hl.max_memory_pressure {
        return Err(format!(
            "is under memory pressure ({:.0}%, limit {:.0}%)",
            s.memory_pressure * 100.0,
            hl.max_memory_pressure * 100.0
        ));
    }
    if s.cpu > hl.max_cpu {
        return Err(format!(
            "is busy ({:.0}% CPU, limit {:.0}%)",
            s.cpu * 100.0,
            hl.max_cpu * 100.0
        ));
    }
    Ok((s.cpu, Vec::new()))
}

/// Hosts whose latest fresh sample is under their free-disk threshold.
pub fn low_on_disk(hosts: &[HostView], limits: &Limits, now: OffsetDateTime) -> Vec<HostId> {
    hosts
        .iter()
        .filter(|v| {
            let hl = limits.for_host(&v.host.id);
            v.sample.as_ref().is_some_and(|s| {
                let age = now - s.ts;
                age <= time::Duration::try_from(hl.max_sample_age).unwrap_or(time::Duration::MAX)
                    && s.disk_free_bytes < hl.min_disk_free_bytes
            })
        })
        .map(|v| v.host.id.clone())
        .collect()
}

fn gib(b: u64) -> String {
    format!("{:.1} GiB", b as f64 / (1u64 << 30) as f64)
}

mod secs {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_secs())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        Ok(Duration::from_secs(u64::deserialize(d)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quark_core::host::{Capacity, Platform, RuntimeKind};
    use quark_core::telemetry::QuarkDisk;

    fn host(id: &str, max: u32) -> Host {
        Host {
            id: id.into(),
            name: id.into(),
            runtime: RuntimeKind::Local,
            platform: Platform {
                os: "macos".into(),
                arch: "aarch64".into(),
            },
            capacity: Capacity {
                cpus: 8,
                memory_bytes: 16 << 30,
                disk_bytes: 500 << 30,
                max_workers: max,
            },
            health: Health::Healthy,
            projects: Vec::new(),
            tasks: Vec::new(),
        }
    }

    fn sample(id: &str, now: OffsetDateTime, cpu: f64, pressure: f64, free_gib: u64) -> HostSample {
        HostSample {
            host: id.into(),
            ts: now,
            cpu,
            memory_used_bytes: 0,
            memory_total_bytes: 16 << 30,
            memory_pressure: pressure,
            disk_free_bytes: free_gib << 30,
            quark_disk: QuarkDisk::default(),
            usage: Vec::new(),
        }
    }

    fn view(h: Host, s: Option<HostSample>, running: &[(&str, u32)]) -> HostView {
        HostView {
            host: h,
            sample: s,
            running: running
                .iter()
                .map(|(p, n)| (ProjectId::from(*p), *n))
                .collect(),
        }
    }

    #[test]
    fn places_on_the_least_loaded_healthy_host() {
        let now = OffsetDateTime::now_utc();
        let hosts = vec![
            view(
                host("a", 4),
                Some(sample("a", now, 0.2, 0.1, 100)),
                &[("p", 2)],
            ),
            view(
                host("b", 4),
                Some(sample("b", now, 0.5, 0.1, 100)),
                &[("p", 1)],
            ),
        ];
        let a = admit(&"p".into(), &hosts, &Limits::default(), now);
        assert_eq!(a.host().unwrap().as_str(), "b");
    }

    #[test]
    fn holds_when_every_host_is_out_of_room() {
        let now = OffsetDateTime::now_utc();
        let mut sick = host("c", 4);
        sick.health = Health::Degraded {
            reason: "ssh slow".into(),
        };
        let hosts = vec![
            view(
                host("a", 2),
                Some(sample("a", now, 0.2, 0.1, 100)),
                &[("p", 1), ("q", 1)],
            ),
            view(host("b", 4), Some(sample("b", now, 0.95, 0.1, 100)), &[]),
            view(sick, None, &[]),
            view(host("d", 4), Some(sample("d", now, 0.1, 0.9, 100)), &[]),
            view(host("e", 4), Some(sample("e", now, 0.1, 0.1, 2)), &[]),
        ];
        let Admission::Hold { reason, refusals } =
            admit(&"p".into(), &hosts, &Limits::default(), now)
        else {
            panic!("placed");
        };
        assert_eq!(reason, "none of 5 hosts has room");
        let r: Vec<&str> = refusals.iter().map(|r| r.reason.as_str()).collect();
        assert_eq!(r[0], "is full (2 of 2 workers)");
        assert!(r[1].starts_with("is busy (95% CPU"));
        assert!(r[2].starts_with("is degraded"));
        assert!(r[3].starts_with("is under memory pressure"));
        assert!(r[4].starts_with("is low on disk (2.0 GiB free"));
        assert_eq!(
            low_on_disk(&hosts, &Limits::default(), now),
            vec![HostId::from("e")]
        );
    }

    #[test]
    fn project_limits() {
        let now = OffsetDateTime::now_utc();
        let hosts = vec![
            view(
                host("a", 0),
                Some(sample("a", now, 0.1, 0.1, 100)),
                &[("p", 2)],
            ),
            view(
                host("b", 0),
                Some(sample("b", now, 0.1, 0.1, 100)),
                &[("p", 1)],
            ),
        ];
        let mut limits = Limits::default();
        limits.projects.insert(
            "p".into(),
            ProjectLimits {
                max_workers: Some(3),
                max_per_host: None,
            },
        );
        assert!(matches!(
            admit(&"p".into(), &hosts, &limits, now),
            Admission::Hold { .. }
        ));
        limits.projects.insert(
            "p".into(),
            ProjectLimits {
                max_workers: None,
                max_per_host: Some(2),
            },
        );
        assert_eq!(
            admit(&"p".into(), &hosts, &limits, now)
                .host()
                .unwrap()
                .as_str(),
            "b"
        );
        // Another Project is unaffected.
        assert!(admit(&"q".into(), &hosts, &limits, now).host().is_some());
    }

    #[test]
    fn stale_or_missing_samples_place_by_count_with_a_note() {
        let now = OffsetDateTime::now_utc();
        let old = now - time::Duration::minutes(10);
        let mut only_q = host("b", 4);
        only_q.projects = vec!["q".into()];
        let hosts = vec![
            view(host("a", 4), Some(sample("a", old, 0.99, 0.99, 1)), &[]),
            view(only_q, None, &[]),
        ];
        let Admission::Place { host, notes } = admit(&"p".into(), &hosts, &Limits::default(), now)
        else {
            panic!("held");
        };
        assert_eq!(host.as_str(), "a");
        assert!(notes[0].contains("stale"));
        assert!(low_on_disk(&hosts, &Limits::default(), now).is_empty());
    }
}
