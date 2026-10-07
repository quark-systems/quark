//! [`SubCoordinators`]: register, seed, launch, steer, hand off to,
//! supervise and retire sub-coordinators on any runtime.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash as _, Hasher as _};
use std::path::{Component, Path};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use quark_core::harness::HarnessManifest;
use quark_core::host::Health;
use quark_core::session::{SessionBackend, SessionId, SessionSpec, TermSize};
use quark_core::{replay, CoreError, EventId, EventLog, HostId, NewEvent, ProjectId, Result, Seq};
use quark_harness::{LaunchVars, ManifestRegistry};
use quark_runtime::{fs, Exec, Options, TmuxSessions};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::events::{Cause, Message, MessageKind, SubEvent};
use crate::home::{self, Layout};
use crate::model::{Placement, Profile, Registration};
use crate::readiness::{self, Readiness};
use crate::registry::{Registry, SubCoordinator};

/// Events read per batch when replaying.
const BATCH: usize = 512;

/// Environment every sub-coordinator agent gets.
pub mod env {
    pub const PROJECT: &str = "QUARK_PROJECT";
    pub const GENERATION: &str = "QUARK_GENERATION";
    pub const HOME: &str = "QUARK_HOME";
    pub const INBOX: &str = "QUARK_INBOX";
    /// The file to append status lines for the parent to.
    pub const PARENT_CHANNEL: &str = "QUARK_PARENT_CHANNEL";
}

/// Tunables. [`Config::new`] has the defaults.
#[derive(Debug, Clone)]
pub struct Config {
    /// This machine, stamped on every event and written into each home.
    pub host: HostId,
    /// Times in a row an agent whose session ended unexpectedly is
    /// relaunched before the engine stops and leaves it to a person.
    pub max_recoveries: u32,
    /// Longest to wait for a pasted-prompt harness to be ready before
    /// typing its brief anyway.
    pub ready_timeout: Duration,
    /// How long the screen must stay still to count as ready.
    pub ready_quiet: Duration,
    /// Most bytes of a parent channel read per pass.
    pub channel_chunk: u64,
    pub size: TermSize,
}

impl Config {
    pub fn new(host: HostId) -> Self {
        Self {
            host,
            max_recoveries: 3,
            ready_timeout: Duration::from_secs(30),
            ready_quiet: Duration::from_millis(1500),
            channel_chunk: 64 * 1024,
            size: TermSize::default(),
        }
    }
}

/// How the engine reaches one placement.
#[derive(Clone)]
pub struct Connection {
    pub exec: Arc<dyn Exec>,
    pub sessions: Arc<dyn SessionBackend>,
    /// Whether the sessions are tmux on the host, which readiness then
    /// requires.
    pub tmux: bool,
}

/// Turns a placement into a connection.
pub trait Hosts: Send + Sync {
    fn connect(&self, placement: &Placement) -> Result<Connection>;
}

/// The runtimes of `quark-runtime`: local commands with this machine's
/// session backend (or tmux), and tmux over SSH for SSH hosts.
pub struct RuntimeHosts {
    options: Options,
    local_sessions: Option<Arc<dyn SessionBackend>>,
    socket: String,
}

impl RuntimeHosts {
    pub fn new(options: Options) -> Self {
        Self {
            options,
            local_sessions: None,
            socket: quark_runtime::TMUX_SOCKET.into(),
        }
    }

    /// Run local sub-coordinators in `sessions` (quarkd's PTY supervisor)
    /// instead of tmux.
    pub fn with_local_sessions(mut self, sessions: Arc<dyn SessionBackend>) -> Self {
        self.local_sessions = Some(sessions);
        self
    }

    /// The tmux socket name (`tmux -L`) on every host.
    pub fn with_socket(mut self, socket: impl Into<String>) -> Self {
        self.socket = socket.into();
        self
    }
}

impl Hosts for RuntimeHosts {
    fn connect(&self, p: &Placement) -> Result<Connection> {
        let exec = quark_runtime::connect(&p.runtime, p.host.clone(), &self.options)?;
        let local = matches!(p.runtime, quark_runtime::RuntimeSpec::Local);
        Ok(match (&self.local_sessions, local) {
            (Some(s), true) => Connection {
                exec,
                sessions: s.clone(),
                tmux: false,
            },
            _ => Connection {
                sessions: Arc::new(TmuxSessions::with_socket(exec.clone(), &self.socket)),
                exec,
                tmux: true,
            },
        })
    }
}

/// A work item moved into a sub-coordinator's queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffItem {
    /// Stable key, unique within the handoff.
    pub key: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// Keys this item waits on.
    #[serde(default)]
    pub depends_on: Vec<String>,
}

/// What the last [`SubCoordinators::tick`] did, for logs and tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Tick {
    pub health_changes: usize,
    /// Hosts that could not be reached; nothing on them was touched.
    pub unreachable: usize,
    pub exited: usize,
    pub recovered: usize,
    pub delivered: usize,
    pub acknowledged: usize,
    pub reports: usize,
    pub errors: usize,
}

/// Runs the sub-coordinators.
///
/// Every change is a `subcoordinator.*` event first, folded into the
/// [`Registry`], so a new instance on the same log carries on where a
/// crashed one stopped: sessions on their hosts outlive it, messages not
/// yet in an inbox are delivered on the next pass, and the parent channel
/// is read on from the last recorded line.
pub struct SubCoordinators {
    log: Arc<dyn EventLog>,
    registry: Registry,
    manifests: Arc<ManifestRegistry>,
    hosts: Arc<dyn Hosts>,
    config: Config,
    locks: Mutex<HashMap<ProjectId, Arc<tokio::sync::Mutex<()>>>>,
}

impl SubCoordinators {
    /// Open on `log`, replaying what it already holds.
    pub async fn open(
        log: Arc<dyn EventLog>,
        manifests: Arc<ManifestRegistry>,
        hosts: Arc<dyn Hosts>,
        config: Config,
    ) -> Result<Self> {
        let registry = Registry::new();
        replay(log.as_ref(), &registry, BATCH).await?;
        Ok(Self {
            log,
            registry,
            manifests,
            hosts,
            config,
            locks: Mutex::default(),
        })
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    pub fn get(&self, id: &ProjectId) -> Option<SubCoordinator> {
        self.registry.get(id)
    }

    /// Record a new sub-coordinator. Nothing happens on its host yet.
    pub async fn register(&self, r: Registration) -> Result<SubCoordinator> {
        r.validate()?;
        let m = self.manifest(&r.profile.harness)?;
        check_profile(&m, &r.profile)?;
        let _guard = self.lock(&r.id).await;
        self.refresh().await?;
        if self.registry.get(&r.id).is_some_and(|s| s.is_live()) {
            return Err(CoreError::Invalid(format!(
                "sub-coordinator {} already exists",
                r.id
            )));
        }
        if let Some(other) = self
            .registry
            .live()
            .into_iter()
            .find(|s| s.registration.placement.overlaps(&r.placement))
        {
            return Err(CoreError::Refused(format!(
                "{} overlaps the home of sub-coordinator {}",
                r.placement.home.display(),
                other.id()
            )));
        }
        self.record(
            &r.id,
            SubEvent::Registered {
                registration: r.clone(),
            },
        )
        .await?;
        self.live(&r.id)
    }

    /// Whether the sub-coordinator's host is ready for it.
    pub async fn doctor(&self, id: &ProjectId) -> Result<Readiness> {
        let s = self.live(id)?;
        let profile = current_profile(&s);
        let conn = self.hosts.connect(&s.registration.placement)?;
        readiness::check(
            conn.exec.as_ref(),
            &*self.manifest(&profile.harness)?,
            conn.tmux,
        )
        .await
    }

    /// Create its home on its host and clone its projects. Safe to repeat.
    pub async fn seed(&self, id: &ProjectId) -> Result<SubCoordinator> {
        let _guard = self.lock(id).await;
        self.refresh().await?;
        let s = self.live(id)?;
        let conn = self.hosts.connect(&s.registration.placement)?;
        self.gate(&conn, &current_profile(&s)).await?;
        home::seed(
            conn.exec.as_ref(),
            &s.registration,
            self.config.host.as_str(),
        )
        .await?;
        if !s.seeded {
            self.record(id, SubEvent::Seeded).await?;
        }
        self.live(id)
    }

    /// Start its agent, or return the running one.
    pub async fn launch(&self, id: &ProjectId) -> Result<String> {
        let _guard = self.lock(id).await;
        self.refresh().await?;
        let s = self.seeded(id)?;
        let conn = self.hosts.connect(&s.registration.placement)?;
        if let Some(g) = s.current.as_ref().filter(|g| g.exited.is_none()) {
            match session_state(conn.sessions.as_ref(), &g.session).await? {
                SessionState::Running => return Ok(g.id.clone()),
                state => {
                    self.record(
                        id,
                        SubEvent::Exited {
                            generation: g.id.clone(),
                            code: state.code(),
                            stopped: false,
                        },
                    )
                    .await?;
                }
            }
        }
        let s = self.live(id)?;
        let profile = current_profile(&s);
        self.start(&conn, &s, Cause::Launch, profile, None).await
    }

    /// Replace its agent with a new one, optionally on another harness,
    /// model or effort, telling it `note` first.
    pub async fn relaunch(
        &self,
        id: &ProjectId,
        profile: Option<Profile>,
        note: &str,
    ) -> Result<String> {
        let _guard = self.lock(id).await;
        self.refresh().await?;
        let s = self.seeded(id)?;
        let profile = profile.unwrap_or_else(|| current_profile(&s));
        check_profile(&*self.manifest(&profile.harness)?, &profile)?;
        let conn = self.hosts.connect(&s.registration.placement)?;
        self.stop_current(&conn, &s).await?;
        let s = self.live(id)?;
        self.start(&conn, &s, Cause::Relaunch, profile, Some(note))
            .await
    }

    /// Stop its agent without retiring it; [`SubCoordinators::launch`]
    /// starts it again.
    pub async fn stop(&self, id: &ProjectId) -> Result<()> {
        let _guard = self.lock(id).await;
        self.refresh().await?;
        let s = self.live(id)?;
        let conn = self.hosts.connect(&s.registration.placement)?;
        self.stop_current(&conn, &s).await
    }

    /// Send its agent a message. Recorded first, so it is delivered even if
    /// the host is unreachable now. Returns the message id.
    pub async fn send(&self, id: &ProjectId, text: &str) -> Result<String> {
        if text.trim().is_empty() {
            return Err(CoreError::Invalid("empty message".into()));
        }
        self.enqueue(id, MessageKind::Steer, text.trim().to_string(), None)
            .await
    }

    /// Answer a question it asked (`needs-decision [key=<key>]`).
    pub async fn answer(&self, id: &ProjectId, key: &str, text: &str) -> Result<String> {
        if !self.live(id)?.decisions.contains_key(key) {
            return Err(CoreError::NotFound(format!("open decision {key} on {id}")));
        }
        self.enqueue(
            id,
            MessageKind::Steer,
            format!("Answer to [{key}]: {}", text.trim()),
            Some(key.to_string()),
        )
        .await
    }

    /// Move work items into its queue. Returns the message id; the handoff
    /// is complete once its agent acknowledges it.
    pub async fn handoff(&self, id: &ProjectId, items: &[HandoffItem]) -> Result<String> {
        if items.is_empty() {
            return Err(CoreError::Invalid("nothing to hand off".into()));
        }
        let mut keys = std::collections::BTreeSet::new();
        for i in items {
            if i.key.trim().is_empty() || i.title.trim().is_empty() {
                return Err(CoreError::Invalid(
                    "every item needs a key and a title".into(),
                ));
            }
            if !keys.insert(i.key.as_str()) {
                return Err(CoreError::Invalid(format!("item {} listed twice", i.key)));
            }
        }
        let body = serde_json::to_string_pretty(items)
            .map_err(|e| CoreError::Invalid(format!("handoff: {e}")))?;
        self.enqueue(id, MessageKind::Handoff, body, None).await
    }

    /// Write inherited configuration (relative path to contents) into its
    /// home, and tell a running agent to re-read it. Returns whether
    /// anything changed.
    pub async fn inherit(
        &self,
        id: &ProjectId,
        material: &BTreeMap<String, Vec<u8>>,
    ) -> Result<bool> {
        for name in material.keys() {
            check_relative(name)?;
        }
        let digest = digest(material);
        let _guard = self.lock(id).await;
        self.refresh().await?;
        let s = self.seeded(id)?;
        if s.inherited.as_deref() == Some(digest.as_str()) {
            return Ok(false);
        }
        let conn = self.hosts.connect(&s.registration.placement)?;
        let dir = Layout::new(&s.registration.placement.home).inherited();
        for (name, bytes) in material {
            fs::write(conn.exec.as_ref(), &dir.join(name), bytes).await?;
        }
        let files: Vec<String> = material.keys().cloned().collect();
        self.record(
            id,
            SubEvent::Inherited {
                digest,
                files: files.clone(),
            },
        )
        .await?;
        if s.is_running() {
            let text = format!(
                "Your inherited configuration changed ({}). Re-read it in {}.",
                files.join(", "),
                dir.display()
            );
            self.push(&conn, id, MessageKind::Steer, text, None).await?;
        }
        Ok(true)
    }

    /// [`SubCoordinators::inherit`] for every seeded, live sub-coordinator.
    pub async fn inherit_all(
        &self,
        material: &BTreeMap<String, Vec<u8>>,
    ) -> Vec<(ProjectId, Result<bool>)> {
        let mut out = Vec::new();
        for s in self.registry.live().into_iter().filter(|s| s.seeded) {
            let r = self.inherit(s.id(), material).await;
            out.push((s.id().clone(), r));
        }
        out
    }

    /// Stop it for good. Refused while a handoff is not yet taken, or while
    /// its host cannot be reached to stop the agent, unless `force`. The
    /// home stays on its host.
    pub async fn retire(&self, id: &ProjectId, reason: &str, force: bool) -> Result<()> {
        let _guard = self.lock(id).await;
        self.refresh().await?;
        let s = self.live(id)?;
        let pending = s.pending_handoffs().count();
        if pending > 0 && !force {
            return Err(CoreError::Refused(format!(
                "{id} has {pending} handoff(s) its agent has not taken yet"
            )));
        }
        let conn = self.hosts.connect(&s.registration.placement)?;
        match self.stop_current(&conn, &s).await {
            Ok(()) => {
                let _ = self.kill_orphans(&conn, &s).await;
            }
            Err(e) if force => tracing::warn!(%id, error = %e, "retiring without stopping"),
            Err(e) => {
                return Err(CoreError::Refused(format!(
                    "could not stop {id}'s agent: {e}"
                )))
            }
        }
        self.record(
            id,
            SubEvent::Retired {
                reason: reason.to_string(),
                forced: force,
            },
        )
        .await
    }

    /// One supervision pass over every live sub-coordinator: record host
    /// health, relaunch agents whose sessions ended, deliver waiting
    /// messages, note acknowledgements and read the parent channels. A host
    /// that cannot be reached is left alone: its agent may well be running.
    pub async fn tick(&self) -> Result<Tick> {
        self.refresh().await?;
        let mut t = Tick::default();
        for s in self.registry.live() {
            let id = s.id().clone();
            if let Err(e) = self.tick_one(&id, &mut t).await {
                t.errors += 1;
                tracing::warn!(%id, error = %e, "sub-coordinator pass failed");
            }
        }
        Ok(t)
    }

    /// Run [`SubCoordinators::tick`] every `every` until the task is
    /// dropped.
    pub async fn run(self: Arc<Self>, every: Duration) {
        let mut timer = tokio::time::interval(every);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            timer.tick().await;
            if let Err(e) = self.tick().await {
                tracing::warn!(error = %e, "sub-coordinator supervision failed");
            }
        }
    }

    async fn tick_one(&self, id: &ProjectId, t: &mut Tick) -> Result<()> {
        let _guard = self.lock(id).await;
        self.refresh().await?;
        let s = self.live(id)?;
        let conn = self.hosts.connect(&s.registration.placement)?;
        let health = conn.exec.health().await?;
        if s.health.as_ref() != Some(&health) {
            self.record(
                id,
                SubEvent::Health {
                    health: health.clone(),
                },
            )
            .await?;
            t.health_changes += 1;
        }
        if health != Health::Healthy {
            t.unreachable += 1;
            return Ok(());
        }
        if !s.seeded {
            return Ok(());
        }
        if let Some(g) = s.current.as_ref().filter(|g| g.exited.is_none()) {
            let state = session_state(conn.sessions.as_ref(), &g.session).await?;
            if state != SessionState::Running {
                self.record(
                    id,
                    SubEvent::Exited {
                        generation: g.id.clone(),
                        code: state.code(),
                        stopped: false,
                    },
                )
                .await?;
                t.exited += 1;
                if s.recoveries >= self.config.max_recoveries {
                    tracing::warn!(%id, recoveries = s.recoveries, "agent keeps exiting; left stopped");
                }
            }
        }
        // An agent that ended on its own is replaced, retrying on later
        // passes if the host is not ready, up to the limit.
        let s = self.live(id)?;
        if let Some(g) = s
            .current
            .as_ref()
            .filter(|g| g.exited.is_some() && !g.stopped)
        {
            if s.recoveries < self.config.max_recoveries {
                let note = format!(
                    "Your previous session ended unexpectedly{}. Pick up where it left off: \
                     check your inbox, and your parent channel for what you last reported.",
                    g.exited
                        .flatten()
                        .map(|c| format!(" (exit code {c})"))
                        .unwrap_or_default()
                );
                self.start(&conn, &s, Cause::Recover, g.profile.clone(), Some(&note))
                    .await?;
                t.recovered += 1;
            }
        }
        let s = self.live(id)?;
        self.kill_orphans(&conn, &s).await?;
        t.delivered += self.deliver(&conn, &s).await?;
        t.acknowledged += self.acknowledge(&conn, &s).await?;
        t.reports += self.read_channel(&conn, &s).await?;
        Ok(())
    }

    async fn enqueue(
        &self,
        id: &ProjectId,
        kind: MessageKind,
        body: String,
        answers: Option<String>,
    ) -> Result<String> {
        let _guard = self.lock(id).await;
        self.refresh().await?;
        let s = self.live(id)?;
        let conn = self.hosts.connect(&s.registration.placement);
        match conn {
            Ok(conn) => self.push(&conn, id, kind, body, answers).await,
            Err(e) => {
                tracing::warn!(%id, error = %e, "message kept for later delivery");
                self.record_message(id, kind, body, answers).await
            }
        }
    }

    /// Record a message and try to deliver it now. The caller holds the
    /// lock.
    async fn push(
        &self,
        conn: &Connection,
        id: &ProjectId,
        kind: MessageKind,
        body: String,
        answers: Option<String>,
    ) -> Result<String> {
        let mid = self.record_message(id, kind, body, answers).await?;
        let s = self.live(id)?;
        if s.seeded {
            if let Err(e) = self.deliver(conn, &s).await {
                tracing::warn!(%id, error = %e, "message kept for later delivery");
            }
        }
        Ok(mid)
    }

    async fn record_message(
        &self,
        id: &ProjectId,
        kind: MessageKind,
        body: String,
        answers: Option<String>,
    ) -> Result<String> {
        let mid = Uuid::new_v4().simple().to_string();
        self.record(
            id,
            SubEvent::Sent {
                message: Message {
                    id: mid.clone(),
                    kind,
                    body,
                    answers,
                },
            },
        )
        .await?;
        Ok(mid)
    }

    /// Write every undelivered message into the inbox, then ring the agent.
    async fn deliver(&self, conn: &Connection, s: &SubCoordinator) -> Result<usize> {
        if !s.seeded {
            return Ok(0);
        }
        let layout = Layout::new(&s.registration.placement.home);
        let mut written = Vec::new();
        for (n, o) in s.outgoing.iter().enumerate().filter(|(_, o)| !o.delivered) {
            let m = &o.message;
            let path = layout.message(n + 1, &m.id, m.kind.suffix());
            fs::write(conn.exec.as_ref(), &path, m.body.as_bytes()).await?;
            self.record(s.id(), SubEvent::Delivered { id: m.id.clone() })
                .await?;
            written.push(path);
        }
        if let (Some(g), false) = (
            s.current.as_ref().filter(|g| g.exited.is_none()),
            written.is_empty(),
        ) {
            let names: Vec<String> = written.iter().map(|p| p.display().to_string()).collect();
            let bell = format!("New message from your parent: {}", names.join(", "));
            if let Err(e) = conn.sessions.input(&g.session, &paste(&bell)).await {
                // The file is the message; the next launch reads the inbox.
                tracing::debug!(id = %s.id(), error = %e, "doorbell not rung");
            }
        }
        Ok(written.len())
    }

    /// Record messages the agent moved to `inbox/handled`.
    async fn acknowledge(&self, conn: &Connection, s: &SubCoordinator) -> Result<usize> {
        if !s.outgoing.iter().any(|o| o.delivered && !o.acknowledged) {
            return Ok(0);
        }
        let dir = Layout::new(&s.registration.placement.home).handled();
        let done: std::collections::BTreeSet<String> = fs::list(conn.exec.as_ref(), &dir)
            .await?
            .iter()
            .filter_map(|n| home::message_id(n).map(str::to_string))
            .collect();
        let mut n = 0;
        for o in s.outgoing.iter().filter(|o| !o.acknowledged) {
            if done.contains(&o.message.id) {
                self.record(
                    s.id(),
                    SubEvent::Acknowledged {
                        id: o.message.id.clone(),
                    },
                )
                .await?;
                n += 1;
            }
        }
        Ok(n)
    }

    /// Record the complete lines appended to the parent channel since the
    /// last one recorded. Event ids come from the sub-coordinator, offset
    /// and line, so reading the same line again records nothing.
    async fn read_channel(&self, conn: &Connection, s: &SubCoordinator) -> Result<usize> {
        let path = Layout::new(&s.registration.placement.home).channel();
        let Some(bytes) = fs::read_from(
            conn.exec.as_ref(),
            &path,
            s.channel,
            self.config.channel_chunk,
        )
        .await?
        else {
            return Ok(0);
        };
        // Complete lines only; a line longer than a whole chunk is cut
        // there so the cursor always moves.
        let mut lines = Vec::new();
        let mut at = s.channel;
        let mut rest = bytes.as_slice();
        while let Some(end) = rest.iter().position(|b| *b == b'\n') {
            lines.push((at, at + end as u64 + 1, &rest[..end]));
            at += end as u64 + 1;
            rest = &rest[end + 1..];
        }
        if lines.is_empty() && bytes.len() as u64 >= self.config.channel_chunk {
            lines.push((at, at + bytes.len() as u64, bytes.as_slice()));
        }
        let mut n = 0;
        for (offset, end, raw) in lines {
            let line = String::from_utf8_lossy(raw)
                .trim_end_matches('\r')
                .to_string();
            if line.trim().is_empty() {
                continue;
            }
            let message = quark_worker::parse_status_line(&line).ok().flatten();
            let event = SubEvent::Report {
                offset,
                end,
                line,
                message,
            };
            let mut e = NewEvent::typed(
                self.config.host.clone(),
                s.id().clone(),
                None,
                event.kind(),
                &event,
            )?;
            e.id = line_id(s.id(), offset, raw);
            self.log.append(e).await?;
            n += 1;
        }
        if n > 0 {
            self.refresh().await?;
        }
        Ok(n)
    }

    /// Launch a new generation. The caller holds the lock and has stopped
    /// any previous one.
    async fn start(
        &self,
        conn: &Connection,
        s: &SubCoordinator,
        cause: Cause,
        profile: Profile,
        note: Option<&str>,
    ) -> Result<String> {
        let m = self.manifest(&profile.harness)?;
        check_profile(&m, &profile)?;
        self.gate(conn, &profile).await?;
        let r = &s.registration;
        let layout = Layout::new(&r.placement.home);
        let generation = Uuid::new_v4().simple().to_string()[..12].to_string();
        let brief = home::brief(r, note);
        fs::write(conn.exec.as_ref(), &layout.brief(), brief.as_bytes()).await?;

        let on_argv = m.launch.prompt_via == "argv";
        let model = match m.models.selection.as_str() {
            "automatic" | "none" => None,
            _ => profile.model.clone().or(m.models.default.clone()),
        };
        let argv = quark_harness::argv(
            &m,
            &LaunchVars {
                model,
                effort: profile.effort.clone(),
                prompt: on_argv.then(|| brief.clone()),
                prompt_file: on_argv.then(|| layout.brief().display().to_string()),
                cwd: Some(r.placement.home.display().to_string()),
            },
        );
        let mut env_vars = m.launch.env.clone();
        env_vars.insert(env::PROJECT.into(), r.id.to_string());
        env_vars.insert(env::GENERATION.into(), generation.clone());
        env_vars.insert(env::HOME.into(), r.placement.home.display().to_string());
        env_vars.insert(env::INBOX.into(), layout.inbox().display().to_string());
        env_vars.insert(
            env::PARENT_CHANNEL.into(),
            layout.channel().display().to_string(),
        );
        let info = conn
            .sessions
            .create(&SessionSpec {
                task: None,
                name: session_name(&r.id, &generation),
                cwd: r.placement.home.clone(),
                argv,
                env: env_vars,
                size: self.config.size,
            })
            .await?;
        self.record(
            &r.id,
            SubEvent::Launched {
                generation: generation.clone(),
                session: info.id.clone(),
                cause,
                profile,
            },
        )
        .await?;
        if !on_argv {
            self.wait_ready(conn.sessions.as_ref(), &info.id, &m).await;
            conn.sessions.input(&info.id, &paste(&brief)).await?;
        }
        Ok(generation)
    }

    /// Wait until the screen shows an idle pattern or settles.
    async fn wait_ready(&self, sessions: &dyn SessionBackend, id: &SessionId, m: &HarnessManifest) {
        let start = Instant::now();
        let mut last = Vec::new();
        let mut still = Instant::now();
        while start.elapsed() < self.config.ready_timeout {
            if let Ok(snap) = sessions.snapshot(id).await {
                let screen = String::from_utf8_lossy(&snap.bytes);
                if m.turn_signals
                    .idle_patterns
                    .iter()
                    .any(|p| screen.contains(p.as_str()))
                {
                    return;
                }
                if snap.bytes != last {
                    last = snap.bytes;
                    still = Instant::now();
                } else if !last.is_empty() && still.elapsed() >= self.config.ready_quiet {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// End the current generation's session, if it has one running.
    async fn stop_current(&self, conn: &Connection, s: &SubCoordinator) -> Result<()> {
        let Some(g) = s.current.as_ref().filter(|g| g.exited.is_none()) else {
            return Ok(());
        };
        let state = session_state(conn.sessions.as_ref(), &g.session).await?;
        if state != SessionState::Missing {
            conn.sessions.kill(&g.session).await?;
        }
        self.record(
            s.id(),
            SubEvent::Exited {
                generation: g.id.clone(),
                code: state.code(),
                stopped: true,
            },
        )
        .await
    }

    /// Kill sessions of this sub-coordinator no current generation owns,
    /// such as one a crash started but never recorded.
    async fn kill_orphans(&self, conn: &Connection, s: &SubCoordinator) -> Result<()> {
        let current = s
            .current
            .as_ref()
            .filter(|g| g.exited.is_none())
            .map(|g| g.session.clone());
        for info in conn.sessions.list().await? {
            if owns_session(s.id(), &info.name) && Some(&info.id) != current.as_ref() {
                tracing::info!(id = %s.id(), session = %info.id.0, "ending an orphaned session");
                conn.sessions.kill(&info.id).await?;
            }
        }
        Ok(())
    }

    async fn gate(&self, conn: &Connection, profile: &Profile) -> Result<()> {
        let m = self.manifest(&profile.harness)?;
        let r = readiness::check(conn.exec.as_ref(), &m, conn.tmux).await?;
        if r.ready() {
            Ok(())
        } else {
            Err(CoreError::Refused(format!(
                "the host is not ready: {}",
                r.summary()
            )))
        }
    }

    fn manifest(&self, harness: &str) -> Result<Arc<HarnessManifest>> {
        let m = self
            .manifests
            .resolve(harness)
            .ok_or_else(|| CoreError::NotFound(format!("harness {harness}")))?;
        if !m.roles.is_empty() && !m.roles.iter().any(|r| r == "coordinator") {
            return Err(CoreError::Refused(format!(
                "harness {} cannot run a coordinator",
                m.id
            )));
        }
        Ok(m.clone())
    }

    fn live(&self, id: &ProjectId) -> Result<SubCoordinator> {
        match self.registry.get(id) {
            Some(s) if s.is_live() => Ok(s),
            Some(_) => Err(CoreError::Refused(format!(
                "sub-coordinator {id} is retired"
            ))),
            None => Err(CoreError::NotFound(format!("sub-coordinator {id}"))),
        }
    }

    fn seeded(&self, id: &ProjectId) -> Result<SubCoordinator> {
        let s = self.live(id)?;
        if !s.seeded {
            return Err(CoreError::Refused(format!(
                "sub-coordinator {id} has no home yet; seed it first"
            )));
        }
        Ok(s)
    }

    async fn record(&self, id: &ProjectId, e: SubEvent) -> Result<()> {
        let event = NewEvent::typed(self.config.host.clone(), id.clone(), None, e.kind(), &e)?;
        self.log.append(event).await?;
        self.refresh().await
    }

    async fn refresh(&self) -> Result<()> {
        replay(self.log.as_ref(), &self.registry, BATCH).await?;
        Ok(())
    }

    async fn lock(&self, id: &ProjectId) -> tokio::sync::OwnedMutexGuard<()> {
        let m = self
            .locks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(id.clone())
            .or_default()
            .clone();
        m.lock_owned().await
    }

    /// The last applied position, for tests and diagnostics.
    pub async fn applied_through(&self) -> Result<Seq> {
        use quark_core::ReadModel as _;
        self.registry.applied_through().await
    }
}

/// The profile the next generation runs on: the current one's, which a
/// relaunch may have changed, else the registered one.
fn current_profile(s: &SubCoordinator) -> Profile {
    s.current
        .as_ref()
        .map(|g| g.profile.clone())
        .unwrap_or_else(|| s.registration.profile.clone())
}

fn check_profile(m: &HarnessManifest, p: &Profile) -> Result<()> {
    if let Some(model) = &p.model {
        if m.models.selection == "listed" && !m.models.known.iter().any(|k| k == model) {
            return Err(CoreError::Invalid(format!(
                "harness {} does not list model {model}",
                m.id
            )));
        }
    }
    if let Some(effort) = &p.effort {
        if !m.efforts.iter().any(|e| e == effort) {
            return Err(CoreError::Invalid(format!(
                "harness {} has no effort {effort}",
                m.id
            )));
        }
    }
    Ok(())
}

/// What became of a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionState {
    Missing,
    Running,
    Exited(Option<i32>),
}

impl SessionState {
    fn code(self) -> Option<i32> {
        match self {
            SessionState::Exited(c) => c,
            _ => None,
        }
    }
}

async fn session_state(sessions: &dyn SessionBackend, id: &SessionId) -> Result<SessionState> {
    Ok(
        match sessions.list().await?.into_iter().find(|i| &i.id == id) {
            None => SessionState::Missing,
            Some(i) if i.alive => SessionState::Running,
            Some(i) => SessionState::Exited(i.exit_code),
        },
    )
}

/// Whether `name` is a session of sub-coordinator `id` (and not of one
/// whose id merely starts the same way).
fn owns_session(id: &ProjectId, name: &str) -> bool {
    name.strip_prefix("sub-")
        .and_then(|r| r.strip_prefix(id.as_str()))
        .and_then(|r| r.strip_prefix('-'))
        .is_some_and(|g| g.len() == 8 && g.chars().all(|c| c.is_ascii_hexdigit()))
}

fn session_name(id: &ProjectId, generation: &str) -> String {
    format!("sub-{id}-{}", &generation[..8.min(generation.len())])
}

/// `text` as a bracketed paste followed by Enter.
fn paste(text: &str) -> Vec<u8> {
    let clean = text.replace("\x1b[201~", "");
    let mut out = b"\x1b[200~".to_vec();
    out.extend_from_slice(clean.trim_end().as_bytes());
    out.extend_from_slice(b"\x1b[201~");
    out.push(b'\r');
    out
}

/// Inherited file names are plain relative paths.
fn check_relative(name: &str) -> Result<()> {
    let p = Path::new(name);
    let ok = !name.is_empty()
        && p.components().all(|c| matches!(c, Component::Normal(_)))
        && !name.contains('\0');
    if ok {
        Ok(())
    } else {
        Err(CoreError::Invalid(format!("inherited file name {name:?}")))
    }
}

struct Fnv(u64);

impl std::hash::Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= u64::from(*b);
            self.0 = self.0.wrapping_mul(0x100_0000_01b3);
        }
    }
}

fn fnv() -> Fnv {
    Fnv(0xcbf2_9ce4_8422_2325)
}

/// A stable digest of the inherited material, to skip unchanged pushes.
fn digest(material: &BTreeMap<String, Vec<u8>>) -> String {
    let mut h = fnv();
    for (name, bytes) in material {
        h.write(name.as_bytes());
        h.write(&[0]);
        h.write(&(bytes.len() as u64).to_le_bytes());
        h.write(bytes);
    }
    format!("{:016x}", h.finish())
}

/// A stable id for the channel line at `offset`.
fn line_id(id: &ProjectId, offset: u64, line: &[u8]) -> EventId {
    let mut a = fnv();
    "subcoordinator.report".hash(&mut a);
    id.as_str().hash(&mut a);
    offset.hash(&mut a);
    let mut b = fnv();
    line.hash(&mut b);
    offset.hash(&mut b);
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&a.finish().to_be_bytes());
    bytes[8..].copy_from_slice(&b.finish().to_be_bytes());
    EventId(Uuid::new_v8(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherited_names_stay_inside() {
        assert!(check_relative("captain.md").is_ok());
        assert!(check_relative("dispatch/profiles.json").is_ok());
        for bad in ["", "/etc/passwd", "../x", "a/../../x", "./a"] {
            assert!(check_relative(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn digests_and_line_ids_are_stable() {
        let mut m = BTreeMap::new();
        m.insert("a".to_string(), b"1".to_vec());
        let d = digest(&m);
        assert_eq!(d, digest(&m.clone()));
        m.insert("a".to_string(), b"2".to_vec());
        assert_ne!(d, digest(&m));
        let id = ProjectId::from("web");
        assert_eq!(line_id(&id, 4, b"x"), line_id(&id, 4, b"x"));
        assert_ne!(line_id(&id, 4, b"x"), line_id(&id, 5, b"x"));
    }

    #[test]
    fn session_names_are_tmux_safe_and_owned() {
        let name = session_name(&ProjectId::from("web-2"), "0123456789ab");
        assert_eq!(name, "sub-web-2-01234567");
        assert!(owns_session(&ProjectId::from("web-2"), &name));
        assert!(!owns_session(&ProjectId::from("web"), &name));
        assert!(!owns_session(&ProjectId::from("web-2"), "sub-web-2-0123"));
    }
}
