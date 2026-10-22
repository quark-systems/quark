//! The native coordinator's Brain and Hands in quarkd (slice 6).
//!
//! - [`InputBrain`] delivers a turn by typing its briefing into the
//!   coordinator's session, the way the coordinator chat does.
//! - [`EngineHands`] carries out the coordinator's tool calls through the
//!   engine adapter and the store: steering, answering, relaunching and
//!   cancelling go to the engine; clearing a finished task goes through
//!   [`crate::clear`]; `ask_user`, `tell_user` and `remember`
//!   become a decision, a coordinator chat message and a memory proposal in
//!   the store. Starting a task and choosing its profile go to the engine's
//!   dispatch ([`EngineAdapter::start_task`], served by the native engine);
//!   acknowledging an inbound message needs the channels API, so it is
//!   refused until that slice runs.

use std::sync::Arc;

use async_trait::async_trait;
use quark_coordinator::tools::Agent;
use quark_coordinator::{Brain, Hands, ToolCall, Turn};
use quark_core::{CoreError, ProjectId, Result};
use quark_systems::{AgentConfig, MemoryEvidence, MemorySource};

use crate::chat::{ChatError, CoordinatorInput, Delivery};
use crate::clear::{ClearError, Cleared};
use crate::engine::{
    EngineAdapter, EngineError, EngineTask, NewTask, StartOutcome, TaskControl, TaskStart,
    WorkspaceRef,
};
use crate::store::{NewProposal, Store, StoreError};

/// Who the engine records as answering a worker for the coordinator.
pub const ANSWERED_BY: &str = "coordinator";
/// Most status lines the `task` tool shows.
const TASK_LINES: usize = 10;

/// The Project's workspace, or why there is none.
async fn workspace(store: &Arc<Store>, project: &ProjectId) -> Result<WorkspaceRef> {
    let id = project.as_str().to_string();
    let store = store.clone();
    let p = tokio::task::spawn_blocking(move || store.get_project(&id))
        .await
        .map_err(|e| CoreError::Backend(e.to_string()))?
        .map_err(|e| CoreError::NotFound(format!("Project {project}: {e}")))?;
    let root = p
        .workspace_path
        .ok_or_else(|| CoreError::Invalid(format!("Project {project} has no workspace")))?;
    Ok(WorkspaceRef {
        project_id: p.id,
        root: root.into(),
    })
}

fn core(e: EngineError) -> CoreError {
    match e {
        EngineError::Invalid(m) => CoreError::Invalid(m),
        EngineError::TaskNotFound(t) => CoreError::NotFound(format!("no task {t}")),
        EngineError::WorkspaceNotFound(p) => {
            CoreError::NotFound(format!("no workspace at {}", p.display()))
        }
        other => CoreError::Backend(other.to_string()),
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> crate::store::Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| CoreError::Backend(e.to_string()))?
        .map_err(|e| CoreError::Backend(e.to_string()))
}

/// Types each turn's briefing into the coordinator's session.
pub struct InputBrain {
    store: Arc<Store>,
    input: Arc<dyn CoordinatorInput>,
}

impl InputBrain {
    pub fn new(store: Arc<Store>, input: Arc<dyn CoordinatorInput>) -> Self {
        Self { store, input }
    }
}

#[async_trait]
impl Brain for InputBrain {
    async fn wake(&self, project: &ProjectId, turn: &Turn) -> Result<()> {
        let ws = workspace(&self.store, project).await?;
        match self.input.send(&ws, &turn.briefing()).await {
            Ok(Delivery::Confirmed) => Ok(()),
            // Typed and submitted; typing it again could deliver it twice.
            // The turn timeout redelivers it if the coordinator never saw it.
            Ok(Delivery::Unconfirmed) => {
                tracing::warn!(%project, turn = %turn.id, "wake typed but not confirmed");
                Ok(())
            }
            Err(ChatError::Unavailable(m)) => Err(CoreError::Refused(m)),
            Err(ChatError::Failed(m)) => Err(CoreError::Backend(m)),
        }
    }
}

/// Carries out the coordinator's calls through the engine and the store.
pub struct EngineHands {
    store: Arc<Store>,
    engine: Arc<dyn EngineAdapter>,
    channels: Option<Arc<quark_triggers::Engine>>,
}

impl EngineHands {
    pub fn new(store: Arc<Store>, engine: Arc<dyn EngineAdapter>) -> Self {
        Self {
            store,
            engine,
            channels: None,
        }
    }

    /// Acknowledge channel messages through `channels` (slice 7), when it
    /// runs.
    pub fn with_channels(mut self, channels: Option<Arc<quark_triggers::Engine>>) -> Self {
        self.channels = channels;
        self
    }

    /// The engine's tasks, without the ones cleared off the board.
    async fn tasks(&self, ws: &WorkspaceRef) -> Result<Vec<EngineTask>> {
        let mut tasks = self.engine.snapshot(ws).await.map_err(core)?.tasks;
        let (store, pid) = (self.store.clone(), ws.project_id.clone());
        let cleared = blocking(move || store.cleared_engine_ids(&pid)).await?;
        tasks.retain(|t| !cleared.contains(&t.id));
        Ok(tasks)
    }
}

fn task_line(t: &EngineTask) -> String {
    let mut line = format!("{} [{}] {}", t.id, t.state.as_str(), t.title);
    if let Some(note) = t.state_note.as_deref().filter(|n| !n.is_empty()) {
        line.push_str(&format!(": {note}"));
    }
    if let Some(pr) = &t.pull_request_url {
        line.push_str(&format!(" ({pr})"));
    }
    line
}

fn relaunch(agent: Option<&Agent>, note: &str) -> TaskControl {
    TaskControl::Relaunch {
        harness: agent.map(|a| a.harness.clone()),
        model: agent.and_then(|a| a.model.clone()),
        effort: agent.and_then(|a| a.effort.clone()),
        note: note.to_string(),
        account_env: Vec::new(),
    }
}

fn agent_config(a: &Agent) -> AgentConfig {
    AgentConfig {
        harness: a.harness.clone(),
        model: a.model.clone(),
        effort: a.effort.clone(),
        pool: None,
    }
}

/// What the coordinator is told about a task it started or picked an
/// agent for.
fn started(s: &TaskStart) -> String {
    let t = &s.task_id;
    match &s.outcome {
        StartOutcome::Started {
            harness,
            model,
            effort,
            account,
        } => {
            let mut agent = harness.clone();
            if let Some(m) = model {
                agent.push_str(&format!(" {m}"));
            }
            if let Some(e) = effort {
                agent.push_str(&format!(" at {e} effort"));
            }
            if let Some(a) = account {
                agent.push_str(&format!(" (account {a})"));
            }
            format!("Started {t} on {agent}.")
        }
        StartOutcome::Waiting { reason } => {
            format!("Queued {t}: {reason}. It starts when a host has room.")
        }
        StartOutcome::Escalated { reason } => format!(
            "Created {t}, but dispatch left its agent to you: {reason}. Pick with choose_profile for task {t}."
        ),
    }
}

fn not_yet(what: &str, slice: &str) -> CoreError {
    CoreError::Unsupported(format!("{what} needs {slice}, which is not native yet"))
}

#[async_trait]
impl Hands for EngineHands {
    async fn run(&self, project: &ProjectId, call: &ToolCall) -> Result<String> {
        let ws = workspace(&self.store, project).await?;
        match call {
            ToolCall::Fleet {} => {
                let tasks = self.tasks(&ws).await?;
                if tasks.is_empty() {
                    return Ok("No tasks.".into());
                }
                Ok(tasks.iter().map(task_line).collect::<Vec<_>>().join("\n"))
            }
            ToolCall::Task { task } => {
                let tasks = self.tasks(&ws).await?;
                let t = tasks
                    .iter()
                    .find(|t| t.id == task.as_str())
                    .ok_or_else(|| CoreError::NotFound(format!("no task {task}")))?;
                let tail = self
                    .engine
                    .status_tail(&ws, task.as_str(), 0)
                    .await
                    .map_err(core)?;
                let mut out = task_line(t);
                let skip = tail.entries.len().saturating_sub(TASK_LINES);
                if tail.entries.len() > skip {
                    out.push_str("\nRecent messages:");
                    for e in &tail.entries[skip..] {
                        out.push_str(&format!("\n- {}", e.raw));
                    }
                }
                Ok(out)
            }
            ToolCall::Steer { task, text } => {
                self.engine
                    .send_message(&ws, task.as_str(), text)
                    .await
                    .map_err(core)?;
                Ok(format!("Sent to {task}."))
            }
            ToolCall::Answer { task, key, answer } => {
                let hold = format!("{task}:{key}");
                self.engine
                    .answer(&ws, &hold, answer, ANSWERED_BY)
                    .await
                    .map_err(core)?;
                Ok(format!("Answered {task}'s question {key}."))
            }
            ToolCall::Relaunch { task, note, agent } => {
                self.engine
                    .control(&ws, task.as_str(), &relaunch(agent.as_ref(), note))
                    .await
                    .map_err(core)?;
                Ok(format!("Relaunched {task}."))
            }
            ToolCall::Cancel { task, .. } => {
                self.engine
                    .control(&ws, task.as_str(), &TaskControl::Cancel)
                    .await
                    .map_err(core)?;
                Ok(format!("Cancelled {task}; its worktree is kept."))
            }
            ToolCall::Clear { task, .. } => {
                let (store, pid) = (self.store.clone(), ws.project_id.clone());
                let id = blocking(move || store.task_ids_by_engine(&pid))
                    .await?
                    .remove(task.as_str())
                    .ok_or_else(|| CoreError::NotFound(format!("no task {task}")))?;
                match crate::clear::clear(&self.store, &self.engine, &id).await {
                    Ok(Cleared::Cleared(_)) => Ok(format!(
                        "Cleared {task}: its session is stopped, its worktree given back and it is off the board."
                    )),
                    Ok(Cleared::Kept {
                        uncommitted,
                        unpushed_commits,
                        ..
                    }) => Err(CoreError::Refused(format!(
                        "Not cleared: {task}'s worktree has {}. Land that work or relaunch {task} to finish it.",
                        crate::clear::unlanded(uncommitted, unpushed_commits)
                    ))),
                    Err(ClearError::Store(StoreError::Conflict(m))) => Err(CoreError::Refused(m)),
                    Err(ClearError::Store(StoreError::NotFound)) => {
                        Err(CoreError::NotFound(format!("no task {task}")))
                    }
                    Err(ClearError::Engine(e)) => Err(core(e)),
                    Err(e) => Err(CoreError::Backend(e.to_string())),
                }
            }
            ToolCall::AskUser { key, question, .. } => {
                let brief = call.brief().unwrap_or_default();
                let (store, pid) = (self.store.clone(), ws.project_id.clone());
                let (key, question) = (key.clone(), question.clone());
                let d = blocking(move || store.ask_from_coordinator(&pid, &key, &question, brief))
                    .await?;
                Ok(format!(
                    "Asked the user (decision {}). Their answer wakes you; keep the rest of the work moving.",
                    d.number
                ))
            }
            ToolCall::TellUser { text } => {
                let (store, pid, text) = (self.store.clone(), ws.project_id.clone(), text.clone());
                blocking(move || store.coordinator_says(&pid, &text)).await?;
                Ok("Told the user.".into())
            }
            ToolCall::Remember { fact } => {
                crate::memory::validate_text(fact).map_err(CoreError::Invalid)?;
                let proposal = NewProposal {
                    task_event_id: None,
                    task_id: None,
                    text: fact.trim().to_string(),
                    evidence: MemoryEvidence::default(),
                    source: MemorySource::Coordinator,
                };
                let (store, pid) = (self.store.clone(), ws.project_id.clone());
                blocking(move || store.propose_memory(&pid, proposal)).await?;
                Ok("Proposed for Project memory; the user reviews it.".into())
            }
            ToolCall::StartTask {
                title,
                brief,
                repo,
                agent,
                rule,
            } => {
                let task = NewTask {
                    title: title.clone(),
                    brief: brief.clone(),
                    repo: repo.clone(),
                    agent: agent.as_ref().map(agent_config),
                    rule: rule.clone(),
                };
                let start = self.engine.start_task(&ws, &task).await.map_err(core)?;
                Ok(started(&start))
            }
            ToolCall::ChooseProfile { task, agent, rule } => {
                let start = match (agent, rule) {
                    (Some(agent), _) => {
                        self.engine
                            .choose_profile(&ws, task.as_str(), &agent_config(agent))
                            .await
                    }
                    (None, Some(rule)) => self.engine.choose_rule(&ws, task.as_str(), rule).await,
                    (None, None) => {
                        return Err(CoreError::Invalid("give `rule` or `agent`".into()))
                    }
                }
                .map_err(core)?;
                Ok(started(&start))
            }
            ToolCall::AckMessage { channel, id } => {
                let channels = self.channels.as_ref().ok_or_else(|| {
                    not_yet("acknowledging a message", "the channels API (slice 7)")
                })?;
                channels.ack(project, channel, id, ANSWERED_BY).await?;
                Ok(format!("Acknowledged {channel} message {id}."))
            }
            // The coordinator answers it from the Project's prompt.
            ToolCall::LoadSkill { .. } => Err(CoreError::Invalid(
                "load_skill is answered by the coordinator".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use quark_core::TaskId;
    use quark_systems::{CreateProject, DecisionState, TaskState};

    use super::*;
    use crate::engine::{FleetSnapshot, Hold, StatusEntry, StubEngine, StubWrite};

    fn setup() -> (Arc<Store>, Arc<StubEngine>, ProjectId) {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let p = store
            .create_project(CreateProject {
                name: "Quark".into(),
                goal: None,
                workspace_path: Some("/ws".into()),
                repos: vec![],
                agent_config: None,
                dispatch_preset: None,
                delivery: None,
            })
            .unwrap();
        let engine = Arc::new(StubEngine::new());
        (store, engine, ProjectId::new(p.id))
    }

    fn task(id: &str, state: TaskState) -> EngineTask {
        EngineTask {
            id: id.into(),
            title: format!("{id} title"),
            kind: None,
            state,
            state_note: Some("note".into()),
            state_source: None,
            harness: None,
            pull_request_url: None,
            terminal: None,
            worktree: None,
        }
    }

    #[tokio::test]
    async fn reads_steers_answers_and_controls_go_to_the_engine() {
        let (store, engine, p) = setup();
        engine.set_snapshot(FleetSnapshot {
            tasks: vec![task("t1", TaskState::NeedsDecision)],
        });
        engine.push_status(
            "t1",
            StatusEntry {
                kind: "needs-decision".into(),
                decision_key: Some("schema".into()),
                note: "v14 or v15?".into(),
                raw: "needs-decision [key=schema]: v14 or v15?".into(),
            },
        );
        engine.set_holds(vec![Hold {
            id: "t1:schema".into(),
            task_id: Some("t1".into()),
            question: "v14 or v15?".into(),
            ..Default::default()
        }]);
        let hands = EngineHands::new(store, engine.clone());
        let t1 = TaskId::from("t1");

        let fleet = hands.run(&p, &ToolCall::Fleet {}).await.unwrap();
        assert_eq!(fleet, "t1 [needs_decision] t1 title: note");
        let one = hands
            .run(&p, &ToolCall::Task { task: t1.clone() })
            .await
            .unwrap();
        assert!(one.contains("needs-decision [key=schema]"), "{one}");
        assert!(hands
            .run(
                &p,
                &ToolCall::Task {
                    task: TaskId::from("nope")
                }
            )
            .await
            .is_err());

        hands
            .run(
                &p,
                &ToolCall::Steer {
                    task: t1.clone(),
                    text: "rebase first".into(),
                },
            )
            .await
            .unwrap();
        hands
            .run(
                &p,
                &ToolCall::Answer {
                    task: t1.clone(),
                    key: "schema".into(),
                    answer: "v15".into(),
                },
            )
            .await
            .unwrap();
        hands
            .run(
                &p,
                &ToolCall::Relaunch {
                    task: t1.clone(),
                    note: "pick up from the failing test".into(),
                    agent: Some(Agent {
                        harness: "codex".into(),
                        model: None,
                        effort: Some("high".into()),
                    }),
                },
            )
            .await
            .unwrap();
        hands
            .run(
                &p,
                &ToolCall::Cancel {
                    task: t1,
                    reason: "superseded".into(),
                },
            )
            .await
            .unwrap();
        let writes = engine.writes();
        assert_eq!(
            writes[0],
            StubWrite::Message {
                task_id: "t1".into(),
                text: "rebase first".into()
            }
        );
        assert_eq!(
            writes[1],
            StubWrite::Answer {
                hold_id: "t1:schema".into(),
                answer: "v15".into(),
                answered_by: ANSWERED_BY.into()
            }
        );
        assert!(matches!(
            &writes[2],
            StubWrite::Control { action: TaskControl::Relaunch { harness: Some(h), effort: Some(e), .. }, .. }
                if h == "codex" && e == "high"
        ));
        assert_eq!(
            writes[3],
            StubWrite::Control {
                task_id: "t1".into(),
                action: TaskControl::Cancel
            }
        );
    }

    #[tokio::test]
    async fn clear_tidies_a_finished_task_and_takes_it_out_of_the_fleet() {
        let (store, engine, p) = setup();
        let snapshot = FleetSnapshot {
            tasks: vec![
                task("t1", TaskState::Failed),
                task("t2", TaskState::Running),
            ],
        };
        store.apply_snapshot(p.as_str(), &snapshot).unwrap();
        engine.set_snapshot(snapshot);
        let hands = EngineHands::new(store.clone(), engine.clone());
        let clear = |t: &str| ToolCall::Clear {
            task: TaskId::from(t),
            reason: "done with it".into(),
        };

        let refused = hands.run(&p, &clear("t2")).await.unwrap_err();
        assert!(matches!(refused, CoreError::Refused(_)), "{refused:?}");
        engine.set_unlanded("t1", true);
        let kept = hands.run(&p, &clear("t1")).await.unwrap_err();
        assert!(kept.to_string().contains("uncommitted changes"), "{kept}");
        engine.set_unlanded("t1", false);
        let out = hands.run(&p, &clear("t1")).await.unwrap();
        assert!(out.starts_with("Cleared t1"), "{out}");
        assert!(matches!(
            hands.run(&p, &clear("nope")).await,
            Err(CoreError::NotFound(_))
        ));

        let fleet = hands.run(&p, &ToolCall::Fleet {}).await.unwrap();
        assert_eq!(fleet, "t2 [running] t2 title: note");
    }

    #[tokio::test]
    async fn ask_user_tell_user_and_remember_land_in_the_store() {
        let (store, engine, p) = setup();
        let hands = EngineHands::new(store.clone(), engine.clone());
        let ask = ToolCall::parse(
            "ask_user",
            &serde_json::json!({
                "key": "storage",
                "question": "SQLite or Postgres?",
                "options": ["SQLite", {"label": "Postgres", "consequence": "needs a server"}],
                "recommended": "SQLite",
                "recommended_why": "no server to run"
            }),
        )
        .unwrap();
        let out = hands.run(&p, &ask).await.unwrap();
        assert!(out.contains("decision 1"), "{out}");
        let open = store
            .list_project_decisions(Some(p.as_str()), Some(DecisionState::Open))
            .unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].question, "SQLite or Postgres?");
        assert_eq!(open[0].brief.asked_by.as_deref(), Some("coordinator"));
        assert_eq!(open[0].brief.recommended.as_deref(), Some("SQLite"));
        assert_eq!(open[0].brief.options.len(), 2);

        hands
            .run(
                &p,
                &ToolCall::TellUser {
                    text: "PR #12 merged.".into(),
                },
            )
            .await
            .unwrap();
        hands
            .run(
                &p,
                &ToolCall::Remember {
                    fact: "Run cargo nextest, not cargo test.".into(),
                },
            )
            .await
            .unwrap();
        let proposals = store.list_memory_proposals(p.as_str(), None).unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].source, MemorySource::Coordinator);
        assert!(engine.writes().is_empty());
    }

    #[tokio::test]
    async fn start_task_and_choose_profile_go_to_the_engines_dispatch() {
        let (store, engine, p) = setup();
        let hands = EngineHands::new(store, engine.clone());
        let start = ToolCall::StartTask {
            title: "Fix the parser".into(),
            brief: "It drops trailing commas.".into(),
            repo: Some("app".into()),
            agent: None,
            rule: None,
        };
        let out = hands.run(&p, &start).await.unwrap();
        assert!(
            out.starts_with("Created stub-1") && out.contains("choose_profile"),
            "{out}"
        );
        let choose = ToolCall::ChooseProfile {
            task: TaskId::from("stub-1"),
            agent: Some(Agent {
                harness: "codex".into(),
                model: Some("gpt-5.5".into()),
                effort: Some("high".into()),
            }),
            rule: None,
        };
        let out = hands.run(&p, &choose).await.unwrap();
        assert_eq!(out, "Started stub-1 on codex gpt-5.5 at high effort.");
        let writes = engine.writes();
        assert!(matches!(
            &writes[0],
            StubWrite::StartTask { task } if task.repo.as_deref() == Some("app") && task.agent.is_none()
        ));
        assert!(matches!(
            &writes[1],
            StubWrite::ChooseProfile { task_id, agent } if task_id == "stub-1" && agent.harness == "codex"
        ));

        // Or the dispatch rule the task fits.
        let by_rule = ToolCall::ChooseProfile {
            task: TaskId::from("stub-1"),
            agent: None,
            rule: Some("rule_1".into()),
        };
        let out = hands.run(&p, &by_rule).await.unwrap();
        assert_eq!(out, "Started stub-1 on stub-rule_1.");
        assert!(matches!(
            &engine.writes()[2],
            StubWrite::ChooseRule { task_id, rule } if task_id == "stub-1" && rule == "rule_1"
        ));

        // An engine without dispatch refuses.
        engine.fail_writes(Some("no"));
        assert!(hands.run(&p, &start).await.is_err());
    }

    #[tokio::test]
    async fn ack_message_marks_the_inbox_message_handled() {
        let (store, engine, p) = setup();
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(quark_eventlog::SqliteEventLog::open(dir.path().join("e.db")).unwrap());
        let channels = Arc::new(
            quark_triggers::Engine::open(
                log,
                quark_core::HostId::from("h"),
                Arc::new(quark_triggers::NoEffects),
                Arc::new(quark_triggers::Processes),
            )
            .await
            .unwrap(),
        );
        let note = quark_triggers::Inbound::note("user", "ship the docs too");
        let id = note.id.clone();
        channels.receive(&p, note).await.unwrap();
        let hands = EngineHands::new(store, engine).with_channels(Some(channels.clone()));
        let ack = ToolCall::AckMessage {
            channel: "inbox".into(),
            id: id.clone(),
        };
        hands.run(&p, &ack).await.unwrap();
        assert!(channels.inbox(&p).await.is_empty());
        // Again is a no-op; an unknown message is not found.
        hands.run(&p, &ack).await.unwrap();
        let unknown = ToolCall::AckMessage {
            channel: "inbox".into(),
            id: "nope".into(),
        };
        assert!(matches!(
            hands.run(&p, &unknown).await,
            Err(CoreError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn what_needs_later_slices_is_refused() {
        let (store, engine, p) = setup();
        let hands = EngineHands::new(store, engine);
        let ack = ToolCall::AckMessage {
            channel: "slack".into(),
            id: "m1".into(),
        };
        assert!(matches!(
            hands.run(&p, &ack).await,
            Err(CoreError::Unsupported(_))
        ));
    }

    struct Typed(Mutex<Vec<String>>);

    #[async_trait]
    impl CoordinatorInput for Typed {
        async fn send(
            &self,
            _: &WorkspaceRef,
            text: &str,
        ) -> std::result::Result<Delivery, ChatError> {
            self.0.lock().unwrap().push(text.to_string());
            Ok(Delivery::Confirmed)
        }
    }

    #[tokio::test]
    async fn the_brain_types_the_briefing() {
        let (store, _, p) = setup();
        let typed = Arc::new(Typed(Mutex::new(Vec::new())));
        let brain = InputBrain::new(store, typed.clone());
        let turn = Turn {
            id: format!("{p}#1"),
            items: vec![],
            attempt: 1,
        };
        brain.wake(&p, &turn).await.unwrap();
        assert_eq!(typed.0.lock().unwrap().as_slice(), [turn.briefing()]);
        let none = InputBrain::new(
            Arc::new(Store::open_in_memory().unwrap()),
            Arc::new(crate::chat::NoSessions),
        );
        assert!(none.wake(&p, &turn).await.is_err());
    }
}
