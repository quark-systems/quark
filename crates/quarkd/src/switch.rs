//! The switch driver: moves each open [`CoordinatorSwitch`] through its
//! steps, so a Project coordinator changes harness, model, effort or pool
//! without losing what its conversation holds. The protocol is in
//! `docs/engine/coordinator-switch.md`.
//!
//! Every step is recorded (see [`crate::store`]'s switches) before its
//! effect, and every step is safe to repeat, so a pass after a daemon crash
//! picks each switch up where it stopped. While a switch is open,
//! [`crate::recovery`] leaves its Project alone.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use quark_systems::{
    AgentConfig, CoordinatorSwitch, DecisionState, Project, SwitchKind, SwitchState, TaskState,
};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::accounts::{Accounts, Holder};
use crate::chat::{engine_note, CoordinatorInput, Delivery};
use crate::engine::native_project::{STATE_DIR, WINDOW_PREFIX};
use crate::engine::{EngineAdapter, WorkspaceRef};
use crate::sessions::Sessions;
use crate::store::{Store, SwitchRow};

/// How often the driver looks at open switches.
pub const INTERVAL: Duration = Duration::from_secs(2);
/// The longest a switch waits for the old coordinator's wrap up.
pub const WRAP_UP_TIMEOUT: Duration = Duration::from_secs(300);
/// How long after the wrap up's turn ends its reply is read, so the
/// transcript ingest has recorded it.
pub const SETTLE: Duration = Duration::from_secs(10);
/// How long the driver tries to type the note into the new session.
pub const BRIEF_TIMEOUT: Duration = Duration::from_secs(120);
/// The handoff file, inside [`STATE_DIR`]; the coordinator's prompt
/// includes it.
pub const HANDOFF_FILE: &str = "handoff.md";

/// What the engine types into the old coordinator before a handoff.
pub const WRAP_UP_NOTE: &str = "Your Project is moving to another coordinator agent, which \
     cannot read this conversation. First save what only this conversation holds: memory \
     with `remember`, open work as Beads issues (`bd create`), questions for the user with \
     `ask_user`. Then reply with a handoff brief and nothing else: what you were doing, what \
     you promised the user, what is waiting on whom, and what you would do next. Start it \
     with \"Handoff brief:\". Do not start new work.";

pub struct Switches {
    store: Arc<Store>,
    engine: Arc<dyn EngineAdapter>,
    accounts: Arc<Accounts>,
    sessions: Sessions,
    chat: Arc<dyn CoordinatorInput>,
    command: PathBuf,
    /// Quark's home, where each Project's Beads database lives.
    home: PathBuf,
    wrap_up: Duration,
    settle: Duration,
}

impl Switches {
    pub fn new(
        store: Arc<Store>,
        engine: Arc<dyn EngineAdapter>,
        accounts: Arc<Accounts>,
        sessions: Sessions,
        chat: Arc<dyn CoordinatorInput>,
        command: PathBuf,
        home: PathBuf,
    ) -> Self {
        Self {
            store,
            engine,
            accounts,
            sessions,
            chat,
            command,
            home,
            wrap_up: WRAP_UP_TIMEOUT,
            settle: SETTLE,
        }
    }

    /// Wait `wrap_up` at most for a wrap up, and `settle` after it.
    pub fn with_timeouts(mut self, wrap_up: Duration, settle: Duration) -> Self {
        (self.wrap_up, self.settle) = (wrap_up, settle);
        self
    }

    pub async fn run(self, interval: Duration) {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            self.pass().await;
        }
    }

    /// Take every open switch as far as it can go now.
    pub async fn pass(&self) {
        let open = match self.blocking(|s| s.open_switches()).await {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!(error = %e, "could not read open coordinator switches");
                return;
            }
        };
        for row in open {
            let id = row.switch.id.clone();
            let mut row = row;
            // Each step either records the next one or waits.
            for _ in 0..6 {
                let state = row.switch.state;
                match self.step(&row).await {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(e) => {
                        tracing::warn!(switch = %id, ?state, error = %e, "coordinator switch step failed");
                        break;
                    }
                }
                let i = id.clone();
                match self.blocking(move |s| s.switch_row(&i)).await {
                    Ok(r) if r.switch.state.open() => row = r,
                    _ => break,
                }
            }
        }
    }

    /// Take `row`'s next step. `Ok(false)` when it waits for something.
    async fn step(&self, row: &SwitchRow) -> Result<bool, String> {
        let s = &row.switch;
        let project = {
            let pid = s.project_id.clone();
            self.blocking(move |st| st.get_project(&pid)).await?
        };
        let ws = workspace(&project)?;
        match s.state {
            SwitchState::Requested => {
                if s.kind == SwitchKind::Handoff && self.window(&s.project_id).await?.is_some() {
                    self.set(&s.id, SwitchState::WrappingUp).await?;
                    let typed = self.chat.send(&ws, &engine_note(WRAP_UP_NOTE, "")).await;
                    match typed {
                        // Turn ends before the note was submitted are not its.
                        Ok(_) => {
                            let id = s.id.clone();
                            self.blocking(move |st| st.switch_noted(&id)).await?;
                        }
                        Err(e) => {
                            tracing::warn!(switch = %s.id, error = %e, "could not ask the coordinator to wrap up; the switch goes on after the timeout")
                        }
                    }
                } else {
                    self.commit(s, None).await?;
                }
                Ok(true)
            }
            SwitchState::WrappingUp => {
                let now = OffsetDateTime::now_utc();
                let since = |t: &str| {
                    OffsetDateTime::parse(t, &Rfc3339)
                        .map(|t| (now - t).try_into().unwrap_or(Duration::ZERO))
                        .unwrap_or(Duration::MAX)
                };
                let wrapped = row
                    .wrapped_at
                    .as_deref()
                    .is_some_and(|t| since(t) >= self.settle);
                let timed_out = since(&s.updated_at) >= self.wrap_up;
                if !wrapped && !timed_out {
                    return Ok(false);
                }
                let (pid, seq) = (s.project_id.clone(), row.after_seq);
                let brief = self
                    .blocking(move |st| st.coordinator_replies_after(&pid, seq))
                    .await?;
                if timed_out && !wrapped {
                    tracing::warn!(switch = %s.id, "the coordinator did not wrap up in time; switching anyway");
                }
                self.commit(s, pick_brief(&brief)).await?;
                Ok(true)
            }
            SwitchState::Switching => {
                if s.kind == SwitchKind::Handoff {
                    self.write_handoff(s, &ws).await?;
                }
                self.close(&s.project_id).await?;
                self.set(&s.id, SwitchState::Starting).await?;
                Ok(true)
            }
            SwitchState::Starting => {
                if self.window(&s.project_id).await?.is_none() {
                    if let Err(e) = self.start(s, &project, &ws).await {
                        let (id, why) = (s.id.clone(), e.clone());
                        self.blocking(move |st| st.fail_switch(&id, &why)).await?;
                        // Recovery starts the old agent again, resumed.
                        tracing::warn!(switch = %s.id, error = %e, "the new coordinator did not start; the old agent is back");
                        return Ok(false);
                    }
                }
                self.set(&s.id, SwitchState::Briefing).await?;
                Ok(true)
            }
            SwitchState::Briefing => {
                let typed = self.chat.send(&ws, &engine_note(&brief_note(s), "")).await;
                let gave_up = OffsetDateTime::parse(&s.updated_at, &Rfc3339)
                    .map(|t| OffsetDateTime::now_utc() - t >= BRIEF_TIMEOUT)
                    .unwrap_or(true);
                match typed {
                    Ok(Delivery::Confirmed) => {}
                    // Typed: not typed again, whatever the log says.
                    Ok(Delivery::Unconfirmed) => {}
                    Err(e) if gave_up => {
                        tracing::warn!(switch = %s.id, error = %e, "could not tell the new coordinator about the switch")
                    }
                    // The new session may not take input yet.
                    Err(_) => return Ok(false),
                }
                self.set(&s.id, SwitchState::Done).await?;
                tracing::info!(switch = %s.id, project = %s.project_id, "coordinator switched agents");
                Ok(true)
            }
            SwitchState::Done | SwitchState::Failed => Ok(false),
        }
    }

    async fn commit(&self, s: &CoordinatorSwitch, brief: Option<String>) -> Result<(), String> {
        let id = s.id.clone();
        self.blocking(move |st| st.commit_switch(&id, brief.as_deref()))
            .await
            .map(|_| ())
    }

    async fn set(&self, id: &str, state: SwitchState) -> Result<(), String> {
        let id = id.to_string();
        self.blocking(move |st| st.set_switch_state(&id, state))
            .await
    }

    /// Start the coordinator with the switch's new agent: resumed for a
    /// `resume` switch, falling back to fresh with a handoff file.
    async fn start(
        &self,
        s: &CoordinatorSwitch,
        project: &Project,
        ws: &WorkspaceRef,
    ) -> Result<(), String> {
        let agent = &s.to;
        self.sessions
            .ensure_server()
            .await
            .map_err(|e| e.to_string())?;
        let lease = self
            .accounts
            .lease(&Holder::Coordinator(project.id.clone()), agent)
            .await
            .map_err(|e| format!("choosing an account: {e}"))?;
        let mut env = lease.map(|l| l.env).unwrap_or_default();
        env.push(crate::beads::agent_env(&self.home, &project.id));
        if s.kind == SwitchKind::Resume {
            match self
                .engine
                .start_coordinator(&self.command, ws, agent, &env, true)
                .await
            {
                Ok(()) => return Ok(()),
                Err(e) => {
                    tracing::warn!(switch = %s.id, error = %e, "the coordinator could not resume with its new agent; starting it fresh");
                    self.write_handoff(s, ws).await?;
                }
            }
        }
        self.engine
            .start_coordinator(&self.command, ws, agent, &env, false)
            .await
            .map_err(|e| e.to_string())
    }

    /// Write `.quark/handoff.md`: the brief and the engine's own facts.
    async fn write_handoff(&self, s: &CoordinatorSwitch, ws: &WorkspaceRef) -> Result<(), String> {
        let pid = s.project_id.clone();
        let (decisions, tasks) = self
            .blocking(move |st| {
                let d = st.list_project_decisions(Some(&pid), Some(DecisionState::Open))?;
                let t = st.list_tasks(&pid)?;
                Ok((d, t))
            })
            .await?;
        let decisions: Vec<String> = decisions
            .iter()
            .map(|d| format!("- #{} {}", d.number, one_line(&d.question)))
            .collect();
        let tasks: Vec<String> = tasks
            .iter()
            .filter(|t| !matches!(t.state, TaskState::Done | TaskState::Failed))
            .map(|t| format!("- {} {} ({:?})", t.id, one_line(&t.title), t.state).to_lowercase())
            .collect();
        let text = handoff_text(s, &now(), &decisions, &tasks);
        let path = ws.root.join(STATE_DIR).join(HANDOFF_FILE);
        tokio::task::spawn_blocking(move || write_file(&path, &text))
            .await
            .map_err(|e| e.to_string())?
    }

    /// The coordinator's window target, when the server has it.
    async fn window(&self, project_id: &str) -> Result<Option<String>, String> {
        let Ok(server) = self.sessions.server() else {
            return Ok(None);
        };
        if !server.is_running().await {
            return Ok(None);
        }
        let recorded = self
            .engine
            .coordinator_terminals(&self.command)
            .await
            .map_err(|e| e.to_string())?
            .remove(project_id);
        let window = format!("{WINDOW_PREFIX}{project_id}");
        let panes = server.list_panes().await.map_err(|e| e.to_string())?;
        Ok(panes.into_iter().map(|p| p.target).find(|t| {
            Some(t) == recorded.as_ref() || t.split_once(':').is_some_and(|(_, w)| w == window)
        }))
    }

    async fn close(&self, project_id: &str) -> Result<(), String> {
        close_coordinator(
            &self.sessions,
            self.engine.as_ref(),
            &self.command,
            project_id,
        )
        .await
    }

    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Store) -> crate::store::Result<T> + Send + 'static,
    ) -> Result<T, String> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || f(&store))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())
    }
}

/// Close a Project coordinator's window: the one the engine recorded, and
/// one named for the Project the native engine would adopt. Its
/// conversation is kept, so it can be resumed.
pub async fn close_coordinator(
    sessions: &Sessions,
    engine: &dyn EngineAdapter,
    command: &Path,
    project_id: &str,
) -> Result<(), String> {
    let Ok(server) = sessions.server() else {
        return Ok(());
    };
    if !server.is_running().await {
        return Ok(());
    }
    let recorded = engine
        .coordinator_terminals(command)
        .await
        .map_err(|e| e.to_string())?
        .remove(project_id);
    let window = format!("{WINDOW_PREFIX}{project_id}");
    let panes = server.list_panes().await.map_err(|e| e.to_string())?;
    let mut targets: Vec<String> = panes
        .into_iter()
        .map(|p| p.target)
        .filter(|t| {
            Some(t) == recorded.as_ref() || t.split_once(':').is_some_and(|(_, w)| w == window)
        })
        .collect();
    targets.dedup();
    for target in targets {
        server
            .run(&["kill-window", "-t", &target])
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The handoff file's text for the coordinator a switch starts.
pub fn handoff_text(
    s: &CoordinatorSwitch,
    at: &str,
    decisions: &[String],
    tasks: &[String],
) -> String {
    let from = s
        .from
        .as_ref()
        .map(describe)
        .unwrap_or_else(|| "none".into());
    let mut out = format!(
        "# Handoff\n\nWritten {at}, when this Project's coordinator changed from {from} to {}. \
         You are the new coordinator, and you start without the old one's conversation. \
         Where this file and `fleet`, decisions or Beads differ, trust them: they are live, \
         and this file is not.\n\n## Brief from the previous coordinator\n\n",
        describe(&s.to)
    );
    match s.brief.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        Some(b) => out.push_str(b),
        None => out.push_str("The previous coordinator left no brief. Read `fleet`, the open decisions and Beads (`bd ready`) to pick up the work."),
    }
    out.push_str("\n\n## Open decisions\n\n");
    out.push_str(&list_or_none(decisions));
    out.push_str("\n\n## Work not finished\n\n");
    out.push_str(&list_or_none(tasks));
    out.push('\n');
    out
}

/// The note typed into the new coordinator when it is up.
pub fn brief_note(s: &CoordinatorSwitch) -> String {
    let from = s
        .from
        .as_ref()
        .map(describe)
        .unwrap_or_else(|| "none".into());
    let to = describe(&s.to);
    match s.kind {
        SwitchKind::Resume => format!(
            "The user changed your agent from {from} to {to}. This is the same conversation; \
             carry on where you left off."
        ),
        SwitchKind::Handoff => format!(
            "The user changed this Project's coordinator from {from} to {to}, and you are the \
             new one. Read the handoff in your instructions, check `fleet` and `bd ready`, \
             then carry on with the work. Tell the user in one line that you took over."
        ),
    }
}

/// `claude-code · opus · high`, with the harness's defaults left out.
pub fn describe(a: &AgentConfig) -> String {
    let mut parts = vec![a.harness.clone()];
    parts.extend(a.model.clone());
    parts.extend(a.effort.clone());
    if let Some(p) = &a.pool {
        parts.push(format!("pool {p}"));
    }
    parts.join(" · ")
}

/// The brief among the coordinator's replies since the switch was asked
/// for, newest first: the newest that is marked as one, else the newest.
pub fn pick_brief(replies: &[String]) -> Option<String> {
    replies
        .iter()
        .find(|r| r.contains("Handoff brief"))
        .or_else(|| replies.first())
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
}

fn list_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "None.".into()
    } else {
        items.join("\n")
    }
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

fn workspace(project: &Project) -> Result<WorkspaceRef, String> {
    let root = project
        .workspace_path
        .as_deref()
        .ok_or("the Project has no workspace")?;
    Ok(WorkspaceRef {
        project_id: project.id.clone(),
        root: PathBuf::from(root),
    })
}

fn write_file(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(harness: &str, model: Option<&str>) -> AgentConfig {
        AgentConfig {
            harness: harness.into(),
            model: model.map(Into::into),
            effort: None,
            pool: None,
        }
    }

    fn switch(kind: SwitchKind, brief: Option<&str>) -> CoordinatorSwitch {
        CoordinatorSwitch {
            id: "sw_1".into(),
            project_id: "prj_1".into(),
            from: Some(agent("claude-code", Some("sonnet"))),
            to: agent("codex", None),
            kind,
            state: SwitchState::Switching,
            brief: brief.map(Into::into),
            error: None,
            requested_at: "t".into(),
            updated_at: "t".into(),
        }
    }

    #[test]
    fn the_handoff_file_has_the_brief_and_the_engine_facts() {
        let s = switch(
            SwitchKind::Handoff,
            Some("Handoff brief: fixing the login bug."),
        );
        let text = handoff_text(&s, "2026-10-24T10:00:00Z", &["- #3 Ship it?".into()], &[]);
        assert!(
            text.contains("from claude-code · sonnet to codex"),
            "{text}"
        );
        assert!(text.contains("Handoff brief: fixing the login bug."));
        assert!(text.contains("- #3 Ship it?"));
        assert!(text.contains("## Work not finished\n\nNone."));
        assert!(text.contains("trust them"));
        let none = handoff_text(&switch(SwitchKind::Handoff, None), "t", &[], &[]);
        assert!(none.contains("left no brief"));
    }

    #[test]
    fn the_brief_is_the_marked_reply_or_else_the_last() {
        assert_eq!(pick_brief(&[]), None);
        let replies = vec![
            "Done.".to_string(),
            "Handoff brief: x".into(),
            "older".into(),
        ];
        assert_eq!(pick_brief(&replies).as_deref(), Some("Handoff brief: x"));
        assert_eq!(
            pick_brief(&["last".into(), "first".into()]).as_deref(),
            Some("last")
        );
    }

    #[test]
    fn the_note_says_what_changed() {
        let resume = brief_note(&switch(SwitchKind::Resume, None));
        assert!(resume.contains("same conversation"));
        let handoff = brief_note(&switch(SwitchKind::Handoff, None));
        assert!(handoff.contains("Read the handoff"));
    }
}
