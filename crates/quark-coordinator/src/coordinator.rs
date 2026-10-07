//! The wake queue: judgment items batched into turns, every step an event.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use quark_core::{CoreError, Event, EventId, EventLog, HostId, NewEvent, ProjectId, Result, Seq};
use quark_triggers::away::AwayState;
use time::{Duration, OffsetDateTime};
use tokio::sync::Mutex;

use crate::baseline::{self, BaselineTurn};
use crate::events::{CoordinatorEvent, TurnEnd, Usage, PREFIX};
use crate::prompt::{self, LayeredPrompt};
use crate::tools::{Hands, ToolCall};
use crate::wake::{judgment, WakeItem};

/// Whether the coordinator acts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Beside firstmate: record what would wake the coordinator and read
    /// firstmate's transcript for the baseline. Nobody is woken and no tool
    /// runs.
    Shadow,
    /// Wake the coordinator and run its tools.
    Native,
}

/// Tunables.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// A turn with no reported end after this long is closed as timed out
    /// and its items wake the coordinator again.
    pub turn_timeout: Duration,
    /// Most items in one turn; the rest wait for the next.
    pub max_items: usize,
    /// Record a cursor at least every this many evaluated events, so a
    /// restart does not re-read the whole log.
    pub cursor_every: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            turn_timeout: Duration::minutes(30),
            max_items: 20,
            cursor_every: 1000,
        }
    }
}

/// One turn handed to the coordinator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub id: String,
    pub items: Vec<WakeItem>,
    /// 1 on the first delivery; a restart or a timeout delivers again.
    pub attempt: u32,
}

impl Turn {
    /// What the coordinator is told, in neutral words (its persona is in its
    /// prompt).
    pub fn briefing(&self) -> String {
        let mut out = format!(
            "Wake {}: {} item{} need{} your judgment.",
            self.id,
            self.items.len(),
            if self.items.len() == 1 { "" } else { "s" },
            if self.items.len() == 1 { "s" } else { "" },
        );
        if self.attempt > 1 {
            out.push_str(&format!(
                " This wake was delivered before (attempt {}); skip what you already handled.",
                self.attempt
            ));
        }
        out.push_str(" Act through your tools and end your turn when nothing more needs you.\n");
        for i in &self.items {
            out.push_str(&format!("- [{}] {}\n", i.need.as_str(), i.summary));
        }
        out
    }
}

/// Delivers a turn to the coordinator LLM. quarkd implements it by typing
/// the briefing into the coordinator's session.
#[async_trait]
pub trait Brain: Send + Sync {
    async fn wake(&self, project: &ProjectId, turn: &Turn) -> Result<()>;
}

/// A brain that refuses, for shadow mode.
pub struct NoBrain;

#[async_trait]
impl Brain for NoBrain {
    async fn wake(&self, _: &ProjectId, _: &Turn) -> Result<()> {
        Err(CoreError::Unsupported("shadow mode does not act".into()))
    }
}

/// What one [`Coordinator::tick`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TickReport {
    pub evaluated: usize,
    /// New items that need judgment.
    pub items: usize,
    /// Turns opened (native) or that would have been (shadow).
    pub turns: usize,
    /// Turns delivered again after a restart or a timeout.
    pub redelivered: usize,
}

#[derive(Debug, Clone)]
struct OpenTurn {
    id: String,
    items: Vec<WakeItem>,
    attempt: u32,
    at: OffsetDateTime,
    /// Delivered by an earlier process; deliver again.
    stale: bool,
}

#[derive(Default)]
struct ProjectState {
    pending: Vec<WakeItem>,
    open: Option<OpenTurn>,
    turns: u64,
    prompt_digest: Option<String>,
}

#[derive(Default)]
struct State {
    applied: Seq,
    evaluated: Seq,
    cursor: Seq,
    away: AwayState,
    projects: BTreeMap<ProjectId, ProjectState>,
    /// Sources already in a turn (or a shadow batch), by Project.
    covered: BTreeSet<(ProjectId, Seq)>,
    /// Tool calls recorded without an outcome.
    tools_open: BTreeMap<String, ProjectId>,
    transcripts: BTreeMap<String, u64>,
}

impl State {
    fn apply(&mut self, e: &Event) {
        self.applied = e.seq;
        self.away.apply(e);
        if e.kind.prefix() != PREFIX {
            return;
        }
        let Ok(ev) = e.decode::<CoordinatorEvent>() else {
            tracing::warn!(
                seq = e.seq.0,
                kind = e.kind.as_str(),
                "unreadable coordinator event"
            );
            return;
        };
        let p = self.projects.entry(e.project.clone()).or_default();
        match ev {
            CoordinatorEvent::Prompt { prompt } => p.prompt_digest = Some(prompt.digest),
            CoordinatorEvent::Woken {
                turn,
                items,
                attempt,
            } => {
                if p.open.as_ref().is_none_or(|o| o.id != turn) {
                    p.turns += 1;
                }
                for i in &items {
                    self.covered.insert((e.project.clone(), i.source));
                }
                p.pending
                    .retain(|x| !items.iter().any(|i| i.source == x.source));
                p.open = Some(OpenTurn {
                    id: turn,
                    items,
                    attempt,
                    at: e.ts,
                    stale: false,
                });
            }
            CoordinatorEvent::WouldWake { items } => {
                p.turns += 1;
                for i in &items {
                    self.covered.insert((e.project.clone(), i.source));
                }
                p.pending
                    .retain(|x| !items.iter().any(|i| i.source == x.source));
            }
            CoordinatorEvent::TurnEnded { turn, end, .. } => {
                if let (Some(open), Some(turn)) = (&p.open, turn) {
                    if open.id == turn {
                        let open = p.open.take().unwrap();
                        if end == TurnEnd::TimedOut {
                            // Its items wake the coordinator again.
                            let mut again = open.items;
                            again.append(&mut p.pending);
                            p.pending = again;
                        }
                    }
                }
            }
            CoordinatorEvent::Tool { id, .. } => {
                self.tools_open.insert(id, e.project.clone());
            }
            CoordinatorEvent::ToolDone { id, .. } => {
                self.tools_open.remove(&id);
            }
            CoordinatorEvent::Cursor { through } => {
                self.cursor = through;
                self.covered.retain(|(_, s)| *s > through);
            }
            CoordinatorEvent::TranscriptRead { path, offset } => {
                self.transcripts.insert(path, offset);
            }
            CoordinatorEvent::Requested { .. } | CoordinatorEvent::Baseline { .. } => {}
        }
    }
}

/// Events evaluation skips: the coordinator's own records and other
/// engines' cursors.
fn is_bookkeeping(e: &Event) -> bool {
    let k = e.kind.as_str();
    (e.kind.prefix() == PREFIX && k != "coordinator.requested")
        || k.ends_with(".cursor")
        || matches!(k, "away.shadowed" | "shadow.divergence")
}

/// The judgment-only coordinator for every Project in one log.
pub struct Coordinator {
    log: Arc<dyn EventLog>,
    host: HostId,
    mode: Mode,
    config: Config,
    brain: Arc<dyn Brain>,
    hands: Arc<dyn Hands>,
    state: Mutex<State>,
    prompts: std::sync::Mutex<BTreeMap<ProjectId, LayeredPrompt>>,
}

const BATCH: usize = 512;

impl Coordinator {
    /// Replay the log and recover. A tool call recorded with no outcome (a
    /// crash while it ran) is closed as unknown, never run again; a turn
    /// still open is delivered again on the next tick. On a log never
    /// evaluated, evaluation starts at its head, so history wakes no one.
    pub async fn open(
        log: Arc<dyn EventLog>,
        host: HostId,
        mode: Mode,
        config: Config,
        brain: Arc<dyn Brain>,
        hands: Arc<dyn Hands>,
    ) -> Result<Self> {
        let c = Self {
            log,
            host,
            mode,
            config,
            brain,
            hands,
            state: Mutex::new(State::default()),
            prompts: Default::default(),
        };
        let mut st = c.state.lock().await;
        c.refresh_locked(&mut st).await?;
        if st.cursor == Seq::ZERO {
            let through = st.applied;
            c.append(
                &ProjectId::engine(),
                &CoordinatorEvent::Cursor { through },
                None,
            )
            .await?;
            c.refresh_locked(&mut st).await?;
        }
        st.evaluated = st.cursor;
        let open: Vec<(String, ProjectId)> = st
            .tools_open
            .iter()
            .map(|(id, p)| (id.clone(), p.clone()))
            .collect();
        for (id, project) in open {
            let ev = CoordinatorEvent::ToolDone {
                id: id.clone(),
                ok: false,
                detail:
                    "the engine stopped while this tool ran; check its effect before repeating it"
                        .into(),
            };
            c.append(&project, &ev, Some(crate::stable_id(&["tool_done", &id])))
                .await?;
        }
        c.refresh_locked(&mut st).await?;
        for p in st.projects.values_mut() {
            if let Some(o) = p.open.as_mut() {
                o.stale = true;
            }
        }
        drop(st);
        Ok(c)
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    async fn append(
        &self,
        project: &ProjectId,
        ev: &CoordinatorEvent,
        id: Option<EventId>,
    ) -> Result<Seq> {
        self.append_for(project, None, ev, id).await
    }

    async fn append_for(
        &self,
        project: &ProjectId,
        task: Option<quark_core::TaskId>,
        ev: &CoordinatorEvent,
        id: Option<EventId>,
    ) -> Result<Seq> {
        let mut e = NewEvent::typed(self.host.clone(), project.clone(), task, ev.kind(), ev)?;
        if let Some(id) = id {
            e.id = id;
        }
        self.log.append(e).await
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

    // ------------------------------------------------------------ prompt

    /// Use `prompt` for `project` (built by [`LayeredPrompt::build`]),
    /// recording it when it differs from the last one recorded. Returns
    /// whether it changed.
    pub async fn set_prompt(
        &self,
        project: &ProjectId,
        prompt: LayeredPrompt,
        persona: &str,
    ) -> Result<bool> {
        let record = prompt.record(persona);
        self.prompts.lock().unwrap().insert(project.clone(), prompt);
        let mut st = self.state.lock().await;
        self.refresh_locked(&mut st).await?;
        let known = st
            .projects
            .get(project)
            .and_then(|p| p.prompt_digest.as_deref());
        if known == Some(record.digest.as_str()) {
            return Ok(false);
        }
        let ev = CoordinatorEvent::Prompt { prompt: record };
        self.append(project, &ev, None).await?;
        self.refresh_locked(&mut st).await?;
        Ok(true)
    }

    /// The prompt `project`'s coordinator runs on, rendered.
    pub fn prompt(&self, project: &ProjectId) -> Option<String> {
        self.prompts
            .lock()
            .unwrap()
            .get(project)
            .map(|p| p.render())
    }

    // ------------------------------------------------------------- wakes

    /// Ask for a wake outside the log's own events (a rule's `wake`
    /// action, the API).
    pub async fn request(
        &self,
        project: &ProjectId,
        task: Option<quark_core::TaskId>,
        note: &str,
        by: &str,
    ) -> Result<Seq> {
        if note.trim().is_empty() {
            return Err(CoreError::Invalid("an empty wake note".into()));
        }
        let ev = CoordinatorEvent::Requested {
            note: note.to_string(),
            by: by.to_string(),
        };
        self.append_for(project, task, &ev, None).await
    }

    /// Evaluate new events, then open turns (native) or record what would
    /// have been woken (shadow). Call it on a timer.
    pub async fn tick(&self, now: OffsetDateTime) -> Result<TickReport> {
        let mut st = self.state.lock().await;
        self.refresh_locked(&mut st).await?;
        let mut report = TickReport::default();
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
                report.evaluated += 1;
                let Some(item) = judgment(e, &st.away) else {
                    continue;
                };
                if st.covered.contains(&(e.project.clone(), item.source)) {
                    continue;
                }
                let p = st.projects.entry(e.project.clone()).or_default();
                if p.pending.iter().any(|x| x.source == item.source) {
                    continue;
                }
                // A question asked again replaces the waiting one.
                p.pending.retain(|x| x.key != item.key);
                p.pending.push(item);
                report.items += 1;
            }
        }
        st.evaluated = through;

        let projects: Vec<ProjectId> = st.projects.keys().cloned().collect();
        let mut wrote = false;
        for project in projects {
            wrote |= self.turns(&mut st, &project, now, &mut report).await?;
        }

        // The cursor never passes an item still waiting for a turn.
        let waiting = st
            .projects
            .values()
            .flat_map(|p| p.pending.iter().map(|i| i.source))
            .min();
        let safe = match waiting {
            Some(s) => Seq(s.0.saturating_sub(1)).min(through),
            None => through,
        };
        if safe > st.cursor && (wrote || safe.0 - st.cursor.0 >= self.config.cursor_every) {
            self.append(
                &ProjectId::engine(),
                &CoordinatorEvent::Cursor { through: safe },
                None,
            )
            .await?;
            self.refresh_locked(&mut st).await?;
        }
        Ok(report)
    }

    /// Open or redeliver `project`'s turn. Returns whether it appended.
    async fn turns(
        &self,
        st: &mut State,
        project: &ProjectId,
        now: OffsetDateTime,
        report: &mut TickReport,
    ) -> Result<bool> {
        let max = self.config.max_items.max(1);
        let p = st.projects.get(project).expect("listed project");
        if self.mode == Mode::Shadow {
            if p.pending.is_empty() {
                return Ok(false);
            }
            let items: Vec<WakeItem> = p.pending.iter().take(max).cloned().collect();
            let id = crate::stable_id(&[
                "would_wake",
                project.as_str(),
                &items[0].source.0.to_string(),
                &items[items.len() - 1].source.0.to_string(),
            ]);
            self.append(project, &CoordinatorEvent::WouldWake { items }, Some(id))
                .await?;
            self.refresh_locked(st).await?;
            report.turns += 1;
            return Ok(true);
        }

        let mut wrote = false;
        if let Some(open) = p.open.clone() {
            if now - open.at >= self.config.turn_timeout {
                let ev = CoordinatorEvent::TurnEnded {
                    turn: Some(open.id.clone()),
                    end: TurnEnd::TimedOut,
                    usage: Usage::default(),
                };
                let id = crate::stable_id(&[
                    "timeout",
                    project.as_str(),
                    &open.id,
                    &open.attempt.to_string(),
                ]);
                self.append(project, &ev, Some(id)).await?;
                self.refresh_locked(st).await?;
                wrote = true;
            } else if open.stale {
                let turn = Turn {
                    id: open.id.clone(),
                    items: open.items.clone(),
                    attempt: open.attempt + 1,
                };
                self.woken(st, project, &turn).await?;
                report.redelivered += 1;
                return Ok(true);
            } else {
                return Ok(false);
            }
        }

        let p = st.projects.get(project).expect("listed project");
        if p.open.is_some() || p.pending.is_empty() {
            return Ok(wrote);
        }
        let turn = Turn {
            id: format!("{}#{}", project, p.turns + 1),
            items: p.pending.iter().take(max).cloned().collect(),
            attempt: 1,
        };
        self.woken(st, project, &turn).await?;
        report.turns += 1;
        Ok(true)
    }

    /// Record `turn` as delivered, then deliver it. A failed delivery
    /// leaves the turn open; the timeout delivers it again.
    async fn woken(&self, st: &mut State, project: &ProjectId, turn: &Turn) -> Result<()> {
        let ev = CoordinatorEvent::Woken {
            turn: turn.id.clone(),
            items: turn.items.clone(),
            attempt: turn.attempt,
        };
        let id = crate::stable_id(&[
            "woken",
            project.as_str(),
            &turn.id,
            &turn.attempt.to_string(),
        ]);
        self.append(project, &ev, Some(id)).await?;
        self.refresh_locked(st).await?;
        if let Err(e) = self.brain.wake(project, turn).await {
            tracing::warn!(%project, turn = %turn.id, error = %e, "could not wake the coordinator");
        }
        Ok(())
    }

    /// The coordinator finished a turn (from its harness's turn-end hook).
    /// Closes the open turn, if any; a turn with none open was the user
    /// talking to it directly. `source` dedupes a hook delivered twice.
    pub async fn turn_ended(
        &self,
        project: &ProjectId,
        usage: Usage,
        source: Option<&str>,
    ) -> Result<Option<String>> {
        let mut st = self.state.lock().await;
        self.refresh_locked(&mut st).await?;
        let turn = st
            .projects
            .get(project)
            .and_then(|p| p.open.as_ref())
            .map(|o| o.id.clone());
        let ev = CoordinatorEvent::TurnEnded {
            turn: turn.clone(),
            end: TurnEnd::Finished,
            usage,
        };
        let id = source.map(|s| crate::stable_id(&["turn_ended", project.as_str(), s]));
        self.append(project, &ev, id).await?;
        self.refresh_locked(&mut st).await?;
        Ok(turn)
    }

    /// The turn `project`'s coordinator is in, if any.
    pub async fn open_turn(&self, project: &ProjectId) -> Option<Turn> {
        let st = self.state.lock().await;
        let o = st.projects.get(project)?.open.as_ref()?;
        Some(Turn {
            id: o.id.clone(),
            items: o.items.clone(),
            attempt: o.attempt,
        })
    }

    /// Items waiting for a turn.
    pub async fn pending(&self, project: &ProjectId) -> Vec<WakeItem> {
        let st = self.state.lock().await;
        st.projects
            .get(project)
            .map(|p| p.pending.clone())
            .unwrap_or_default()
    }

    // ------------------------------------------------------------- tools

    /// Run one tool call for `project`'s coordinator: recorded first, then
    /// run, then its outcome recorded. Shadow mode refuses every call.
    pub async fn call(&self, project: &ProjectId, call: ToolCall) -> Result<String> {
        if self.mode == Mode::Shadow {
            return Err(CoreError::Unsupported(
                "the native coordinator is in shadow mode".into(),
            ));
        }
        let turn = self.open_turn(project).await.map(|t| t.id);
        let id = EventId::new().to_string();
        let ev = CoordinatorEvent::Tool {
            id: id.clone(),
            turn,
            call: call.clone(),
        };
        self.append(project, &ev, None).await?;
        let result = match &call {
            ToolCall::LoadSkill { name } => self.load_skill(project, name),
            _ => self.hands.run(project, &call).await,
        };
        let (ok, detail) = match &result {
            Ok(text) => (true, summary(text)),
            Err(e) => (false, e.to_string()),
        };
        self.append(
            project,
            &CoordinatorEvent::ToolDone { id, ok, detail },
            None,
        )
        .await?;
        result
    }

    fn load_skill(&self, project: &ProjectId, name: &str) -> Result<String> {
        let prompts = self.prompts.lock().unwrap();
        let skill = prompts
            .get(project)
            .and_then(|p| p.skill(name))
            .ok_or_else(|| CoreError::NotFound(format!("no skill {name}")))?;
        prompt::skill_body(skill).map_err(|e| CoreError::Backend(format!("skill {name}: {e}")))
    }

    // ---------------------------------------------------------- baseline

    /// Shadow: read firstmate's coordinator transcript at `path` for
    /// `project` from where the last read stopped, recording each finished
    /// turn once. Returns the turns recorded.
    pub async fn read_baseline(&self, project: &ProjectId, path: &Path) -> Result<usize> {
        let key = path.display().to_string();
        let offset = {
            let mut st = self.state.lock().await;
            self.refresh_locked(&mut st).await?;
            st.transcripts.get(&key).copied().unwrap_or(0)
        };
        let p: PathBuf = path.to_path_buf();
        let (turns, next) = tokio::task::spawn_blocking(move || baseline::read_turns(&p, offset))
            .await
            .map_err(|e| CoreError::Backend(e.to_string()))?
            .map_err(|e| CoreError::Backend(format!("{key}: {e}")))?;
        let n = turns.len();
        for turn in turns {
            self.record_baseline(project, &key, turn).await?;
        }
        if next != offset {
            let ev = CoordinatorEvent::TranscriptRead {
                path: key,
                offset: next,
            };
            self.append(project, &ev, None).await?;
            let mut st = self.state.lock().await;
            self.refresh_locked(&mut st).await?;
        }
        Ok(n)
    }

    async fn record_baseline(
        &self,
        project: &ProjectId,
        path: &str,
        turn: BaselineTurn,
    ) -> Result<()> {
        let id = crate::stable_id(&["baseline", project.as_str(), path, &turn.id]);
        self.append(project, &CoordinatorEvent::Baseline { turn }, Some(id))
            .await?;
        Ok(())
    }
}

/// The first line of a tool's output, for the log.
fn summary(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.len() <= 200 {
        return line.to_string();
    }
    let mut end = 200;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &line[..end])
}
