//! Native dispatch (slice 5) for the native engine: starting a task with no
//! firstmate.
//!
//! [`NativeDispatch`] puts a [`quark_dispatch::Dispatcher`] in front of the
//! native [`Supervisor`]:
//!
//! - [`NativeDispatch::start`] creates the task, records its request, and
//!   resolves its agent from the Project's dispatch rules (classifier,
//!   quota, selection), or takes the agent the caller named. A clear
//!   resolution chooses an account from the profile's pool; anything else
//!   escalates until [`NativeDispatch::choose`] picks one.
//! - A chosen task is placed at once if admission control finds a host with
//!   room, and its worker spawned there by the supervisor in an isolated
//!   worktree; otherwise it waits, and the dispatcher's own timer
//!   ([`NativeDispatch::run`]) places it when a host frees up.
//!
//! Layout, in the Project workspace (`WorkspaceRef::root`):
//!
//! - `repos/<name>` is each source repo's primary checkout, the one task
//!   worktrees are cut from. `projects/<name>`, where firstmate clones
//!   them, is read too, so a workspace firstmate seeded runs natively.
//! - `config/crew-dispatch.json` holds the dispatch rules
//!   ([`NativeDispatch::set_config`]), the same file firstmate reads.
//!
//! Every decision is a `dispatch.*` event, so a restarted daemon resumes
//! dispatch where the last one stopped.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use quark_core::{
    replay, CoreError, Event, EventLog, HostId, HostRegistry, IsolationMode, ProjectId, ReadModel,
    Seq, TaskId,
};
use quark_dispatch::events::PREFIX;
use quark_dispatch::{
    Accounts, Choice, Classifier, Configs, DispatchConfig, DispatchEvent, Dispatcher, LocalSpawner,
    NoAccounts, Parts, Pending, PoolAccount, Profile, ProviderFamilies, QuotaSource, Request,
    Resolution, Settings, Spawner, Stage, Status,
};
use quark_supervisor::{SpawnRequest, Supervisor};
use quark_systems::{
    AgentConfig, DispatchCandidate, DispatchChoice, DispatchRule, DispatchStatus, TaskKind,
    TaskState,
};

use super::{
    EngineError, EngineResolution, EngineTask, NewTask, StartOutcome, TaskStart, WorkspaceRef,
};

/// Where a Project workspace keeps its source repos' primary checkouts.
pub const REPOS_DIR: &str = "repos";
/// Where firstmate clones them, read as a fallback.
pub const FIRSTMATE_REPOS_DIR: &str = "projects";
/// The dispatch rules, relative to the workspace root.
pub const CONFIG_FILE: &str = "config/crew-dispatch.json";
/// How often waiting tasks are offered to admission control again.
pub const TICK: Duration = Duration::from_secs(5);
/// Account variables the native supervisor sets on a worker's launch: every
/// built-in harness's that has one.
pub const ACCOUNT_ENVS: &[&str] = &[
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "GROK_HOME",
    "PI_CODING_AGENT_DIR",
];
/// Longest task id made from a title, before any `-<n>` suffix.
const MAX_SLUG: usize = 40;
/// Most `-<n>` suffixes tried for a free task id.
const MAX_SUFFIX: usize = 100;
const REPLAY_BATCH: usize = 512;

type Key = (ProjectId, TaskId);

/// What [`NativeDispatch`] is built from, besides the supervisor.
pub struct DispatchParts {
    pub log: Arc<dyn EventLog>,
    /// This host: it stamps the events, and its supervisor spawns.
    pub host: HostId,
    pub hosts: Arc<dyn HostRegistry>,
    pub classifier: Arc<dyn Classifier>,
    pub quota: Arc<dyn QuotaSource>,
    pub families: ProviderFamilies,
    pub settings: Settings,
}

impl DispatchParts {
    /// The daemon's: System One, `quota-axi`, the host registry in the
    /// event log, and the harness manifests in `<home>/harnesses`.
    pub fn for_daemon(config: &crate::config::Config, log: Arc<dyn EventLog>) -> Self {
        let host = crate::event_ingest::host();
        let key = std::env::var(quark_dispatch::classify::KEY_ENV).is_ok_and(|k| !k.is_empty());
        Self {
            hosts: Arc::new(quark_hosts::EventHosts::new(log.clone(), host.clone())),
            log,
            host,
            classifier: Arc::new(quark_dispatch::SystemOne::default()),
            quota: Arc::new(quark_dispatch::QuotaAxi::new(&config.quota_axi)),
            families: crate::native_dispatch::families(config),
            settings: Settings {
                key_present: key,
                ..Settings::default()
            },
        }
    }
}

/// Native dispatch for the native engine. See the module docs.
pub struct NativeDispatch {
    dispatcher: Arc<Dispatcher>,
    supervisor: Arc<Supervisor>,
    log: Arc<dyn EventLog>,
    roots: Arc<Roots>,
    spawner: Arc<RecordingSpawner>,
    accounts: Arc<LateAccounts>,
    recorded: Recorded,
}

impl NativeDispatch {
    /// A dispatcher over `parts.log`, caught up with it, spawning on
    /// `supervisor`.
    pub async fn open(
        parts: DispatchParts,
        supervisor: Arc<Supervisor>,
    ) -> Result<Arc<Self>, EngineError> {
        let roots = Arc::new(Roots::default());
        let accounts = Arc::new(LateAccounts::default());
        let spawner = Arc::new(RecordingSpawner {
            inner: LocalSpawner {
                host: parts.host.clone(),
                supervisor: supervisor.clone(),
            },
            failures: Mutex::default(),
        });
        let dispatcher = Dispatcher::open(
            Parts {
                log: parts.log.clone(),
                host: parts.host,
                configs: roots.clone(),
                classifier: parts.classifier,
                quota: parts.quota,
                families: parts.families,
                accounts: accounts.clone(),
                hosts: parts.hosts,
                spawner: spawner.clone(),
                // The red-main guard comes with native verification (slice 2).
                guard: None,
                pool: None,
            },
            parts.settings,
        )
        .await
        .map_err(engine_error)?;
        Ok(Arc::new(Self {
            dispatcher: Arc::new(dispatcher),
            supervisor,
            log: parts.log,
            roots,
            spawner,
            accounts,
            recorded: Recorded::default(),
        }))
    }

    /// Where pool accounts come from. Until it is set, every worker runs
    /// under its harness's default configuration.
    pub fn set_accounts(&self, accounts: Arc<dyn Accounts>) {
        if self.accounts.0.set(accounts).is_err() {
            tracing::warn!("dispatch accounts were already set");
        }
    }

    /// Places waiting tasks every `every`, until aborted.
    pub fn run(&self, every: Duration) -> tokio::task::JoinHandle<()> {
        tokio::spawn(self.dispatcher.clone().run(every))
    }

    /// One placement pass now, as [`NativeDispatch::run`] makes on its
    /// timer.
    pub async fn place(&self) -> Result<(), EngineError> {
        self.dispatcher.tick().await.map(drop).map_err(engine_error)
    }

    /// Create the task, resolve its agent, and start it if a host has room.
    pub async fn start(&self, ws: &WorkspaceRef, task: &NewTask) -> Result<TaskStart, EngineError> {
        if task.title.trim().is_empty() || task.brief.trim().is_empty() {
            return Err(EngineError::Invalid(
                "a task needs a title and a brief".into(),
            ));
        }
        let p = self.roots.remember(ws);
        let repo = source_repo(&ws.root, task.repo.as_deref())?;
        let profile = task.agent.as_ref().map(profile_of);
        let base = slug(&task.title);
        let pending = self.dispatcher.pending().await.map_err(engine_error)?;
        for n in 1..=MAX_SUFFIX {
            let id = match n {
                1 => base.clone(),
                n => format!("{base}-{n}"),
            };
            let t = TaskId::from(id.as_str());
            let taken = self.supervisor.task(&p, &t).is_some()
                || pending.iter().any(|x| x.project == p && x.task == t);
            if taken {
                continue;
            }
            let request = Request {
                title: task.title.trim().to_string(),
                brief: task.brief.clone(),
                repo: repo.clone(),
                branch: format!("quark/{id}"),
                base: None,
                isolation: IsolationMode::Native,
                policy: Default::default(),
                env: BTreeMap::new(),
                profile: profile.clone(),
            };
            let submitted = match (&profile, task.rule.as_deref()) {
                (None, Some(rule)) => {
                    self.dispatcher
                        .submit_rule(p.clone(), t.clone(), request, rule)
                        .await
                }
                _ => self.dispatcher.submit(p.clone(), t.clone(), request).await,
            };
            match submitted {
                Ok(stage) => return self.settle(&p, &t, stage).await,
                // Dispatched before and gone: a withdrawn or failed task.
                Err(CoreError::Invalid(m)) if m.contains("already dispatched") => continue,
                Err(e) => return Err(engine_error(e)),
            }
        }
        Err(EngineError::Invalid(format!(
            "no free task id for {base:?}; give the task another title"
        )))
    }

    /// The coordinator's agent for a task whose dispatch escalated.
    pub async fn choose(
        &self,
        ws: &WorkspaceRef,
        task: &str,
        agent: &AgentConfig,
    ) -> Result<TaskStart, EngineError> {
        let p = self.roots.remember(ws);
        let t = task_id(task)?;
        let stage = self
            .dispatcher
            .choose(&p, &t, profile_of(agent))
            .await
            .map_err(engine_error)?;
        self.settle(&p, &t, stage).await
    }

    /// The dispatch rule the coordinator matched a task whose dispatch
    /// escalated to, resolved as a classifier's answer would be.
    pub async fn choose_rule(
        &self,
        ws: &WorkspaceRef,
        task: &str,
        rule: &str,
    ) -> Result<TaskStart, EngineError> {
        let p = self.roots.remember(ws);
        let t = task_id(task)?;
        let stage = self
            .dispatcher
            .choose_rule(&p, &t, rule)
            .await
            .map_err(engine_error)?;
        self.settle(&p, &t, stage).await
    }

    /// Place a chosen task now, and say where it stands.
    async fn settle(
        &self,
        p: &ProjectId,
        t: &TaskId,
        stage: Stage,
    ) -> Result<TaskStart, EngineError> {
        let account = match &stage {
            Stage::Escalated { reason } => {
                return Ok(started(
                    t,
                    StartOutcome::Escalated {
                        reason: reason.clone(),
                    },
                ))
            }
            Stage::Chosen { choice } | Stage::Placed { choice, .. } => choice.account.clone(),
            Stage::Requested => None,
        };
        self.dispatcher.tick().await.map_err(engine_error)?;
        if let Some((_, w)) = self.supervisor.task(p, t) {
            let a = &w.assignment;
            let g = w.current.as_ref();
            return Ok(started(
                t,
                StartOutcome::Started {
                    harness: g.map_or_else(|| a.harness.clone(), |g| g.harness.clone()),
                    model: g.map_or_else(|| a.model.clone(), |g| g.model.clone()),
                    effort: g.map_or_else(|| a.effort.clone(), |g| g.effort.clone()),
                    account,
                },
            ));
        }
        let pending = self.dispatcher.pending().await.map_err(engine_error)?;
        if let Some(x) = pending.iter().find(|x| &x.project == p && &x.task == t) {
            return Ok(started(t, waiting(x)));
        }
        let reason = self
            .spawner
            .failure(p, t)
            .unwrap_or_else(|| "its spawn was not recorded".into());
        Err(EngineError::Command(format!(
            "the worker for {t} did not start: {reason}"
        )))
    }

    /// The resolution that dispatched the task, as recorded; for a task
    /// dispatch has no record of, the rules run on its brief now. `None`
    /// for a task this engine does not know.
    pub async fn resolution(
        &self,
        ws: &WorkspaceRef,
        task: &str,
    ) -> Result<Option<EngineResolution>, EngineError> {
        let p = self.roots.remember(ws);
        let t = task_id(task)?;
        replay(self.log.as_ref(), &self.recorded, REPLAY_BATCH)
            .await
            .map_err(engine_error)?;
        if let Some((resolution, choice)) = self.recorded.get(&p, &t) {
            return Ok(Some(recorded_resolution(
                resolution.as_ref(),
                choice.as_ref(),
            )));
        }
        let brief = match self.supervisor.task(&p, &t) {
            Some((_, w)) => Some(w.assignment.brief),
            None => self
                .dispatcher
                .pending()
                .await
                .map_err(engine_error)?
                .into_iter()
                .find(|x| x.project == p && x.task == t)
                .map(|x| x.request.brief),
        };
        Ok(match brief {
            Some(b) => Some(engine_resolution(&self.dispatcher.resolution(&p, &b).await)),
            None => None,
        })
    }

    /// The Project's rules resolved on `description`, with nothing recorded.
    pub async fn describe(&self, ws: &WorkspaceRef, description: &str) -> EngineResolution {
        let p = self.roots.remember(ws);
        engine_resolution(&self.dispatcher.resolution(&p, description).await)
    }

    /// Replace the Project's dispatch rules. Rules that do not parse are
    /// refused and the old ones stay.
    pub async fn set_config(&self, ws: &WorkspaceRef, config: &str) -> Result<(), EngineError> {
        if !ws.root.is_dir() {
            return Err(EngineError::WorkspaceNotFound(ws.root.clone()));
        }
        DispatchConfig::parse(config)
            .map_err(|e| EngineError::Invalid(format!("crew dispatch config refused: {e}")))?;
        let path = ws.root.join(CONFIG_FILE);
        let text = config.to_string();
        tokio::task::spawn_blocking(move || write_atomic(&path, &text))
            .await
            .map_err(|e| EngineError::Command(e.to_string()))??;
        self.roots.remember(ws);
        Ok(())
    }

    /// Tasks of the Project still in dispatch's hands, as queued tasks,
    /// except those in `skip` (which the supervisor already reports).
    pub async fn queued(
        &self,
        ws: &WorkspaceRef,
        skip: &HashSet<String>,
    ) -> Result<Vec<EngineTask>, EngineError> {
        let p = ProjectId::new(ws.project_id.clone());
        Ok(self
            .dispatcher
            .pending()
            .await
            .map_err(engine_error)?
            .iter()
            .filter(|x| x.project == p && !skip.contains(x.task.as_str()))
            .map(queued_task)
            .collect())
    }
}

fn started(t: &TaskId, outcome: StartOutcome) -> TaskStart {
    TaskStart {
        task_id: t.to_string(),
        outcome,
    }
}

fn waiting(p: &Pending) -> StartOutcome {
    match &p.stage {
        Stage::Escalated { reason } => StartOutcome::Escalated {
            reason: reason.clone(),
        },
        _ => StartOutcome::Waiting { reason: note(p) },
    }
}

/// Where a pending task stands, for a person.
fn note(p: &Pending) -> String {
    match &p.stage {
        Stage::Requested => "resolving its agent".into(),
        Stage::Escalated { reason } => format!("waiting for its agent to be chosen: {reason}"),
        Stage::Chosen { .. } => p
            .held
            .clone()
            .unwrap_or_else(|| "waiting for a host".into()),
        Stage::Placed { host, .. } => format!("starting on host {host}"),
    }
}

fn queued_task(p: &Pending) -> EngineTask {
    let harness = match &p.stage {
        Stage::Chosen { choice } | Stage::Placed { choice, .. } => {
            Some(choice.profile.harness.clone())
        }
        _ => None,
    };
    EngineTask {
        id: p.task.to_string(),
        title: p.request.title.clone(),
        kind: Some(TaskKind::Ship),
        state: TaskState::Queued,
        state_note: Some(note(p)),
        state_source: Some(super::native::SOURCE.to_string()),
        harness,
        pull_request_url: None,
        terminal: None,
        worktree: None,
    }
}

fn profile_of(agent: &AgentConfig) -> Profile {
    Profile {
        harness: agent.harness.clone(),
        model: agent.model.clone(),
        effort: agent.effort.clone(),
        pool: agent.pool.clone(),
        ..Profile::default()
    }
}

fn task_id(task: &str) -> Result<TaskId, EngineError> {
    quark_engine::validate_task_id(task).map_err(|_| EngineError::TaskNotFound(task.into()))?;
    Ok(TaskId::from(task))
}

fn engine_error(e: CoreError) -> EngineError {
    match e {
        CoreError::NotFound(m) => EngineError::TaskNotFound(m),
        CoreError::Invalid(m) | CoreError::Refused(m) | CoreError::Unsupported(m) => {
            EngineError::Invalid(m)
        }
        other => EngineError::Command(other.to_string()),
    }
}

/// A task id made from `title`: lowercase letters, digits and dashes, at
/// most [`MAX_SLUG`] long, `task` when nothing is left. It always passes
/// [`quark_engine::validate_task_id`].
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= MAX_SLUG {
            break;
        }
    }
    let out = out.trim_end_matches('-');
    if out.is_empty() {
        "task".into()
    } else {
        out.to_string()
    }
}

/// The primary checkout of the source repo `name` in the workspace at
/// `root` (see the module docs), or of its only repo when `name` is `None`.
pub fn source_repo(root: &Path, name: Option<&str>) -> Result<PathBuf, EngineError> {
    let dirs = [root.join(REPOS_DIR), root.join(FIRSTMATE_REPOS_DIR)];
    if let Some(name) = name {
        if quark_engine::validate_task_id(name).is_err() {
            return Err(EngineError::Invalid(format!("{name:?} is not a repo name")));
        }
        return dirs
            .iter()
            .map(|d| d.join(name))
            .find(|p| p.join(".git").exists())
            .ok_or_else(|| {
                EngineError::Invalid(format!(
                    "no repo {name} in {}",
                    root.join(REPOS_DIR).display()
                ))
            });
    }
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    for d in &dirs {
        let Ok(entries) = std::fs::read_dir(d) else {
            continue;
        };
        let mut here: Vec<(String, PathBuf)> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join(".git").exists())
            .filter_map(|p| Some((p.file_name()?.to_str()?.to_string(), p)))
            .filter(|(n, _)| !found.iter().any(|(f, _)| f == n))
            .collect();
        here.sort();
        found.extend(here);
    }
    match found.as_slice() {
        [(_, only)] => Ok(only.clone()),
        [] => Err(EngineError::Invalid(format!(
            "the workspace has no repo in {}",
            root.join(REPOS_DIR).display()
        ))),
        many => Err(EngineError::Invalid(format!(
            "name the repo: one of {}",
            many.iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn write_atomic(path: &Path, text: &str) -> Result<(), EngineError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// The neutral form of a resolution.
pub fn engine_resolution(r: &Resolution) -> EngineResolution {
    EngineResolution {
        status: match r.status {
            Status::Clear => DispatchStatus::Clear,
            Status::Ambiguous => DispatchStatus::Ambiguous,
            Status::Escalate => DispatchStatus::Escalate,
            Status::Error => DispatchStatus::Error,
            Status::Off => DispatchStatus::Off,
        },
        rule: r.rule.as_ref().map(|id| DispatchRule {
            id: id.clone(),
            when: r.rule_when.clone(),
        }),
        reason: r.reason.clone(),
        notes: r.notes.clone(),
        candidates: r
            .candidates
            .iter()
            .map(|c| DispatchCandidate {
                harness: c.profile.harness.clone(),
                model: c.profile.model.clone(),
                passed: c.verdict.eligible(),
                reason: c.verdict.reason(),
                evidence: c.evidence(),
            })
            .collect(),
        profile: r.profile().map(|p| choice_of(p, None)),
        fallback: r.fallback.clone(),
        classifier_consulted: r.answer.is_some(),
        classifier_model: r.answer.as_ref().and_then(|a| a.model.clone()),
        confidence: r.answer.as_ref().map(|a| a.confidence),
        output: None,
    }
}

fn choice_of(p: &Profile, account: Option<&str>) -> DispatchChoice {
    DispatchChoice {
        harness: p.harness.clone(),
        model: p.model.clone(),
        effort: p.effort.clone(),
        account: account.map(str::to_string),
    }
}

/// A task's recorded dispatch: the resolution, when the rules ran, and the
/// agent and account chosen.
fn recorded_resolution(
    resolution: Option<&Resolution>,
    choice: Option<&Choice>,
) -> EngineResolution {
    let mut r = match resolution {
        Some(r) => engine_resolution(r),
        None => EngineResolution {
            status: DispatchStatus::NotConsulted,
            reason: Some("the agent was named when the task was started".into()),
            ..engine_resolution(&Resolution::off())
        },
    };
    if let Some(c) = choice {
        if r.status == DispatchStatus::Clear {
            r.profile = Some(choice_of(&c.profile, c.account.as_deref()));
        } else {
            r.notes
                .push(format!("the coordinator chose {}", c.profile.label()));
        }
    }
    r
}

/// Each Project's workspace root, learned from the calls that name it, and
/// the dispatch rules read from it.
#[derive(Default)]
struct Roots(Mutex<HashMap<ProjectId, PathBuf>>);

impl Roots {
    fn remember(&self, ws: &WorkspaceRef) -> ProjectId {
        let p = ProjectId::new(ws.project_id.clone());
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(p.clone(), ws.root.clone());
        p
    }
}

#[async_trait]
impl Configs for Roots {
    async fn config(&self, project: &ProjectId) -> Result<DispatchConfig, String> {
        let root = self
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(project)
            .cloned();
        let Some(root) = root else {
            return Err(format!("no workspace is known for Project {project}"));
        };
        match tokio::fs::read_to_string(root.join(CONFIG_FILE)).await {
            Ok(text) => DispatchConfig::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DispatchConfig::default()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// The local supervisor, remembering why a spawn failed.
struct RecordingSpawner {
    inner: LocalSpawner,
    failures: Mutex<HashMap<Key, String>>,
}

impl RecordingSpawner {
    fn failure(&self, p: &ProjectId, t: &TaskId) -> Option<String> {
        self.failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(p.clone(), t.clone()))
            .cloned()
    }
}

#[async_trait]
impl Spawner for RecordingSpawner {
    async fn spawn(&self, host: &HostId, request: SpawnRequest) -> quark_core::Result<()> {
        let key = (request.project.clone(), request.task.clone());
        let res = self.inner.spawn(host, request).await;
        if let Err(e) = &res {
            self.failures
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, e.to_string());
        }
        res
    }

    async fn has(
        &self,
        host: &HostId,
        project: &ProjectId,
        task: &TaskId,
    ) -> quark_core::Result<bool> {
        self.inner.has(host, project, task).await
    }
}

/// Pool accounts, once the daemon has them.
#[derive(Default)]
struct LateAccounts(OnceLock<Arc<dyn Accounts>>);

#[async_trait]
impl Accounts for LateAccounts {
    async fn accounts(&self, harness: &str) -> Result<Vec<PoolAccount>, String> {
        match self.0.get() {
            Some(a) => a.accounts(harness).await,
            None => NoAccounts.accounts(harness).await,
        }
    }
}

/// The daemon's accounts and pools ([`crate::accounts`]) as dispatch sees
/// them.
pub struct StoreAccounts(pub Arc<crate::accounts::Accounts>);

#[async_trait]
impl Accounts for StoreAccounts {
    async fn accounts(&self, harness: &str) -> Result<Vec<PoolAccount>, String> {
        self.0
            .pool_accounts(harness)
            .await
            .map_err(|e| e.to_string())
    }
}

/// Each task's recorded resolution and choice, folded from `dispatch.*`
/// events.
#[derive(Default)]
struct Recorded(Mutex<RecordedInner>);

type Dispatched = (Option<Resolution>, Option<Choice>);

#[derive(Default)]
struct RecordedInner {
    applied: Seq,
    tasks: HashMap<Key, Dispatched>,
}

impl Recorded {
    fn lock(&self) -> std::sync::MutexGuard<'_, RecordedInner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn get(&self, p: &ProjectId, t: &TaskId) -> Option<Dispatched> {
        self.lock().tasks.get(&(p.clone(), t.clone())).cloned()
    }
}

#[async_trait]
impl ReadModel for Recorded {
    async fn applied_through(&self) -> quark_core::Result<Seq> {
        Ok(self.lock().applied)
    }

    async fn apply(&self, event: &Event) -> quark_core::Result<()> {
        let mut inner = self.lock();
        if event.seq <= inner.applied {
            return Ok(());
        }
        inner.applied = event.seq;
        let Some(task) = event.task.clone().filter(|_| event.kind.prefix() == PREFIX) else {
            return Ok(());
        };
        let key = (event.project.clone(), task);
        match event.decode::<DispatchEvent>() {
            Ok(DispatchEvent::Resolved { resolution }) => {
                inner.tasks.entry(key).or_default().0 = Some(resolution);
            }
            Ok(DispatchEvent::Chosen { choice }) => {
                inner.tasks.entry(key).or_default().1 = Some(choice);
            }
            _ => {}
        }
        Ok(())
    }

    async fn reset(&self) -> quark_core::Result<()> {
        *self.lock() = RecordedInner::default();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_valid_task_ids() {
        for (title, want) in [
            ("Fix the parser", "fix-the-parser"),
            ("  --Hello, World!!  ", "hello-world"),
            ("日本語", "task"),
            ("", "task"),
        ] {
            let s = slug(title);
            assert_eq!(s, want);
            quark_engine::validate_task_id(&s).unwrap();
        }
        let long = slug(&"word ".repeat(30));
        assert!(long.len() <= MAX_SLUG && !long.ends_with('-'), "{long}");
    }

    #[test]
    fn repos_come_from_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert!(source_repo(root, None).is_err());
        std::fs::create_dir_all(root.join("repos/app/.git")).unwrap();
        assert_eq!(source_repo(root, None).unwrap(), root.join("repos/app"));
        // firstmate's layout is read too.
        std::fs::create_dir_all(root.join("projects/site/.git")).unwrap();
        let both = source_repo(root, None).unwrap_err().to_string();
        assert!(both.contains("app, site"), "{both}");
        assert_eq!(
            source_repo(root, Some("site")).unwrap(),
            root.join("projects/site")
        );
        assert!(source_repo(root, Some("nope")).is_err());
        assert!(source_repo(root, Some("../app")).is_err());
    }
}
