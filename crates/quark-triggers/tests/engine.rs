//! The slice 7 engine end to end over a real event log: shadow comparison
//! with firstmate, rule fires and their at-most-once guarantee across
//! restarts, polled conditions, routing, digests and the return brief.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use quark_core::event::kinds;
use quark_core::slice::Divergence;
use quark_core::{EventLog, HostId, NewEvent, ProjectId, Result, Seq, TaskId};
use quark_eventlog::SqliteEventLog;
use quark_triggers::commands::Ran;
use quark_triggers::trigger::{Status, TriggerEvent};
use quark_triggers::{
    Action, AwayPolicy, Commands, Condition, Effects, Engine, Expect, Inbound, Item, Mode,
    Occasion, Posture, Reach, Route, Rule, INBOX,
};
use time::OffsetDateTime;

fn project() -> ProjectId {
    ProjectId::from("quark")
}

#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);

impl Recorder {
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut self.0.lock().unwrap())
    }
}

#[async_trait]
impl Effects for Recorder {
    async fn wake(&self, _: &ProjectId, task: Option<&TaskId>, note: &str) -> Result<()> {
        let t = task.map(|t| t.to_string()).unwrap_or_default();
        self.0.lock().unwrap().push(format!("wake[{t}] {note}"));
        Ok(())
    }
    async fn notify(&self, _: &ProjectId, _: Option<&TaskId>, note: &str) -> Result<()> {
        self.0.lock().unwrap().push(format!("notify {note}"));
        Ok(())
    }
    async fn digest(&self, _: &ProjectId, items: &[Item], returning: bool) -> Result<()> {
        let what: Vec<_> = items.iter().map(|i| i.summary.clone()).collect();
        self.0
            .lock()
            .unwrap()
            .push(format!("digest returning={returning} {}", what.join(" | ")));
        Ok(())
    }
    async fn steer(&self, _: &ProjectId, task: &TaskId, text: &str) -> Result<()> {
        self.0.lock().unwrap().push(format!("steer[{task}] {text}"));
        Ok(())
    }
}

/// Commands that answer from a script and record what ran.
#[derive(Default)]
struct Script {
    answers: Mutex<VecDeque<Ran>>,
    ran: Mutex<Vec<Vec<String>>>,
}

impl Script {
    fn push(&self, code: Option<i32>, stdout: &str) {
        self.answers.lock().unwrap().push_back(Ran {
            code,
            timed_out: false,
            stdout: stdout.into(),
            stderr: String::new(),
        });
    }
    fn ran(&self) -> Vec<Vec<String>> {
        self.ran.lock().unwrap().clone()
    }
}

#[async_trait]
impl Commands for Script {
    async fn run(&self, argv: &[String], _: Duration) -> Ran {
        self.ran.lock().unwrap().push(argv.to_vec());
        self.answers.lock().unwrap().pop_front().unwrap_or(Ran {
            code: Some(0),
            ..Ran::default()
        })
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    log: Arc<SqliteEventLog>,
    effects: Arc<Recorder>,
    script: Arc<Script>,
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.db");
        let log = Arc::new(SqliteEventLog::open(&path).unwrap());
        Self {
            _dir: dir,
            path,
            log,
            effects: Arc::new(Recorder::default()),
            script: Arc::new(Script::default()),
        }
    }

    async fn engine(&self, mode: Mode) -> Engine {
        Engine::open(
            self.log.clone(),
            HostId::from("local"),
            mode,
            self.effects.clone(),
            self.script.clone(),
        )
        .await
        .unwrap()
    }

    /// A fresh log handle on the same file, as after a daemon restart.
    async fn restarted(&self, mode: Mode) -> Engine {
        let log = Arc::new(SqliteEventLog::open(&self.path).unwrap());
        Engine::open(
            log,
            HostId::from("local"),
            mode,
            self.effects.clone(),
            self.script.clone(),
        )
        .await
        .unwrap()
    }

    /// A status line as the slice 1 bridge records it.
    async fn status(&self, task: &str, line: &str) -> Seq {
        let verb = line.split(':').next().unwrap().trim().to_string();
        let note = line.split_once(':').map(|x| x.1.trim()).unwrap_or("");
        let payload = serde_json::json!({
            "verb": verb, "key": null, "corr": null,
            "note": note, "raw": line, "offset": 0,
        });
        self.log
            .append(NewEvent::new(
                HostId::from("local"),
                project(),
                Some(TaskId::from(task)),
                quark_eventlog::firstmate::kinds::STATUS,
                payload,
            ))
            .await
            .unwrap()
    }

    async fn of_kind(&self, kind: &str) -> Vec<quark_core::Event> {
        self.log
            .read(Seq::ZERO, 10_000)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.kind.as_str() == kind)
            .collect()
    }
}

fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

#[tokio::test]
async fn shadow_agrees_with_firstmate_and_records_disagreement() {
    let rig = Rig::new();
    let engine = rig.engine(Mode::Shadow).await;
    for line in [
        "working: started",
        "needs-decision: key=api which shape?",
        "resolved: key=api answered",
        "paused: waiting on CI",
        "note: PR ready for review",
        "done: PR https://example/pr/1 checks green",
        "failed: tests red",
    ] {
        rig.status("t1", line).await;
    }
    let r = engine.tick(now()).await.unwrap();
    assert_eq!(r.compared, 7);
    assert_eq!(r.diverged, 0);
    // Shadow mode never acts.
    assert!(rig.effects.take().is_empty());
    let shadowed = rig.of_kind("away.shadowed").await;
    assert_eq!(shadowed.len(), 1);
    assert_eq!(shadowed[0].payload["compared"], 7);

    // A Project that stops waking on done disagrees with firstmate.
    let mut policy = AwayPolicy::default();
    for posture in [Posture::Present, Posture::Away, Posture::Quiet] {
        policy.set(posture, Occasion::Done, Route::new(false, Reach::Digest));
    }
    engine.set_policy(&project(), policy, "user").await.unwrap();
    rig.status("t2", "done: PR https://example/pr/2").await;
    rig.status("t2", "working: next").await;
    let r = engine.tick(now()).await.unwrap();
    assert_eq!((r.compared, r.diverged), (2, 1));
    let div = rig.of_kind(kinds::SHADOW_DIVERGENCE).await;
    assert_eq!(div.len(), 1);
    let d: Divergence = div[0].decode().unwrap();
    assert_eq!(d.operation, "away.route");
    assert_eq!(d.bash["escalate"], true);
    assert_eq!(d.native["wake"], false);

    // Nothing is compared twice, here or after a restart.
    assert_eq!(engine.tick(now()).await.unwrap().compared, 0);
    let again = rig.restarted(Mode::Shadow).await;
    assert_eq!(again.tick(now()).await.unwrap().compared, 0);
}

#[tokio::test]
async fn mirrors_the_firstmate_inbox_and_posture() {
    let rig = Rig::new();
    let engine = rig.engine(Mode::Shadow).await;
    let home = tempfile::tempdir().unwrap();
    let inbox = home.path().join("state/inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::write(
        inbox.join("1-a.note"),
        "id=1-a\nat=2026-10-07T01:00:00Z\nsource=cli\n--\nlook at the flaky test\n",
    )
    .unwrap();
    let r = engine
        .mirror_firstmate(&project(), home.path())
        .await
        .unwrap();
    assert_eq!((r.received, r.acked, r.posture), (1, 0, false));
    let pending = engine.inbox(&project()).await;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].body, "look at the flaky test");

    // firstmate acks it and the user goes away.
    std::fs::create_dir_all(inbox.join("handled")).unwrap();
    std::fs::rename(inbox.join("1-a.note"), inbox.join("handled/1-a.note")).unwrap();
    std::fs::write(home.path().join("state/.afk"), "away\n").unwrap();
    let r = engine
        .mirror_firstmate(&project(), home.path())
        .await
        .unwrap();
    assert_eq!((r.received, r.acked, r.posture), (0, 1, true));
    assert!(engine.inbox(&project()).await.is_empty());
    assert_eq!(engine.posture(&project()).await, Posture::Away);

    // Running it again changes nothing.
    let r = engine
        .mirror_firstmate(&project(), home.path())
        .await
        .unwrap();
    assert_eq!((r.received, r.acked, r.posture), (0, 0, false));

    std::fs::remove_file(home.path().join("state/.afk")).unwrap();
    engine
        .mirror_firstmate(&project(), home.path())
        .await
        .unwrap();
    assert_eq!(engine.posture(&project()).await, Posture::Present);
}

#[tokio::test]
async fn inbox_is_one_inbound_api() {
    let rig = Rig::new();
    let engine = rig.engine(Mode::Native).await;
    let note = Inbound::note("user", "rebase the dashboard PR");
    let s1 = engine.receive(&project(), note.clone()).await.unwrap();
    // The same message again (a retry) lands once.
    let s2 = engine.receive(&project(), note.clone()).await.unwrap();
    assert_eq!(s1, s2);
    let mail = Inbound {
        id: "<msg-1@example>".into(),
        channel: "email".into(),
        from: "ops@example".into(),
        body: "disk alert".into(),
        at: now(),
        task: None,
    };
    engine.receive(&project(), mail).await.unwrap();
    assert_eq!(engine.inbox(&project()).await.len(), 2);
    assert!(engine
        .receive(&project(), Inbound::note("user", "  "))
        .await
        .is_err());

    // Each arrival wakes the coordinator once.
    engine.tick(now()).await.unwrap();
    let woke = rig.effects.take();
    assert_eq!(woke.len(), 2, "{woke:?}");
    assert!(woke[0].starts_with("wake[] inbox from user: rebase"));

    engine
        .ack(&project(), INBOX, &note.id, "coordinator")
        .await
        .unwrap();
    engine
        .ack(&project(), INBOX, &note.id, "coordinator")
        .await
        .unwrap();
    assert!(engine.ack(&project(), INBOX, "nope", "x").await.is_err());
    assert_eq!(engine.inbox(&project()).await.len(), 1);
    let again = rig.restarted(Mode::Native).await;
    assert_eq!(again.inbox(&project()).await.len(), 1);
}

#[tokio::test]
async fn event_rules_fire_once_per_event_and_survive_restarts() {
    let rig = Rig::new();
    // History before the engine first ran never fires anything.
    rig.status("old", "done: before the engine existed").await;
    let engine = rig.engine(Mode::Native).await;
    let mut fields = BTreeMap::new();
    fields.insert("/verb".to_string(), serde_json::json!("done"));
    let rule = Rule::new(
        "steer-on-done",
        Condition::Event {
            kind: "firstmate.status".into(),
            task: None,
            fields,
        },
        Action::Steer {
            task: TaskId::from("reviewer"),
            text: "a PR is ready to review".into(),
        },
    );
    engine
        .define(&project(), rule.clone(), "user")
        .await
        .unwrap();
    // Defining the same rule again changes nothing.
    engine.define(&project(), rule, "user").await.unwrap();
    engine.tick(now()).await.unwrap();
    assert!(rig.effects.take().is_empty());

    rig.status("t1", "working: on it").await;
    rig.status("t1", "done: PR https://example/pr/3").await;
    let r = engine.tick(now()).await.unwrap();
    assert_eq!(r.fired, 1);
    let calls = rig.effects.take();
    assert!(calls.contains(&"steer[reviewer] a PR is ready to review".to_string()));
    let outcomes = rig.of_kind("trigger.outcome").await;
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].payload["status"], "ok");

    // The outcome itself is routed: digest while present, no wake.
    engine.tick(now()).await.unwrap();
    assert!(rig.effects.take().is_empty());

    // A restart evaluates nothing twice.
    let again = rig.restarted(Mode::Native).await;
    let r = again.tick(now()).await.unwrap();
    assert_eq!(r.fired, 0);
    assert!(rig.effects.take().is_empty());
    rig.status("t2", "done: another").await;
    assert_eq!(again.tick(now()).await.unwrap().fired, 1);

    // Removed rules stop.
    again
        .remove(&project(), &"steer-on-done".into(), "user")
        .await
        .unwrap();
    rig.status("t3", "done: third").await;
    assert_eq!(again.tick(now()).await.unwrap().fired, 0);
}

#[tokio::test]
async fn an_interrupted_fire_is_ambiguous_not_rerun() {
    let rig = Rig::new();
    let engine = rig.engine(Mode::Native).await;
    let rule = Rule::new(
        "deploy",
        Condition::At {
            at: OffsetDateTime::UNIX_EPOCH,
        },
        Action::Command {
            argv: vec!["deploy".into()],
            timeout_secs: 10,
        },
    );
    engine.define(&project(), rule, "user").await.unwrap();
    let defined = engine.rules(&project()).await.remove(0);
    // A crash between the claim and the outcome.
    let claim = TriggerEvent::Fired {
        id: "deploy".into(),
        fire: defined.fire_id("at"),
        cause: None,
    };
    let mut ev =
        NewEvent::typed(HostId::from("local"), project(), None, claim.kind(), &claim).unwrap();
    ev.id = quark_core::EventId::new();
    rig.log.append(ev).await.unwrap();
    drop(engine);

    let again = rig.restarted(Mode::Native).await;
    again.tick(now()).await.unwrap();
    assert!(rig.script.ran().is_empty(), "the action ran again");
    let outcomes = rig.of_kind("trigger.outcome").await;
    assert_eq!(outcomes.len(), 1);
    let TriggerEvent::Outcome { status, .. } = outcomes[0].decode().unwrap() else {
        panic!()
    };
    assert_eq!(status, Status::Ambiguous);
    // An unknown outcome reaches the coordinator.
    again.tick(now()).await.unwrap();
    assert!(rig
        .effects
        .take()
        .iter()
        .any(|c| c.starts_with("wake[] rule deploy")));
}

#[tokio::test]
async fn polled_conditions_need_a_stable_true_and_have_an_error_budget() {
    let rig = Rig::new();
    let engine = rig.engine(Mode::Native).await;
    let poll = |id: &str, expect| {
        Rule::new(
            id,
            Condition::Command {
                argv: vec!["check".into(), id.into()],
                interval_secs: 1,
                stable: 2,
                timeout_secs: 5,
                expect,
                error_budget: 2,
            },
            Action::Inbox {
                body: format!("{id} holds"),
            },
        )
    };
    engine
        .define(
            &project(),
            poll("ci", Some(Expect::Differs("OPEN 0".into()))),
            "user",
        )
        .await
        .unwrap();
    let mut t = now();
    // true, not yet (same output), true, true -> fires on the second true in a row.
    for (code, out) in [
        (Some(0), "OPEN 1"),
        (Some(0), "OPEN 0"),
        (Some(0), "CLOSED 1"),
        (Some(0), "CLOSED 1"),
    ] {
        rig.script.push(code, out);
    }
    let mut fired = 0;
    for _ in 0..4 {
        fired += engine.tick(t).await.unwrap().fired;
        t += time::Duration::seconds(2);
    }
    assert_eq!(fired, 1);
    assert_eq!(rig.script.ran().len(), 4);
    let inbox = engine.inbox(&project()).await;
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].body, "ci holds");
    assert_eq!(inbox[0].from, "trigger:ci");
    // Polled rules fire once.
    engine.tick(t + time::Duration::seconds(5)).await.unwrap();
    assert_eq!(rig.script.ran().len(), 4);

    // Errors in a row end the rule with a condition error.
    engine
        .define(&project(), poll("flaky", None), "user")
        .await
        .unwrap();
    rig.script.push(Some(2), "");
    rig.script.push(None, "");
    for _ in 0..3 {
        engine.tick(t).await.unwrap();
        t += time::Duration::seconds(2);
    }
    let outcomes = rig.of_kind("trigger.outcome").await;
    let statuses: Vec<_> = outcomes
        .iter()
        .map(|e| e.payload["status"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(statuses, ["ok", "condition_error"]);
}

#[tokio::test]
async fn clocks_fire_per_slot_and_shadow_only_records() {
    let rig = Rig::new();
    let engine = rig.engine(Mode::Shadow).await;
    let rule = Rule::new(
        "hourly",
        Condition::Every { secs: 3600 },
        Action::Wake {
            note: "hourly check".into(),
        },
    );
    engine.define(&project(), rule, "user").await.unwrap();
    let t = now();
    // Not in the slot it was defined in.
    assert_eq!(engine.tick(t).await.unwrap().fired, 0);
    let later = t + time::Duration::hours(1);
    assert_eq!(engine.tick(later).await.unwrap().fired, 1);
    assert_eq!(engine.tick(later).await.unwrap().fired, 0);
    assert_eq!(
        engine
            .tick(later + time::Duration::hours(1))
            .await
            .unwrap()
            .fired,
        1
    );
    assert_eq!(rig.of_kind("trigger.would_fire").await.len(), 2);
    assert!(rig.of_kind("trigger.fired").await.is_empty());
    assert!(rig.effects.take().is_empty());
}

#[tokio::test]
async fn away_holds_decisions_digests_the_rest_and_briefs_on_return() {
    let rig = Rig::new();
    let engine = rig.engine(Mode::Native).await;
    engine
        .set_posture(
            &ProjectId::engine(),
            Posture::Away,
            Some("back tomorrow".into()),
            None,
            "user",
        )
        .await
        .unwrap();
    rig.status("t1", "working: going").await;
    rig.status("t1", "needs-decision: key=db which database?")
        .await;
    rig.status("t2", "done: PR https://example/pr/9").await;
    let t = now();
    engine.tick(t).await.unwrap();
    let calls = rig.effects.take();
    // The coordinator is woken for both; the user hears nothing yet.
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls.iter().all(|c| c.starts_with("wake")));
    assert_eq!(engine.held(&project()).await.len(), 1);
    assert_eq!(engine.digest(&project()).await.len(), 1);

    // The digest goes out on its cadence, without the held decision.
    engine.tick(t + time::Duration::hours(2)).await.unwrap();
    let calls = rig.effects.take();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(calls[0].starts_with("digest returning=false t2: done"));
    assert!(engine.digest(&project()).await.is_empty());

    // Coming back delivers the return brief with what was held.
    engine
        .set_posture(&ProjectId::engine(), Posture::Present, None, None, "user")
        .await
        .unwrap();
    engine.tick(t + time::Duration::hours(2)).await.unwrap();
    let calls = rig.effects.take();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(calls[0].starts_with("digest returning=true t1: needs-decision"));
    assert!(engine.held(&project()).await.is_empty());

    // Present: a decision notifies the user right away.
    rig.status("t3", "needs-decision: key=x now?").await;
    engine.tick(t + time::Duration::hours(3)).await.unwrap();
    let calls = rig.effects.take();
    assert!(
        calls.iter().any(|c| c.starts_with("notify t3:")),
        "{calls:?}"
    );
}

#[tokio::test]
async fn policies_that_lose_a_decision_are_refused() {
    let rig = Rig::new();
    let engine = rig.engine(Mode::Native).await;
    let mut p = AwayPolicy::default();
    p.set(
        Posture::Away,
        Occasion::Decision,
        Route::new(false, Reach::Silent),
    );
    assert!(engine.set_policy(&project(), p, "user").await.is_err());
    let bad = Rule::new(
        "loop",
        Condition::Event {
            kind: "trigger.*".into(),
            task: None,
            fields: BTreeMap::new(),
        },
        Action::Wake { note: "x".into() },
    );
    assert!(engine.define(&project(), bad, "user").await.is_err());
}
