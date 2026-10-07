//! The wake queue against the SQLite log: batching, shadow mode, crash
//! recovery, timeouts, tools and the baseline reader.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use quark_coordinator::events::{CoordinatorEvent, TurnEnd};
use quark_coordinator::mcp::CoordinatorMcp;
use quark_coordinator::{
    Brain, Config, Coordinator, Efficiency, Hands, LayeredPrompt, Mode, Need, NoBrain, NoHands,
    Sources, ToolCall, Turn, Usage,
};
use quark_core::worker::{Transport, WorkerEnvelope, WorkerMessage};
use quark_core::{event::kinds, Event, EventLog, HostId, NewEvent, ProjectId, Result, Seq, TaskId};
use quark_eventlog::SqliteEventLog;
use serde_json::json;
use time::{Duration, OffsetDateTime};

#[derive(Default)]
struct Recorder {
    turns: Mutex<Vec<(ProjectId, Turn)>>,
    calls: Mutex<Vec<ToolCall>>,
}

#[async_trait]
impl Brain for Recorder {
    async fn wake(&self, project: &ProjectId, turn: &Turn) -> Result<()> {
        self.turns
            .lock()
            .unwrap()
            .push((project.clone(), turn.clone()));
        Ok(())
    }
}

#[async_trait]
impl Hands for Recorder {
    async fn run(&self, _: &ProjectId, call: &ToolCall) -> Result<String> {
        self.calls.lock().unwrap().push(call.clone());
        Ok(format!("did {}", call.name()))
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    log: Arc<SqliteEventLog>,
    rec: Arc<Recorder>,
}

impl Rig {
    fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.db");
        let log = Arc::new(SqliteEventLog::open(&path).unwrap());
        Rig {
            _dir: dir,
            path,
            log,
            rec: Arc::new(Recorder::default()),
        }
    }

    async fn open(&self, mode: Mode) -> Coordinator {
        let (brain, hands): (Arc<dyn Brain>, Arc<dyn Hands>) = match mode {
            Mode::Native => (self.rec.clone(), self.rec.clone()),
            Mode::Shadow => (Arc::new(NoBrain), Arc::new(NoHands)),
        };
        Coordinator::open(
            self.log.clone(),
            HostId::from("h"),
            mode,
            Config::default(),
            brain,
            hands,
        )
        .await
        .unwrap()
    }

    /// The same log reopened, as after a daemon crash.
    fn reopen_log(&mut self) {
        self.log = Arc::new(SqliteEventLog::open(&self.path).unwrap());
    }

    async fn worker(&self, task: &str, message: WorkerMessage) -> Seq {
        let env = WorkerEnvelope {
            task: TaskId::from(task),
            generation: "g1".into(),
            via: Transport::Mcp,
            message,
        };
        self.log
            .append(
                NewEvent::typed(
                    HostId::from("h"),
                    ProjectId::from("p"),
                    Some(TaskId::from(task)),
                    kinds::WORKER,
                    &env,
                )
                .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn working(&self, task: &str) -> Seq {
        self.worker(
            task,
            WorkerMessage::Report {
                state: "working".into(),
                note: "tests pass".into(),
            },
        )
        .await
    }

    async fn ask(&self, task: &str, key: &str, q: &str) -> Seq {
        self.worker(
            task,
            WorkerMessage::Ask {
                key: key.into(),
                question: q.into(),
            },
        )
        .await
    }

    async fn events(&self, kind: &str) -> Vec<Event> {
        self.log
            .read(Seq::ZERO, 10_000)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.kind.as_str() == kind)
            .collect()
    }

    async fn decoded(&self, kind: &str) -> Vec<CoordinatorEvent> {
        self.events(kind)
            .await
            .iter()
            .map(|e| e.decode().unwrap())
            .collect()
    }
}

fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

fn p() -> ProjectId {
    ProjectId::from("p")
}

#[tokio::test]
async fn history_wakes_no_one_and_progress_never_does() {
    let rig = Rig::new();
    rig.ask("t0", "old", "an old question").await;
    let c = rig.open(Mode::Native).await;
    for _ in 0..5 {
        rig.working("t1").await;
    }
    let r = c.tick(now()).await.unwrap();
    assert_eq!(r.evaluated, 5);
    assert_eq!(r.turns, 0);
    assert!(rig.rec.turns.lock().unwrap().is_empty());
}

#[tokio::test]
async fn shadow_records_would_wake_once_and_acts_on_nothing() {
    let mut rig = Rig::new();
    let c = rig.open(Mode::Shadow).await;
    rig.working("t1").await;
    let ask = rig.ask("t1", "schema", "v14 or v15?").await;
    rig.worker(
        "t2",
        WorkerMessage::Report {
            state: "blocked".into(),
            note: "no credentials".into(),
        },
    )
    .await;
    rig.working("t2").await;
    let r = c.tick(now()).await.unwrap();
    assert_eq!((r.items, r.turns), (2, 1));
    c.tick(now()).await.unwrap();
    let would = rig.decoded("coordinator.would_wake").await;
    assert_eq!(would.len(), 1);
    let CoordinatorEvent::WouldWake { items } = &would[0] else {
        unreachable!()
    };
    assert_eq!(items[0].source, ask);
    assert_eq!(items[0].need, Need::Decision);
    assert_eq!(items[1].need, Need::Blocked);
    assert!(c
        .call(&p(), ToolCall::TellUser { text: "x".into() })
        .await
        .is_err());

    // A restart re-evaluates nothing it already batched.
    drop(c);
    rig.reopen_log();
    let c = rig.open(Mode::Shadow).await;
    c.tick(now()).await.unwrap();
    assert_eq!(rig.decoded("coordinator.would_wake").await.len(), 1);
    let eff = Efficiency::fold(&rig.log.read(Seq::ZERO, 1000).await.unwrap(), |_| true);
    assert_eq!(eff.would_wake, 1);
}

#[tokio::test]
async fn native_batches_items_into_turns_and_waits_for_the_turn_to_end() {
    let rig = Rig::new();
    let c = rig.open(Mode::Native).await;
    rig.ask("t1", "schema", "v14 or v15?").await;
    rig.worker(
        "t2",
        WorkerMessage::Done {
            summary: "fix".into(),
            pull_request: Some("https://github.com/x/y/pull/9".into()),
        },
    )
    .await;
    c.tick(now()).await.unwrap();
    {
        let turns = rig.rec.turns.lock().unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].1.id, "p#1");
        assert_eq!(turns[0].1.items.len(), 2);
        let b = turns[0].1.briefing();
        assert!(b.starts_with("Wake p#1: 2 items need your judgment."));
        assert!(b.contains("- [decision] t1: asks: v14 or v15?"));
        assert!(b.contains("pull/9"));
    }

    // While the turn is open, new items wait; the same question asked
    // again replaces the waiting one.
    rig.ask("t3", "name", "first wording").await;
    rig.ask("t3", "name", "second wording").await;
    c.tick(now()).await.unwrap();
    assert_eq!(rig.rec.turns.lock().unwrap().len(), 1);
    let pending = c.pending(&p()).await;
    assert_eq!(pending.len(), 1);
    assert!(pending[0].summary.contains("second wording"));

    // The coordinator acts, then its turn ends.
    let out = c
        .call(
            &p(),
            ToolCall::Answer {
                task: "t1".into(),
                key: "schema".into(),
                answer: "v15".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(out, "did answer");
    let ended = c
        .turn_ended(
            &p(),
            Usage {
                input: 1000,
                output: 50,
                cache_read: 800,
                calls: 2,
            },
            Some("hook-1"),
        )
        .await
        .unwrap();
    assert_eq!(ended.as_deref(), Some("p#1"));
    // The hook delivered twice records once.
    c.turn_ended(&p(), Usage::default(), Some("hook-1"))
        .await
        .unwrap();
    assert_eq!(rig.events("coordinator.turn_ended").await.len(), 1);

    c.tick(now()).await.unwrap();
    let turns = rig.rec.turns.lock().unwrap().clone();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[1].1.id, "p#2");
    assert_eq!(turns[1].1.items.len(), 1);

    // A turn that only read is an acknowledgement.
    c.call(&p(), ToolCall::Fleet {}).await.unwrap();
    c.turn_ended(&p(), Usage::default(), None).await.unwrap();
    let eff = Efficiency::fold(&rig.log.read(Seq::ZERO, 1000).await.unwrap(), |_| true);
    assert_eq!(eff.native.turns, 2);
    assert_eq!(eff.native.acks, 1);
    assert_eq!(eff.native.usage.total(), 1050);
}

#[tokio::test]
async fn a_crash_redelivers_the_open_turn_and_closes_a_tool_that_was_running() {
    let mut rig = Rig::new();
    let c = rig.open(Mode::Native).await;
    rig.ask("t1", "k", "q?").await;
    c.tick(now()).await.unwrap();
    // A tool call recorded, then the daemon dies before its outcome.
    let tool = CoordinatorEvent::Tool {
        id: "x1".into(),
        turn: Some("p#1".into()),
        call: ToolCall::Cancel {
            task: "t1".into(),
            reason: "r".into(),
        },
    };
    rig.log
        .append(NewEvent::typed(HostId::from("h"), p(), None, tool.kind(), &tool).unwrap())
        .await
        .unwrap();
    // And an item arrives that the dead daemon never evaluated.
    rig.ask("t2", "k", "another?").await;
    drop(c);

    rig.reopen_log();
    let c = rig.open(Mode::Native).await;
    let done = rig.decoded("coordinator.tool_done").await;
    assert_eq!(done.len(), 1);
    let CoordinatorEvent::ToolDone { id, ok, .. } = &done[0] else {
        unreachable!()
    };
    assert_eq!((id.as_str(), *ok), ("x1", false));
    assert!(rig.rec.calls.lock().unwrap().is_empty(), "never re-run");

    let r = c.tick(now()).await.unwrap();
    assert_eq!(r.redelivered, 1);
    let turns = rig.rec.turns.lock().unwrap().clone();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[1].1.id, "p#1");
    assert_eq!(turns[1].1.attempt, 2);
    assert!(turns[1].1.briefing().contains("attempt 2"));
    assert_eq!(c.pending(&p()).await.len(), 1);

    // Reopening again without the turn ending redelivers once more, and
    // the waiting item survives the restart too.
    drop(c);
    rig.reopen_log();
    let c = rig.open(Mode::Native).await;
    c.tick(now()).await.unwrap();
    assert_eq!(rig.rec.turns.lock().unwrap().last().unwrap().1.attempt, 3);
    c.turn_ended(&p(), Usage::default(), None).await.unwrap();
    c.tick(now()).await.unwrap();
    let last = rig.rec.turns.lock().unwrap().last().unwrap().1.clone();
    assert_eq!(last.id, "p#2");
    assert!(last.items[0].summary.contains("another?"));
}

#[tokio::test]
async fn a_turn_that_never_ends_times_out_and_its_items_wake_again() {
    let rig = Rig::new();
    let c = rig.open(Mode::Native).await;
    rig.ask("t1", "k", "q?").await;
    c.tick(now()).await.unwrap();
    rig.ask("t2", "k", "later?").await;
    c.tick(now()).await.unwrap();
    assert_eq!(rig.rec.turns.lock().unwrap().len(), 1);

    c.tick(now() + Duration::minutes(31)).await.unwrap();
    let ends = rig.decoded("coordinator.turn_ended").await;
    assert!(matches!(
        ends[0],
        CoordinatorEvent::TurnEnded {
            end: TurnEnd::TimedOut,
            ..
        }
    ));
    let turns = rig.rec.turns.lock().unwrap().clone();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[1].1.id, "p#2");
    let summaries: Vec<&str> = turns[1]
        .1
        .items
        .iter()
        .map(|i| i.summary.as_str())
        .collect();
    assert_eq!(summaries, ["t1: asks: q?", "t2: asks: later?"]);
}

#[tokio::test]
async fn explicit_requests_and_dispatch_escalations_wake() {
    let rig = Rig::new();
    let c = rig.open(Mode::Native).await;
    c.request(&p(), None, "nightly failed twice", "rule nightly")
        .await
        .unwrap();
    rig.log
        .append(NewEvent::new(
            HostId::from("h"),
            p(),
            Some(TaskId::from("t9")),
            "dispatch.escalated",
            json!({"type": "escalated", "reason": "quota tie"}),
        ))
        .await
        .unwrap();
    c.tick(now()).await.unwrap();
    let turns = rig.rec.turns.lock().unwrap().clone();
    let needs: Vec<Need> = turns[0].1.items.iter().map(|i| i.need).collect();
    assert_eq!(needs, [Need::Requested, Need::DispatchEscalated]);
    assert!(c.request(&p(), None, "  ", "x").await.is_err());
}

#[tokio::test]
async fn prompts_are_recorded_on_change_and_serve_skills() {
    let rig = Rig::new();
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("quark");
    std::fs::create_dir_all(repo.join(".agents/skills/verify")).unwrap();
    std::fs::write(repo.join("AGENTS.md"), "Rebase first.\n").unwrap();
    std::fs::write(
        repo.join(".agents/skills/verify/SKILL.md"),
        "---\nname: verify\ndescription: Prove it works.\n---\nLaunch, drive, capture.\n",
    )
    .unwrap();
    let pack = |id: &str| {
        quark_persona::builtin()
            .iter()
            .find(|p| p.id == id)
            .unwrap()
            .clone()
    };
    let sources = |id: &str| Sources {
        persona: pack(id),
        repos: vec![("quark".into(), repo.clone())],
        ..Default::default()
    };
    let c = Arc::new(rig.open(Mode::Native).await);
    assert!(c
        .set_prompt(&p(), LayeredPrompt::build(&sources("nautical")), "nautical")
        .await
        .unwrap());
    assert!(!c
        .set_prompt(&p(), LayeredPrompt::build(&sources("nautical")), "nautical")
        .await
        .unwrap());
    assert!(c.prompt(&p()).unwrap().contains("Rebase first."));
    // Switching persona is a new prompt, with the same neutral layers.
    assert!(c
        .set_prompt(
            &p(),
            LayeredPrompt::build(&sources("kitchen-brigade")),
            "kitchen-brigade"
        )
        .await
        .unwrap());
    let prompts = rig.decoded("coordinator.prompt").await;
    assert_eq!(prompts.len(), 2);
    let (CoordinatorEvent::Prompt { prompt: a }, CoordinatorEvent::Prompt { prompt: b }) =
        (&prompts[0], &prompts[1])
    else {
        unreachable!()
    };
    assert_eq!(a.layers[0], b.layers[0]);
    assert_eq!(a.layers[2], b.layers[2]);
    assert_ne!(a.layers[1], b.layers[1]);

    // Over MCP.
    let mcp = CoordinatorMcp::new(c.clone());
    let list = mcp
        .handle(
            &p(),
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        )
        .await
        .unwrap();
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"start_task") && !names.contains(&"merge"));
    let skill = mcp
        .handle(
            &p(),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                   "params": {"name": "load_skill", "arguments": {"name": "verify"}}}),
        )
        .await
        .unwrap();
    assert_eq!(skill["result"]["isError"], false);
    assert_eq!(
        skill["result"]["content"][0]["text"],
        "Launch, drive, capture."
    );
    let bad = mcp
        .handle(
            &p(),
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                   "params": {"name": "steer", "arguments": {"task": "t1"}}}),
        )
        .await
        .unwrap();
    assert_eq!(bad["result"]["isError"], true);
    let unknown = mcp
        .handle(
            &p(),
            json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
                   "params": {"name": "merge", "arguments": {}}}),
        )
        .await
        .unwrap();
    assert!(unknown["error"].is_object());
    assert!(mcp
        .handle(
            &p(),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
        )
        .await
        .is_none());
}

#[tokio::test]
async fn the_baseline_is_read_once_per_turn() {
    let rig = Rig::new();
    let c = rig.open(Mode::Shadow).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("coordinator.jsonl");
    let user = |id: &str, text: &str| {
        format!(
            "{}\n",
            json!({"type": "user", "uuid": id, "timestamp": "t", "message": {"role": "user", "content": text}})
        )
    };
    let reply = |id: &str| {
        format!(
            "{}\n",
            json!({"type": "assistant", "message": {"id": id, "content": [], "usage": {"input_tokens": 10, "output_tokens": 2}}})
        )
    };
    let text = [
        user("u1", "<task-notification>done</task-notification>"),
        reply("m1"),
        user("u2", "what's the status?"),
        reply("m2"),
        user("u3", "signal: t1"),
    ]
    .concat();
    std::fs::write(&path, text).unwrap();
    assert_eq!(c.read_baseline(&p(), &path).await.unwrap(), 2);
    assert_eq!(c.read_baseline(&p(), &path).await.unwrap(), 0);
    let eff = Efficiency::fold(&rig.log.read(Seq::ZERO, 1000).await.unwrap(), |_| true);
    assert_eq!(eff.baseline.turns, 2);
    assert_eq!(eff.baseline.acks, 1);
    assert_eq!(eff.baseline.usage.total(), 24);
}
