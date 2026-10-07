//! The slice 7 engine: channels, triggers and the away policy over one
//! event log.
//!
//! [`Engine`] keeps its read models ([`Inboxes`], [`AwayState`],
//! [`RuleSet`]) folded from the log and evaluates each new event once,
//! after the last `trigger.cursor`. What it does with an event depends on
//! its [`Mode`]:
//!
//! - **Shadow**: firstmate still runs the Project. The engine mirrors the
//!   bash inbox and posture into the log ([`Engine::mirror_firstmate`]),
//!   compares its route for every `firstmate.status` line with firstmate's
//!   away classifier and records each disagreement as a
//!   `shadow.divergence` event, and records `trigger.would_fire` instead of
//!   running actions. It never wakes, notifies or steers anyone.
//! - **Native**: the engine routes occasions through [`Effects`], fires
//!   rules (claim, act, outcome) and sends digests and the return brief.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use quark_core::event::kinds;
use quark_core::slice::Divergence;
use quark_core::worker::{WorkerEnvelope, WorkerMessage};
use quark_core::{
    CoreError, Event, EventId, EventLog, HostId, NewEvent, ProjectId, Result, Seq, Slice, TaskId,
};
use serde::Serialize;
use time::OffsetDateTime;
use tokio::sync::Mutex;

use crate::away::{AwayEvent, AwayPolicy, AwayState, Item, Occasion, Posture, Reach, Route};
use crate::channel::{self, ChannelEvent, Inbound, Inboxes};
use crate::commands::{Commands, Ran};
use crate::firstmate;
use crate::trigger::{
    self, event_matches, Action, Condition, Defined, Expect, Rule, RuleId, RuleSet, Status,
    TriggerEvent,
};

/// How the engine acts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Record what native would do; act on nothing.
    Shadow,
    Native,
}

/// Who the engine records as the actor of its own changes.
pub const ENGINE: &str = "engine";

/// What native mode does in the world. quarkd implements it.
#[async_trait]
pub trait Effects: Send + Sync {
    /// Wake the coordinator of `project` for judgment.
    async fn wake(&self, project: &ProjectId, task: Option<&TaskId>, note: &str) -> Result<()>;
    /// Tell the user now.
    async fn notify(&self, project: &ProjectId, task: Option<&TaskId>, note: &str) -> Result<()>;
    /// Send the user a digest. `returning` marks the brief for a user who
    /// just came back, which also carries what was held.
    async fn digest(&self, project: &ProjectId, items: &[Item], returning: bool) -> Result<()>;
    /// Send a steering message to a task's worker.
    async fn steer(&self, project: &ProjectId, task: &TaskId, text: &str) -> Result<()>;
}

/// Effects that refuse everything, for shadow mode.
pub struct NoEffects;

#[async_trait]
impl Effects for NoEffects {
    async fn wake(&self, _: &ProjectId, _: Option<&TaskId>, _: &str) -> Result<()> {
        Err(CoreError::Unsupported("shadow mode does not act".into()))
    }
    async fn notify(&self, _: &ProjectId, _: Option<&TaskId>, _: &str) -> Result<()> {
        Err(CoreError::Unsupported("shadow mode does not act".into()))
    }
    async fn digest(&self, _: &ProjectId, _: &[Item], _: bool) -> Result<()> {
        Err(CoreError::Unsupported("shadow mode does not act".into()))
    }
    async fn steer(&self, _: &ProjectId, _: &TaskId, _: &str) -> Result<()> {
        Err(CoreError::Unsupported("shadow mode does not act".into()))
    }
}

/// What one [`Engine::tick`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TickReport {
    /// Events evaluated.
    pub evaluated: usize,
    /// Status lines whose route was compared with firstmate's (shadow).
    pub compared: usize,
    /// Of those, how many disagreed.
    pub diverged: usize,
    /// Rules fired (native) or that would have fired (shadow).
    pub fired: usize,
    /// Occasions routed to someone (native).
    pub routed: usize,
    /// Digests sent (native).
    pub digests: usize,
}

/// What one [`Engine::mirror_firstmate`] pass appended.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MirrorReport {
    pub received: usize,
    pub acked: usize,
    pub posture: bool,
}

#[derive(Debug, Default)]
struct Poll {
    next: Option<OffsetDateTime>,
    streak: u32,
    errors: u32,
}

#[derive(Default)]
struct State {
    applied: Seq,
    /// Events up to here have been evaluated.
    evaluated: Seq,
    inboxes: Inboxes,
    away: AwayState,
    rules: RuleSet,
    polls: HashMap<(ProjectId, RuleId), Poll>,
}

impl State {
    fn apply(&mut self, e: &Event) {
        self.inboxes.apply(e);
        self.away.apply(e);
        self.rules.apply(e);
        self.applied = e.seq;
    }
}

/// Channels, triggers and the away policy for every Project in one log.
pub struct Engine {
    log: Arc<dyn EventLog>,
    host: HostId,
    mode: Mode,
    effects: Arc<dyn Effects>,
    commands: Arc<dyn Commands>,
    state: Mutex<State>,
}

const BATCH: usize = 512;

impl Engine {
    /// Replay the log and recover: a fire claimed with no outcome (a crash
    /// mid-action) is closed as ambiguous, never re-run. On a log that has
    /// never been evaluated, evaluation starts at its head, so history does
    /// not fire rules.
    pub async fn open(
        log: Arc<dyn EventLog>,
        host: HostId,
        mode: Mode,
        effects: Arc<dyn Effects>,
        commands: Arc<dyn Commands>,
    ) -> Result<Self> {
        let engine = Self {
            log,
            host,
            mode,
            effects,
            commands,
            state: Mutex::new(State::default()),
        };
        let mut st = engine.state.lock().await;
        engine.refresh_locked(&mut st).await?;
        st.evaluated = if st.rules.cursor() == Seq::ZERO {
            st.applied
        } else {
            st.rules.cursor()
        };
        let open: Vec<_> = st.rules.open().cloned().collect();
        for (project, id, fire) in open {
            let ev = TriggerEvent::Outcome {
                id: id.clone(),
                fire: fire.clone(),
                status: Status::Ambiguous,
                detail: "the engine stopped while the action ran; it is not run again".into(),
            };
            engine
                .append_stable(
                    &project,
                    None,
                    &ev,
                    &["outcome", project.as_str(), id.as_str(), &fire],
                )
                .await?;
        }
        engine.refresh_locked(&mut st).await?;
        drop(st);
        Ok(engine)
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    // ----------------------------------------------------------- writes

    /// The one inbound API: record a message from any channel. Receiving
    /// the same (channel, id) again is a no-op.
    pub async fn receive(&self, project: &ProjectId, message: Inbound) -> Result<Seq> {
        if message.body.trim().is_empty() {
            return Err(CoreError::Invalid("an empty message".into()));
        }
        let task = message.task.clone();
        let ev = ChannelEvent::Received { message };
        let id = ev.event_id(project);
        let seq = self.append(project, task, &ev, ev.kind(), Some(id)).await?;
        self.refresh().await?;
        Ok(seq)
    }

    /// Mark a pending message handled.
    pub async fn ack(&self, project: &ProjectId, channel: &str, id: &str, by: &str) -> Result<()> {
        self.refresh().await?;
        {
            let st = self.state.lock().await;
            if !st.inboxes.is_pending(project, channel, id) {
                if st.inboxes.was_acked(project, channel, id) {
                    return Ok(());
                }
                return Err(CoreError::NotFound(format!("{channel} message {id}")));
            }
        }
        let ev = ChannelEvent::Acked {
            channel: channel.into(),
            id: id.into(),
            by: by.into(),
        };
        let eid = ev.event_id(project);
        self.append(project, None, &ev, ev.kind(), Some(eid))
            .await?;
        self.refresh().await?;
        Ok(())
    }

    /// Set the user's posture for `project` (the engine project: everywhere
    /// without its own).
    pub async fn set_posture(
        &self,
        project: &ProjectId,
        posture: Posture,
        words: Option<String>,
        expected_return: Option<OffsetDateTime>,
        by: &str,
    ) -> Result<()> {
        let ev = AwayEvent::Posture {
            posture,
            words,
            expected_return,
            by: by.into(),
        };
        self.append(project, None, &ev, ev.kind(), None).await?;
        self.refresh().await
    }

    pub async fn set_policy(
        &self,
        project: &ProjectId,
        policy: AwayPolicy,
        by: &str,
    ) -> Result<()> {
        policy.validate()?;
        let ev = AwayEvent::Policy {
            policy,
            by: by.into(),
        };
        self.append(project, None, &ev, ev.kind(), None).await?;
        self.refresh().await
    }

    /// Add or replace a rule.
    pub async fn define(&self, project: &ProjectId, rule: Rule, by: &str) -> Result<()> {
        rule.validate()?;
        let ev = TriggerEvent::Defined {
            rule,
            by: by.into(),
        };
        self.append(project, None, &ev, ev.kind(), None).await?;
        self.refresh().await
    }

    pub async fn remove(&self, project: &ProjectId, id: &RuleId, by: &str) -> Result<()> {
        self.refresh().await?;
        if self.state.lock().await.rules.rule(project, id).is_none() {
            return Err(CoreError::NotFound(format!("rule {id}")));
        }
        let ev = TriggerEvent::Removed {
            id: id.clone(),
            by: by.into(),
        };
        self.append(project, None, &ev, ev.kind(), None).await?;
        self.refresh().await
    }

    // ------------------------------------------------------------ reads

    pub async fn inbox(&self, project: &ProjectId) -> Vec<Inbound> {
        self.state.lock().await.inboxes.pending(project)
    }

    pub async fn rules(&self, project: &ProjectId) -> Vec<Defined> {
        self.state
            .lock()
            .await
            .rules
            .rules(project)
            .into_iter()
            .cloned()
            .collect()
    }

    pub async fn posture(&self, project: &ProjectId) -> Posture {
        self.state.lock().await.away.posture(project)
    }

    pub async fn policy(&self, project: &ProjectId) -> AwayPolicy {
        self.state.lock().await.away.policy(project)
    }

    pub async fn route(&self, project: &ProjectId, occasion: Occasion) -> Route {
        self.state.lock().await.away.route(project, occasion)
    }

    pub async fn digest(&self, project: &ProjectId) -> Vec<Item> {
        self.state.lock().await.away.digest(project).to_vec()
    }

    pub async fn held(&self, project: &ProjectId) -> Vec<Item> {
        self.state.lock().await.away.held(project).to_vec()
    }

    // ------------------------------------------------------- the passes

    /// Fold new events into the read models without evaluating them.
    pub async fn refresh(&self) -> Result<()> {
        let mut st = self.state.lock().await;
        self.refresh_locked(&mut st).await
    }

    async fn refresh_locked(&self, st: &mut State) -> Result<()> {
        loop {
            let events = self.log.read(st.applied, BATCH).await?;
            if events.is_empty() {
                return Ok(());
            }
            for e in &events {
                st.apply(e);
            }
        }
    }

    /// Mirror the firstmate home of `project` into the log: new inbox
    /// notes, notes firstmate handled, and its away posture. Safe to run
    /// again at any time.
    pub async fn mirror_firstmate(&self, project: &ProjectId, home: &Path) -> Result<MirrorReport> {
        let path = home.to_path_buf();
        let home = tokio::task::spawn_blocking(move || firstmate::read_home(&path))
            .await
            .map_err(|e| CoreError::Backend(e.to_string()))??;
        self.refresh().await?;
        let mut report = MirrorReport::default();
        let (new, to_ack, own_posture) = {
            let st = self.state.lock().await;
            let known = |m: &Inbound| {
                st.inboxes.is_pending(project, &m.channel, &m.id)
                    || st.inboxes.was_acked(project, &m.channel, &m.id)
            };
            let new: Vec<Inbound> = home
                .pending
                .iter()
                .chain(home.handled.iter())
                .filter(|m| !known(m))
                .cloned()
                .collect();
            let to_ack: Vec<Inbound> = home
                .handled
                .iter()
                .filter(|m| !st.inboxes.was_acked(project, &m.channel, &m.id))
                .cloned()
                .collect();
            (new, to_ack, st.away.own_posture(project))
        };
        for m in new {
            let ev = ChannelEvent::Received { message: m };
            let id = ev.event_id(project);
            self.append(project, None, &ev, ev.kind(), Some(id)).await?;
            report.received += 1;
        }
        for m in to_ack {
            let ev = ChannelEvent::Acked {
                channel: m.channel,
                id: m.id,
                by: "firstmate".into(),
            };
            let id = ev.event_id(project);
            self.append(project, None, &ev, ev.kind(), Some(id)).await?;
            report.acked += 1;
        }
        if own_posture.unwrap_or_default() != home.posture {
            let ev = AwayEvent::Posture {
                posture: home.posture,
                words: None,
                expected_return: home.expected_return,
                by: "firstmate".into(),
            };
            self.append(project, None, &ev, ev.kind(), None).await?;
            report.posture = true;
        }
        self.refresh().await?;
        Ok(report)
    }

    /// Evaluate every new event once, then clocks and polled commands,
    /// then digests. Call it on a timer.
    pub async fn tick(&self, now: OffsetDateTime) -> Result<TickReport> {
        let mut st = self.state.lock().await;
        self.refresh_locked(&mut st).await?;
        let mut report = TickReport::default();
        let mut bookkeeping_only = true;
        let mut through = st.evaluated;
        while through < st.applied {
            let events = self.log.read(through, BATCH).await?;
            if events.is_empty() {
                break;
            }
            for e in &events {
                if e.seq > st.applied {
                    break;
                }
                through = e.seq;
                if is_bookkeeping(e) {
                    continue;
                }
                bookkeeping_only = false;
                report.evaluated += 1;
                self.evaluate(&mut st, e, &mut report).await?;
            }
        }
        if self.mode == Mode::Shadow && report.compared > 0 {
            let ev = AwayEvent::Shadowed {
                through,
                compared: report.compared,
                diverged: report.diverged,
            };
            self.append(&ProjectId::engine(), None, &ev, ev.kind(), None)
                .await?;
        }
        self.clocks(&mut st, now, &mut report).await?;
        if self.mode == Mode::Native {
            self.digests(&mut st, now, &mut report).await?;
        }
        if !bookkeeping_only {
            let ev = TriggerEvent::Cursor { through };
            self.append(&ProjectId::engine(), None, &ev, ev.kind(), None)
                .await?;
        }
        st.evaluated = through;
        self.refresh_locked(&mut st).await?;
        Ok(report)
    }

    async fn evaluate(&self, st: &mut State, e: &Event, report: &mut TickReport) -> Result<()> {
        // Event rules, as defined when the event was appended.
        let matching: Vec<(ProjectId, Defined)> = st
            .rules
            .live()
            .into_iter()
            .filter(|(p, d)| *p == e.project && d.since < e.seq && event_matches(&d.rule.when, e))
            .collect();
        for (project, d) in matching {
            let fire = d.fire_id(&format!("e{}", e.seq.0));
            if st.rules.has_fired(&project, &d.rule.id, &fire) {
                continue;
            }
            self.fire(st, &project, &d, fire, Some(e.seq), None, report)
                .await?;
        }

        let Some((occasion, summary)) = occasion(e) else {
            return Ok(());
        };
        match self.mode {
            Mode::Shadow => {
                if e.kind.as_str() != firstmate::STATUS_KIND {
                    return Ok(());
                }
                let Ok(line) = e.decode::<firstmate::StatusLine>() else {
                    return Ok(());
                };
                let route = st.away.route(&e.project, occasion);
                let bash = firstmate::bash_escalates(&line.verb, &line.raw);
                report.compared += 1;
                if route.wake != bash {
                    report.diverged += 1;
                    let d = Divergence {
                        slice: Slice::SubCoordinators,
                        operation: "away.route".into(),
                        bash: serde_json::json!({ "escalate": bash, "line": line.raw }),
                        native: serde_json::json!({
                            "wake": route.wake,
                            "occasion": occasion,
                            "posture": st.away.posture(&e.project).as_str(),
                        }),
                    };
                    self.append(
                        &e.project,
                        e.task.clone(),
                        &d,
                        kinds::SHADOW_DIVERGENCE.into(),
                        Some(crate::stable_id(&["divergence", &e.id.to_string()])),
                    )
                    .await?;
                }
            }
            Mode::Native => {
                let route = st.away.route(&e.project, occasion);
                self.dispatch(e, occasion, route, &summary, report).await?;
            }
        }
        Ok(())
    }

    async fn dispatch(
        &self,
        e: &Event,
        occasion: Occasion,
        route: Route,
        summary: &str,
        report: &mut TickReport,
    ) -> Result<()> {
        if route.wake {
            if let Err(err) = self
                .effects
                .wake(&e.project, e.task.as_ref(), summary)
                .await
            {
                tracing::warn!(project = %e.project, error = %err, "could not wake the coordinator");
            }
        }
        if route.user == Reach::Notify {
            if let Err(err) = self
                .effects
                .notify(&e.project, e.task.as_ref(), summary)
                .await
            {
                tracing::warn!(project = %e.project, error = %err, "could not notify the user");
            }
        }
        if route.wake || route.user != Reach::Silent {
            report.routed += 1;
        }
        if matches!(route.user, Reach::Digest | Reach::Hold | Reach::Notify) {
            let ev = AwayEvent::Routed {
                source: e.seq,
                occasion,
                route,
                summary: summary.to_string(),
            };
            self.append(
                &e.project,
                e.task.clone(),
                &ev,
                ev.kind(),
                Some(crate::stable_id(&["routed", &e.id.to_string()])),
            )
            .await?;
        }
        Ok(())
    }

    /// Claim, act, record. In shadow mode, only record what would fire.
    #[allow(clippy::too_many_arguments)]
    async fn fire(
        &self,
        st: &mut State,
        project: &ProjectId,
        d: &Defined,
        fire: String,
        cause: Option<Seq>,
        failure: Option<String>,
        report: &mut TickReport,
    ) -> Result<()> {
        let id = d.rule.id.clone();
        let key_id = id.0.clone();
        let key = ["fire", project.as_str(), key_id.as_str(), &fire];
        report.fired += 1;
        if self.mode == Mode::Shadow {
            let ev = TriggerEvent::WouldFire {
                id,
                fire: fire.clone(),
                cause,
            };
            self.append_stable(project, None, &ev, &key).await?;
            self.refresh_locked(st).await?;
            return Ok(());
        }
        let claim = TriggerEvent::Fired {
            id: id.clone(),
            fire: fire.clone(),
            cause,
        };
        self.append_stable(project, None, &claim, &key).await?;
        self.refresh_locked(st).await?;
        let (status, detail) = match failure {
            Some(why) => (Status::ConditionError, why),
            None => self.act(project, &d.rule, &fire).await,
        };
        let outcome = TriggerEvent::Outcome {
            id: id.clone(),
            fire: fire.clone(),
            status,
            detail,
        };
        self.append_stable(
            project,
            None,
            &outcome,
            &["outcome", project.as_str(), id.as_str(), &fire],
        )
        .await?;
        self.refresh_locked(st).await
    }

    async fn act(&self, project: &ProjectId, rule: &Rule, fire: &str) -> (Status, String) {
        let done = |r: Result<()>, ok: &str| match r {
            Ok(()) => (Status::Ok, ok.to_string()),
            Err(e) => (Status::ActionFailed, e.to_string()),
        };
        match &rule.then {
            Action::Wake { note } => done(
                self.effects.wake(project, None, note).await,
                "woke the coordinator",
            ),
            Action::Steer { task, text } => done(
                self.effects.steer(project, task, text).await,
                "sent the message",
            ),
            Action::Inbox { body } => {
                let message = Inbound {
                    id: format!("trigger-{}-{fire}", rule.id),
                    channel: channel::INBOX.into(),
                    from: format!("trigger:{}", rule.id),
                    body: body.clone(),
                    at: OffsetDateTime::now_utc(),
                    task: None,
                };
                let ev = ChannelEvent::Received { message };
                let eid = ev.event_id(project);
                done(
                    self.append(project, None, &ev, ev.kind(), Some(eid))
                        .await
                        .map(|_| ()),
                    "left a note in the inbox",
                )
            }
            Action::Command { argv, timeout_secs } => {
                let ran = self
                    .commands
                    .run(argv, Duration::from_secs(*timeout_secs))
                    .await;
                let detail = describe(&ran);
                if ran.code == Some(0) {
                    (Status::Ok, detail)
                } else {
                    (Status::ActionFailed, detail)
                }
            }
        }
    }

    /// Clock and polled-command conditions.
    async fn clocks(
        &self,
        st: &mut State,
        now: OffsetDateTime,
        report: &mut TickReport,
    ) -> Result<()> {
        let live = st.rules.live();
        st.polls
            .retain(|k, _| live.iter().any(|(p, d)| (p, &d.rule.id) == (&k.0, &k.1)));
        for (project, d) in live {
            match &d.rule.when {
                Condition::Event { .. } => {}
                Condition::Every { secs } => {
                    let slot = now.unix_timestamp().div_euclid(*secs as i64);
                    let defined_slot = d.at.unix_timestamp().div_euclid(*secs as i64);
                    if slot <= defined_slot {
                        continue;
                    }
                    let fire = d.fire_id(&format!("t{slot}"));
                    if !st.rules.has_fired(&project, &d.rule.id, &fire) {
                        self.fire(st, &project, &d, fire, None, None, report)
                            .await?;
                    }
                }
                Condition::At { at } => {
                    let fire = d.fire_id("at");
                    if now >= *at && !st.rules.has_fired(&project, &d.rule.id, &fire) {
                        self.fire(st, &project, &d, fire, None, None, report)
                            .await?;
                    }
                }
                Condition::Command {
                    argv,
                    interval_secs,
                    stable,
                    timeout_secs,
                    expect,
                    error_budget,
                } => {
                    let key = (project.clone(), d.rule.id.clone());
                    let poll = st.polls.entry(key.clone()).or_default();
                    if poll.next.is_some_and(|n| now < n) {
                        continue;
                    }
                    poll.next = Some(now + time::Duration::seconds(*interval_secs as i64));
                    let ran = self
                        .commands
                        .run(argv, Duration::from_secs(*timeout_secs))
                        .await;
                    let poll = st.polls.get_mut(&key).expect("just inserted");
                    match (ran.code, ran.timed_out) {
                        (Some(0), false) if expect_holds(expect.as_ref(), &ran.stdout) => {
                            poll.streak += 1;
                            poll.errors = 0;
                        }
                        (Some(0) | Some(1), false) => {
                            poll.streak = 0;
                            poll.errors = 0;
                        }
                        _ => {
                            poll.streak = 0;
                            poll.errors += 1;
                        }
                    }
                    if poll.errors >= *error_budget {
                        let why = format!(
                            "the condition failed {} times in a row: {}",
                            poll.errors,
                            describe(&ran)
                        );
                        st.polls.remove(&key);
                        let fire = d.fire_id("error");
                        self.fire(st, &project, &d, fire, None, Some(why), report)
                            .await?;
                    } else if poll.streak >= *stable {
                        st.polls.remove(&key);
                        let fire = d.fire_id("held");
                        self.fire(st, &project, &d, fire, None, None, report)
                            .await?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Send what is due: the return brief for a user who came back, and
    /// digests on their cadence.
    async fn digests(
        &self,
        st: &mut State,
        now: OffsetDateTime,
        report: &mut TickReport,
    ) -> Result<()> {
        let projects: Vec<ProjectId> = st.away.projects().cloned().collect();
        for project in projects {
            let returning =
                st.away.posture(&project) == Posture::Present && !st.away.held(&project).is_empty();
            if !returning && !st.away.digest_due(&project, now) {
                continue;
            }
            let mut items: Vec<Item> = st.away.digest(&project).to_vec();
            if returning {
                items.extend(st.away.held(&project).iter().cloned());
            }
            items.sort_by_key(|i| i.seq);
            let Some(last) = items.last().map(|i| i.seq) else {
                continue;
            };
            self.effects.digest(&project, &items, returning).await?;
            let ev = AwayEvent::DigestSent {
                through: last,
                items: items.len(),
                returning,
            };
            self.append(&project, None, &ev, ev.kind(), None).await?;
            report.digests += 1;
        }
        self.refresh_locked(st).await
    }

    // ---------------------------------------------------------- helpers

    async fn append<T: Serialize>(
        &self,
        project: &ProjectId,
        task: Option<TaskId>,
        value: &T,
        kind: quark_core::EventKind,
        id: Option<EventId>,
    ) -> Result<Seq> {
        let mut ev = NewEvent::typed(self.host.clone(), project.clone(), task, kind, value)?;
        if let Some(id) = id {
            ev.id = id;
        }
        self.log.append(ev).await
    }

    async fn append_stable(
        &self,
        project: &ProjectId,
        task: Option<TaskId>,
        ev: &TriggerEvent,
        key: &[&str],
    ) -> Result<Seq> {
        self.append(project, task, ev, ev.kind(), Some(crate::stable_id(key)))
            .await
    }
}

/// The engine's own position records, which never trigger anything.
fn is_bookkeeping(e: &Event) -> bool {
    matches!(e.kind.as_str(), "trigger.cursor" | "away.shadowed")
}

fn expect_holds(expect: Option<&Expect>, stdout: &str) -> bool {
    match expect {
        None => true,
        Some(Expect::Equals(v)) => stdout.trim() == v,
        Some(Expect::Differs(v)) => stdout.trim() != v,
    }
}

fn describe(ran: &Ran) -> String {
    let code = match (ran.code, ran.timed_out) {
        (_, true) => "timed out".to_string(),
        (Some(c), _) => format!("exit {c}"),
        (None, _) => "did not run".to_string(),
    };
    let tail = if ran.stderr.trim().is_empty() {
        ran.stdout.trim()
    } else {
        ran.stderr.trim()
    };
    let tail: String = tail
        .chars()
        .rev()
        .take(300)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if tail.is_empty() {
        code
    } else {
        format!("{code}: {tail}")
    }
}

/// The occasion an event stands for, with a one-line summary.
pub fn occasion(e: &Event) -> Option<(Occasion, String)> {
    let task = e
        .task
        .as_ref()
        .map(|t| format!("{t}: "))
        .unwrap_or_default();
    match e.kind.as_str() {
        firstmate::STATUS_KIND => {
            let line = e.decode::<firstmate::StatusLine>().ok()?;
            let occ = firstmate::status_occasion(&line.verb, &line.raw);
            let text = if line.note.is_empty() {
                &line.raw
            } else {
                &line.note
            };
            Some((occ, format!("{task}{} {text}", line.verb)))
        }
        kinds::WORKER => {
            let env = e.decode::<WorkerEnvelope>().ok()?;
            match env.message {
                WorkerMessage::Report { state, note } => {
                    let occ = match state.as_str() {
                        "blocked" => Occasion::Blocked,
                        "failed" => Occasion::Failed,
                        "paused" => Occasion::Paused,
                        _ => Occasion::Progress,
                    };
                    Some((occ, format!("{task}{state} {note}")))
                }
                WorkerMessage::Ask { question, .. } => {
                    Some((Occasion::Decision, format!("{task}asks: {question}")))
                }
                WorkerMessage::Done {
                    summary,
                    pull_request,
                } => Some((
                    Occasion::Done,
                    match pull_request {
                        Some(pr) => format!("{task}done: {summary} ({pr})"),
                        None => format!("{task}done: {summary}"),
                    },
                )),
                WorkerMessage::Learned { .. } | WorkerMessage::Signal { .. } => None,
            }
        }
        "supervisor.stale" => Some((Occasion::Stale, format!("{task}no activity for a while"))),
        k if k == format!("{}.received", channel::PREFIX) => {
            let ChannelEvent::Received { message } = e.decode::<ChannelEvent>().ok()? else {
                return None;
            };
            Some((
                Occasion::Inbound,
                format!(
                    "{} from {}: {}",
                    message.channel,
                    message.from,
                    message.summary(120)
                ),
            ))
        }
        k if k == format!("{}.outcome", trigger::PREFIX) => {
            let TriggerEvent::Outcome {
                id, status, detail, ..
            } = e.decode::<TriggerEvent>().ok()?
            else {
                return None;
            };
            let occ = if status == Status::Ok {
                Occasion::TriggerFired
            } else {
                Occasion::TriggerFailed
            };
            Some((occ, format!("rule {id}: {detail}")))
        }
        _ => None,
    }
}
