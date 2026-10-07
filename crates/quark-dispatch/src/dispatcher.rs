//! [`Dispatcher`]: from a task's request to a worker on a host.
//!
//! 1. [`Dispatcher::submit`] records the request, resolves the agent from
//!    the Project's rules (classifier, quota, selection), and picks an
//!    account from the profile's pool. A clear resolution is `chosen`;
//!    anything else is `escalated` until the coordinator calls
//!    [`Dispatcher::choose`].
//! 2. [`Dispatcher::tick`] places chosen tasks, oldest first: the merge
//!    guard must allow dispatch (red main), then admission control picks a
//!    host or holds the task. A placed task goes to that host's
//!    [`Spawner`]. The same pass prunes idle worktrees on hosts short of
//!    disk and raises a decision for a host unhealthy too long.
//!
//! Every step is a `dispatch.*` event before it is acted on, and the
//! dispatcher's state is folded from the log, so a new dispatcher on the
//! same log resumes where a crashed one stopped: a request never resolved
//! is resolved, and a placement never confirmed is checked against the
//! host and spawned if the host does not have it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use quark_core::event::kinds;
use quark_core::host::{Health, HostRegistry};
use quark_core::task::TaskEvent;
use quark_core::telemetry::HostSample;
use quark_core::{
    CoreError, Event, EventLog, HostId, MergeGuard, NewEvent, ProjectId, Result, Seq, TaskId,
};
use quark_supervisor::{Assignment, SpawnRequest, Supervisor};
use time::OffsetDateTime;
use tokio::sync::Mutex;

use crate::admission::{self, Admission, HostView, Limits};
use crate::classify::Classifier;
use crate::events::{Choice, Decider, DispatchEvent, Request, PREFIX};
use crate::pools::{self, Accounts};
use crate::quota::{ProviderFamilies, QuotaSource};
use crate::resolve::{self, Resolution, Status};
use crate::rules::{ClassifierSettings, DispatchConfig, Profile};

/// Hands a placed task to the supervisor on its host.
#[async_trait]
pub trait Spawner: Send + Sync {
    async fn spawn(&self, host: &HostId, request: SpawnRequest) -> Result<()>;

    /// Whether the host's supervisor already has the task (a spawn a crash
    /// interrupted may have reached it).
    async fn has(&self, host: &HostId, project: &ProjectId, task: &TaskId) -> Result<bool>;
}

/// The supervisor of this host. Placements on other hosts are refused
/// until their runtimes land.
pub struct LocalSpawner {
    pub host: HostId,
    pub supervisor: Arc<Supervisor>,
}

#[async_trait]
impl Spawner for LocalSpawner {
    async fn spawn(&self, host: &HostId, request: SpawnRequest) -> Result<()> {
        if host != &self.host {
            return Err(CoreError::Unsupported(format!("spawning on host {host}")));
        }
        self.supervisor.spawn(request).await.map(drop)
    }

    async fn has(&self, host: &HostId, project: &ProjectId, task: &TaskId) -> Result<bool> {
        if host != &self.host {
            return Err(CoreError::Unsupported(format!("host {host}")));
        }
        Ok(self.supervisor.task(project, task).is_some())
    }
}

/// Frees disk on a host by pruning its idle pool worktrees.
#[async_trait]
pub trait IdlePool: Send + Sync {
    /// What was pruned, for a person.
    async fn prune_idle(&self, host: &HostId) -> Result<String>;
}

/// A Project's dispatch rules.
#[async_trait]
pub trait Configs: Send + Sync {
    async fn config(&self, project: &ProjectId) -> std::result::Result<DispatchConfig, String>;
}

/// The same rules for every Project.
pub struct FixedConfig(pub DispatchConfig);

#[async_trait]
impl Configs for FixedConfig {
    async fn config(&self, _: &ProjectId) -> std::result::Result<DispatchConfig, String> {
        Ok(self.0.clone())
    }
}

/// What the dispatcher is built from.
pub struct Parts {
    pub log: Arc<dyn EventLog>,
    /// This host, which stamps the events.
    pub host: HostId,
    pub configs: Arc<dyn Configs>,
    pub classifier: Arc<dyn Classifier>,
    pub quota: Arc<dyn QuotaSource>,
    pub families: ProviderFamilies,
    pub accounts: Arc<dyn Accounts>,
    pub hosts: Arc<dyn HostRegistry>,
    pub spawner: Arc<dyn Spawner>,
    /// The red-main guard; `None` allows every dispatch.
    pub guard: Option<Arc<dyn MergeGuard>>,
    pub pool: Option<Arc<dyn IdlePool>>,
}

/// Tunables.
#[derive(Debug, Clone)]
pub struct Settings {
    pub limits: Limits,
    /// How long a host may stay unhealthy before a person is asked.
    pub unhealthy_after: Duration,
    /// Whether a classifier API key is in the environment, for rules files
    /// without a `classifier` block.
    pub key_present: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            unhealthy_after: Duration::from_secs(600),
            key_present: false,
        }
    }
}

/// Where a request is.
#[derive(Debug, Clone, PartialEq)]
pub enum Stage {
    /// Recorded, not yet resolved.
    Requested,
    Escalated {
        reason: String,
    },
    Chosen {
        choice: Choice,
    },
    Placed {
        choice: Choice,
        host: HostId,
    },
}

/// One request still in the dispatcher's hands.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub project: ProjectId,
    pub task: TaskId,
    pub request: Request,
    pub stage: Stage,
    pub resolution: Option<Resolution>,
    /// Why it waits for a host, while it does.
    pub held: Option<String>,
    seq: Seq,
}

/// What a [`Dispatcher::tick`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Tick {
    pub resolved: usize,
    pub placed: usize,
    pub held: usize,
    pub spawned: usize,
    pub failed: usize,
    pub pruned: usize,
    pub alerts: usize,
}

type Key = (ProjectId, TaskId);

#[derive(Default)]
struct State {
    cursor: Seq,
    pending: BTreeMap<Key, Pending>,
    /// Every task ever requested, so a second request is refused.
    known: HashSet<Key>,
    /// Running tasks and the host they run on, from task transitions.
    active: HashMap<Key, HostId>,
    samples: HashMap<HostId, HostSample>,
    /// Hosts with an open unhealthy decision.
    reported: HashSet<HostId>,
    /// Hosts pruned in the current low-disk episode.
    pruned: HashSet<HostId>,
    /// When each unhealthy host was first seen unhealthy (not durable: the
    /// clock restarts after a crash).
    unhealthy_since: HashMap<HostId, OffsetDateTime>,
}

impl State {
    fn apply(&mut self, e: &Event) {
        self.cursor = e.seq;
        let kind = e.kind.as_str();
        if kind == quark_hosts::recorder::SAMPLE {
            if let Ok(s) = e.decode::<HostSample>() {
                let newer = self.samples.get(&s.host).is_none_or(|old| old.ts <= s.ts);
                if newer {
                    self.samples.insert(s.host.clone(), s);
                }
            }
            return;
        }
        let Some(task) = e.task.clone() else {
            if e.kind.prefix() == PREFIX {
                self.apply_engine(e);
            }
            return;
        };
        let key = (e.project.clone(), task);
        if kind == kinds::TASK {
            match e.decode::<TaskEvent>() {
                Ok(TaskEvent::Started { .. }) => {
                    self.active.insert(key, e.host.clone());
                }
                Ok(TaskEvent::Completed | TaskEvent::Failed { .. } | TaskEvent::Cancelled) => {
                    self.active.remove(&key);
                }
                _ => {}
            }
            return;
        }
        if e.kind.prefix() != PREFIX {
            return;
        }
        let Ok(ev) = e.decode::<DispatchEvent>() else {
            return;
        };
        if let DispatchEvent::Requested { request } = ev {
            if self.known.insert(key.clone()) {
                self.pending.insert(
                    key.clone(),
                    Pending {
                        project: key.0,
                        task: key.1,
                        request,
                        stage: Stage::Requested,
                        resolution: None,
                        held: None,
                        seq: e.seq,
                    },
                );
            }
            return;
        }
        let Some(p) = self.pending.get_mut(&key) else {
            return;
        };
        match ev {
            DispatchEvent::Resolved { resolution } => p.resolution = Some(resolution),
            DispatchEvent::Escalated { reason } => p.stage = Stage::Escalated { reason },
            DispatchEvent::Chosen { choice } => {
                p.stage = Stage::Chosen { choice };
                p.held = None;
            }
            DispatchEvent::Held { reason, .. } => p.held = Some(reason),
            DispatchEvent::Placed { host, .. } => {
                if let Stage::Chosen { choice } = &p.stage {
                    p.stage = Stage::Placed {
                        choice: choice.clone(),
                        host,
                    };
                    p.held = None;
                }
            }
            DispatchEvent::Spawned
            | DispatchEvent::SpawnFailed { .. }
            | DispatchEvent::Withdrawn => {
                self.pending.remove(&key);
            }
            _ => {}
        }
    }

    fn apply_engine(&mut self, e: &Event) {
        match e.decode::<DispatchEvent>() {
            Ok(DispatchEvent::HostUnhealthy { host, .. }) => {
                self.reported.insert(host);
            }
            Ok(DispatchEvent::HostRecovered { host }) => {
                self.reported.remove(&host);
            }
            Ok(DispatchEvent::Pruned { host, .. }) => {
                self.pruned.insert(host);
            }
            _ => {}
        }
    }

    /// Workers per host and Project: running tasks plus placements the
    /// host has not started yet.
    fn running(&self) -> HashMap<HostId, BTreeMap<ProjectId, u32>> {
        let mut out: HashMap<HostId, BTreeMap<ProjectId, u32>> = HashMap::new();
        for ((project, _), host) in &self.active {
            *out.entry(host.clone())
                .or_default()
                .entry(project.clone())
                .or_default() += 1;
        }
        for (key, p) in &self.pending {
            if let Stage::Placed { host, .. } = &p.stage {
                if !self.active.contains_key(key) {
                    *out.entry(host.clone())
                        .or_default()
                        .entry(p.project.clone())
                        .or_default() += 1;
                }
            }
        }
        out
    }
}

/// Dispatches tasks to workers on hosts. See the module docs.
pub struct Dispatcher {
    parts: Parts,
    settings: Settings,
    state: Mutex<State>,
}

impl Dispatcher {
    /// A dispatcher over `parts.log`, caught up with it.
    pub async fn open(parts: Parts, settings: Settings) -> Result<Self> {
        let d = Self {
            parts,
            settings,
            state: Mutex::new(State::default()),
        };
        d.refresh(&mut *d.state.lock().await).await?;
        Ok(d)
    }

    async fn refresh(&self, s: &mut State) -> Result<()> {
        loop {
            let batch = self.parts.log.read(s.cursor, 1000).await?;
            if batch.is_empty() {
                return Ok(());
            }
            for e in &batch {
                s.apply(e);
            }
        }
    }

    async fn record(
        &self,
        s: &mut State,
        project: &ProjectId,
        task: Option<&TaskId>,
        event: DispatchEvent,
    ) -> Result<()> {
        let e = NewEvent::typed(
            self.parts.host.clone(),
            project.clone(),
            task.cloned(),
            event.kind(),
            &event,
        )?;
        self.parts.log.append(e).await?;
        self.refresh(s).await
    }

    /// Requests not yet handed to a host, oldest first.
    pub async fn pending(&self) -> Result<Vec<Pending>> {
        let mut s = self.state.lock().await;
        self.refresh(&mut s).await?;
        let mut out: Vec<Pending> = s.pending.values().cloned().collect();
        out.sort_by_key(|p| p.seq);
        Ok(out)
    }

    /// Record a task's request and resolve its agent. Returns where it
    /// stands: chosen (waiting for a host) or escalated to the coordinator.
    pub async fn submit(
        &self,
        project: ProjectId,
        task: TaskId,
        request: Request,
    ) -> Result<Stage> {
        let mut s = self.state.lock().await;
        self.refresh(&mut s).await?;
        let key = (project.clone(), task.clone());
        if s.known.contains(&key) {
            return Err(CoreError::Invalid(format!(
                "task {task} was already dispatched"
            )));
        }
        self.record(
            &mut s,
            &project,
            Some(&task),
            DispatchEvent::Requested { request },
        )
        .await?;
        self.resolve_one(&mut s, &key).await
    }

    /// The coordinator's pick for an escalated (or waiting) task.
    pub async fn choose(
        &self,
        project: &ProjectId,
        task: &TaskId,
        profile: Profile,
    ) -> Result<Stage> {
        let mut s = self.state.lock().await;
        self.refresh(&mut s).await?;
        let key = (project.clone(), task.clone());
        let Some(p) = s.pending.get(&key) else {
            return Err(CoreError::NotFound(format!(
                "pending dispatch of task {task}"
            )));
        };
        if matches!(p.stage, Stage::Placed { .. }) {
            return Err(CoreError::Invalid(format!("task {task} is already placed")));
        }
        self.choose_account(&mut s, &key, profile, Decider::Coordinator)
            .await
    }

    /// Take back a task not yet placed on a host.
    pub async fn withdraw(&self, project: &ProjectId, task: &TaskId) -> Result<()> {
        let mut s = self.state.lock().await;
        self.refresh(&mut s).await?;
        match s
            .pending
            .get(&(project.clone(), task.clone()))
            .map(|p| &p.stage)
        {
            None => Err(CoreError::NotFound(format!(
                "pending dispatch of task {task}"
            ))),
            Some(Stage::Placed { .. }) => Err(CoreError::Invalid(format!(
                "task {task} is already placed; cancel it instead"
            ))),
            Some(_) => {
                self.record(&mut s, project, Some(task), DispatchEvent::Withdrawn)
                    .await
            }
        }
    }

    /// Resolve the agent for a requested task.
    async fn resolve_one(&self, s: &mut State, key: &Key) -> Result<Stage> {
        let p =
            s.pending.get(key).cloned().ok_or_else(|| {
                CoreError::NotFound(format!("pending dispatch of task {}", key.1))
            })?;
        if let Some(profile) = p.request.profile.clone() {
            return self
                .choose_account(s, key, profile, Decider::Coordinator)
                .await;
        }
        let resolution = self.resolution(&p.project, &p.request.brief).await;
        let (project, task) = key;
        self.record(
            s,
            project,
            Some(task),
            DispatchEvent::Resolved {
                resolution: resolution.clone(),
            },
        )
        .await?;
        match (resolution.status, resolution.profile()) {
            (Status::Clear, Some(profile)) => {
                self.choose_account(s, key, profile.clone(), Decider::Resolution)
                    .await
            }
            _ => {
                let reason = match resolution.status {
                    Status::Off => "no classifier is configured; the coordinator picks".into(),
                    st => format!(
                        "dispatch resolution is {}: {}",
                        st.as_str(),
                        resolution
                            .reason
                            .as_deref()
                            .unwrap_or("no profile selected")
                    ),
                };
                self.escalate(s, key, reason).await
            }
        }
    }

    /// The resolution of `brief` under `project`'s rules, never failing:
    /// an unreadable rules file or quota read is an `error` resolution.
    pub async fn resolution(&self, project: &ProjectId, brief: &str) -> Resolution {
        let config = match self.parts.configs.config(project).await {
            Ok(c) => c,
            Err(e) => return Resolution::error(format!("dispatch rules: {e}")),
        };
        let settings = ClassifierSettings::from_config(&config, self.settings.key_present);
        if !settings.on {
            return Resolution::off();
        }
        if config.rules.is_empty() {
            return Resolution::no_rules();
        }
        let classification = self
            .parts
            .classifier
            .classify(Some(project.as_str()), brief, &config, &settings)
            .await;
        let providers = resolve::providers(&config, &self.parts.families);
        let quota = self.parts.quota.snapshot(&providers).await;
        resolve::resolve(
            &config,
            &settings,
            Some(&classification),
            quota.as_ref().map_err(String::as_str),
            &self.parts.families,
        )
    }

    async fn escalate(&self, s: &mut State, key: &Key, reason: String) -> Result<Stage> {
        let (project, task) = key;
        self.record(
            s,
            project,
            Some(task),
            DispatchEvent::Escalated {
                reason: reason.clone(),
            },
        )
        .await?;
        Ok(Stage::Escalated { reason })
    }

    async fn choose_account(
        &self,
        s: &mut State,
        key: &Key,
        profile: Profile,
        by: Decider,
    ) -> Result<Stage> {
        let accounts = match self.parts.accounts.accounts(&profile.harness).await {
            Ok(a) => a,
            Err(e) => {
                return self
                    .escalate(s, key, format!("accounts for {}: {e}", profile.harness))
                    .await
            }
        };
        let chosen = match pools::choose(&accounts, profile.pool.as_deref(), None) {
            Ok(a) => a.cloned(),
            Err(e) => return self.escalate(s, key, e.to_string()).await,
        };
        let choice = Choice {
            account: chosen.as_ref().map(|a| a.id.clone()),
            env: chosen.map(|a| a.env).unwrap_or_default(),
            profile,
            by,
        };
        let (project, task) = key;
        self.record(
            s,
            project,
            Some(task),
            DispatchEvent::Chosen {
                choice: choice.clone(),
            },
        )
        .await?;
        Ok(Stage::Chosen { choice })
    }

    /// One pass: finish interrupted work, watch host health, prune hosts
    /// short of disk, and place chosen tasks.
    pub async fn tick(&self) -> Result<Tick> {
        let mut s = self.state.lock().await;
        self.refresh(&mut s).await?;
        let mut out = Tick::default();
        let now = OffsetDateTime::now_utc();

        // Requests a crash left unresolved.
        let requested: Vec<Key> = s
            .pending
            .iter()
            .filter(|(_, p)| p.stage == Stage::Requested)
            .map(|(k, _)| k.clone())
            .collect();
        for key in requested {
            self.resolve_one(&mut s, &key).await?;
            out.resolved += 1;
        }

        let hosts = self.parts.hosts.hosts().await?;
        out.alerts += self.watch_health(&mut s, &hosts, now).await?;

        let running = s.running();
        let mut views: Vec<HostView> = hosts
            .into_iter()
            .map(|h| HostView {
                sample: s.samples.get(&h.id).cloned(),
                running: running.get(&h.id).cloned().unwrap_or_default(),
                host: h,
            })
            .collect();

        out.pruned += self.prune(&mut s, &views, now).await?;

        // Placements a crash left unconfirmed.
        let placed: Vec<(Key, HostId)> = s
            .pending
            .iter()
            .filter_map(|(k, p)| match &p.stage {
                Stage::Placed { host, .. } => Some((k.clone(), host.clone())),
                _ => None,
            })
            .collect();
        for (key, host) in placed {
            if self.parts.spawner.has(&host, &key.0, &key.1).await? {
                self.record(&mut s, &key.0, Some(&key.1), DispatchEvent::Spawned)
                    .await?;
                out.spawned += 1;
            } else if self.spawn(&mut s, &key, &host).await? {
                out.spawned += 1;
            } else {
                out.failed += 1;
            }
        }

        let mut chosen: Vec<(Seq, Key, Option<String>)> = s
            .pending
            .iter()
            .filter(|(_, p)| matches!(p.stage, Stage::Chosen { .. }))
            .map(|(k, p)| (p.seq, k.clone(), p.held.clone()))
            .collect();
        chosen.sort();
        for (_, key, held) in chosen {
            let (project, task) = &key;
            if let Some(guard) = &self.parts.guard {
                if let quark_core::Permit::Deny { reason } =
                    guard.may_dispatch(project, task).await?
                {
                    if held.as_deref() != Some(reason.as_str()) {
                        self.record(
                            &mut s,
                            project,
                            Some(task),
                            DispatchEvent::Held {
                                reason,
                                refusals: Vec::new(),
                            },
                        )
                        .await?;
                    }
                    out.held += 1;
                    continue;
                }
            }
            match admission::admit(project, &views, &self.settings.limits, now) {
                Admission::Hold { reason, refusals } => {
                    if held.as_deref() != Some(reason.as_str()) {
                        self.record(
                            &mut s,
                            project,
                            Some(task),
                            DispatchEvent::Held { reason, refusals },
                        )
                        .await?;
                    }
                    out.held += 1;
                }
                Admission::Place { host, notes } => {
                    self.record(
                        &mut s,
                        project,
                        Some(task),
                        DispatchEvent::Placed {
                            host: host.clone(),
                            notes,
                        },
                    )
                    .await?;
                    out.placed += 1;
                    if let Some(v) = views.iter_mut().find(|v| v.host.id == host) {
                        *v.running.entry(project.clone()).or_default() += 1;
                    }
                    if self.spawn(&mut s, &key, &host).await? {
                        out.spawned += 1;
                    } else {
                        out.failed += 1;
                    }
                }
            }
        }
        Ok(out)
    }

    /// Hand a placed task to its host. `Ok(false)` when the host refused.
    async fn spawn(&self, s: &mut State, key: &Key, host: &HostId) -> Result<bool> {
        let Some(p) = s.pending.get(key).cloned() else {
            return Ok(false);
        };
        let Stage::Placed { choice, .. } = &p.stage else {
            return Ok(false);
        };
        let r = &p.request;
        let mut env = r.env.clone();
        env.extend(choice.env.clone());
        let request = SpawnRequest {
            project: p.project.clone(),
            task: p.task.clone(),
            assignment: Assignment {
                title: r.title.clone(),
                brief: r.brief.clone(),
                repo: r.repo.clone(),
                branch: r.branch.clone(),
                base: r.base.clone(),
                harness: choice.profile.harness.clone(),
                model: choice.profile.model.clone(),
                effort: choice.profile.effort.clone(),
                isolation: r.isolation,
                policy: r.policy.clone(),
                env,
            },
        };
        let (project, task) = key;
        match self.parts.spawner.spawn(host, request).await {
            Ok(()) => {
                self.record(s, project, Some(task), DispatchEvent::Spawned)
                    .await?;
                Ok(true)
            }
            Err(e) => {
                tracing::warn!(%task, %host, error = %e, "dispatch spawn failed");
                self.record(
                    s,
                    project,
                    Some(task),
                    DispatchEvent::SpawnFailed {
                        reason: e.to_string(),
                    },
                )
                .await?;
                Ok(false)
            }
        }
    }

    /// Raise one decision per host that stays unhealthy past
    /// `unhealthy_after`, and close it when the host recovers.
    async fn watch_health(
        &self,
        s: &mut State,
        hosts: &[quark_core::Host],
        now: OffsetDateTime,
    ) -> Result<usize> {
        let mut alerts = 0;
        for h in hosts {
            if h.health == Health::Healthy {
                s.unhealthy_since.remove(&h.id);
                if s.reported.contains(&h.id) {
                    self.record(
                        s,
                        &ProjectId::engine(),
                        None,
                        DispatchEvent::HostRecovered { host: h.id.clone() },
                    )
                    .await?;
                }
                continue;
            }
            let since = *s.unhealthy_since.entry(h.id.clone()).or_insert(now);
            let secs = (now - since).whole_seconds().max(0) as u64;
            if secs >= self.settings.unhealthy_after.as_secs() && !s.reported.contains(&h.id) {
                self.record(
                    s,
                    &ProjectId::engine(),
                    None,
                    DispatchEvent::HostUnhealthy {
                        host: h.id.clone(),
                        health: h.health.clone(),
                        unhealthy_secs: secs,
                    },
                )
                .await?;
                alerts += 1;
            }
        }
        Ok(alerts)
    }

    /// Prune idle worktrees once per low-disk episode on each host.
    async fn prune(&self, s: &mut State, views: &[HostView], now: OffsetDateTime) -> Result<usize> {
        let low: HashSet<HostId> = admission::low_on_disk(views, &self.settings.limits, now)
            .into_iter()
            .collect();
        // An episode ends when the host has room again.
        s.pruned.retain(|h| low.contains(h));
        let Some(pool) = &self.parts.pool else {
            return Ok(0);
        };
        let mut n = 0;
        for host in low {
            if s.pruned.contains(&host) {
                continue;
            }
            let outcome = match pool.prune_idle(&host).await {
                Ok(o) => o,
                Err(e) => format!("prune failed: {e}"),
            };
            self.record(
                s,
                &ProjectId::engine(),
                None,
                DispatchEvent::Pruned {
                    host: host.clone(),
                    outcome,
                },
            )
            .await?;
            s.pruned.insert(host);
            n += 1;
        }
        Ok(n)
    }

    /// Ticks every `every` until the task is dropped.
    pub async fn run(self: Arc<Self>, every: Duration) {
        let mut ticks = tokio::time::interval(every);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            if let Err(e) = self.tick().await {
                tracing::warn!(error = %e, "dispatch pass failed");
            }
        }
    }
}
