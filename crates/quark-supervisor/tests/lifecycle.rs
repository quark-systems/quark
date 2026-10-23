//! The supervisor against real pseudo-terminals, real git worktrees and the
//! SQLite event log, with a shell script standing in for the agent.

mod common;

use std::sync::Arc;

use common::{eventually, git, received, Env};
use quark_core::session::{SessionBackend, SessionSpec};
use quark_core::worktree::ReturnOutcome;
use quark_core::{CoreError, NewEvent, ProjectId, TaskEvent, TaskId, WorkerProtocol};
use quark_sessions::pty::PtySupervisor;
use quark_supervisor::{Cause, Conversations, Relaunch, SupervisorEvent};
use quark_systems::TaskState;

fn ids(task: &str) -> (ProjectId, TaskId) {
    (ProjectId::from("p"), TaskId::from(task))
}

#[tokio::test]
async fn spawn_steer_decide_and_ship() {
    let env = Env::new();
    let pty = Arc::new(PtySupervisor::new());
    let sup = env.supervisor(pty.clone()).await;
    let (p, t) = ids("t1");

    let w = sup.spawn(env.request("t1")).await.unwrap();
    let wt = w.worktree.clone().unwrap();
    assert_ne!(wt.path, env.repo);
    assert!(wt.path.join(".git").is_file(), "a linked worktree");
    assert_eq!(
        git(&wt.path, &["branch", "--show-current"]).trim(),
        "quark/t1"
    );
    let g = w.current.clone().unwrap();
    assert_eq!(g.cause, Cause::Spawn);
    assert_eq!(sup.task(&p, &t).unwrap().0.state, TaskState::Running);
    eventually("the brief", async || {
        received(&env, "t1", &g.id)
            .iter()
            .any(|l| l == "Fix the parser.")
    })
    .await;

    sup.steer(&p, &t, "ask now").await.unwrap();
    eventually("a decision", async || {
        sup.tick().await.unwrap();
        sup.task(&p, &t).unwrap().0.state == TaskState::NeedsDecision
    })
    .await;
    assert!(sup.task(&p, &t).unwrap().0.open_decisions.contains("k1"));

    sup.answer(&p, &t, "k1", "left").await.unwrap();
    assert_eq!(sup.task(&p, &t).unwrap().0.state, TaskState::Running);
    eventually("the answer", async || {
        received(&env, "t1", &g.id)
            .iter()
            .any(|l| l == "Answer to [k1]: left")
    })
    .await;

    sup.steer(&p, &t, "ship it").await.unwrap();
    eventually("review", async || {
        sup.tick().await.unwrap();
        sup.task(&p, &t).unwrap().0.state == TaskState::InReview
    })
    .await;
    let (record, worker) = sup.task(&p, &t).unwrap();
    assert_eq!(
        record.pull_request.as_deref(),
        Some("https://github.com/o/r/pull/7")
    );
    assert!(worker.undelivered().next().is_none());
    // Nothing above ended the session or started another.
    assert_eq!(worker.current.unwrap().id, g.id);
}

#[tokio::test]
async fn daemon_crash_adopts_the_running_worker() {
    let env = Env::new();
    // The PTY supervisor stands in for quark-ptyd: it outlives the daemon.
    let pty = Arc::new(PtySupervisor::new());
    let (p, t) = ids("t1");
    let g = {
        let sup = env.supervisor(pty.clone()).await;
        let w = sup.spawn(env.request("t1")).await.unwrap();
        w.current.unwrap()
    };

    let sup = env.supervisor(pty.clone()).await;
    assert_eq!(sup.recover().await.unwrap(), 0);
    let tick = sup.tick().await.unwrap();
    assert_eq!(tick.recovered, 0);
    assert_eq!(tick.exited, 0);
    let (record, worker) = sup.task(&p, &t).unwrap();
    assert_eq!(record.state, TaskState::Running);
    assert_eq!(worker.current.unwrap().id, g.id);

    sup.steer(&p, &t, "still there?").await.unwrap();
    eventually("the steer", async || {
        received(&env, "t1", &g.id)
            .iter()
            .any(|l| l == "still there?")
    })
    .await;
}

#[tokio::test]
async fn lost_sessions_are_relaunched_with_waiting_messages() {
    let env = Env::new();
    let (p, t) = ids("t1");
    let old = {
        let sup = env.supervisor(Arc::new(PtySupervisor::new())).await;
        sup.spawn(env.request("t1")).await.unwrap().current.unwrap()
    };
    // The session server went away with the daemon (tmux loss, reboot).
    let pty = Arc::new(PtySupervisor::new());
    let sup = env.supervisor(pty.clone()).await;
    // A message sent before the supervisor notices: recorded, not lost.
    sup.steer(&p, &t, "remember the schema").await.unwrap();
    sup.recover().await.unwrap();
    let tick = sup.tick().await.unwrap();
    assert_eq!((tick.exited, tick.recovered), (1, 1));

    let (record, worker) = sup.task(&p, &t).unwrap();
    let new = worker.current.clone().unwrap();
    assert_ne!(new.id, old.id);
    assert_eq!(new.cause, Cause::Recover);
    assert_eq!(record.state, TaskState::Running);
    assert_eq!(record.generation.as_deref(), Some(new.id.as_str()));
    assert_eq!(worker.recoveries, 1);
    assert!(worker.undelivered().next().is_none());
    eventually("the relaunched brief", async || {
        let got = received(&env, "t1", &new.id);
        got.iter()
            .any(|l| l.starts_with("Your previous session ended"))
            && got.iter().any(|l| l == "- remember the schema")
            && got.iter().any(|l| l == "Fix the parser.")
    })
    .await;

    // A message from the replaced worker is recorded but refused.
    let err = sup
        .recorder()
        .receive(quark_worker::WorkerIdentity::new("t1", &old.id).envelope(
            quark_core::worker::Transport::Mcp,
            quark_core::worker::WorkerMessage::Learned { fact: "x".into() },
        ))
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::Refused(_)), "{err}");
}

/// Finds a conversation in every worktree.
struct Logged;

impl Conversations for Logged {
    fn exists(
        &self,
        _: &quark_core::harness::HarnessManifest,
        _: &std::path::Path,
        _: &std::collections::BTreeMap<String, String>,
    ) -> bool {
        true
    }
}

#[tokio::test]
async fn lost_sessions_resume_their_conversation() {
    let env = Env::new();
    let (p, t) = ids("t1");
    let first = {
        let sup = env.supervisor(Arc::new(PtySupervisor::new())).await;
        sup.spawn(env.request("t1")).await.unwrap().current.unwrap()
    };
    eventually("the first worker", async || {
        received(&env, "t1", &first.id).contains(&"args: ".to_string())
    })
    .await;
    // The session server went away (a reboot); the harness has logged the
    // worker's conversation in its worktree.
    let sup = env
        .supervisor(Arc::new(PtySupervisor::new()))
        .await
        .with_conversations(Arc::new(Logged));
    sup.steer(&p, &t, "remember the schema").await.unwrap();
    sup.recover().await.unwrap();
    assert_eq!(sup.tick().await.unwrap().recovered, 1);

    let new = sup.task(&p, &t).unwrap().1.current.unwrap();
    assert_eq!(new.cause, Cause::Recover);
    eventually("the resumed worker", async || {
        let got = received(&env, "t1", &new.id);
        got.contains(&"args: --continue".to_string())
            && got.iter().any(|l| {
                l.starts_with("Your previous session ended. This is the same conversation, resumed")
            })
            && got.iter().any(|l| l == "- remember the schema")
    })
    .await;
    // The conversation has the brief already.
    assert!(!received(&env, "t1", &new.id).contains(&"Fix the parser.".to_string()));
}

#[tokio::test]
async fn crashing_worker_is_recovered_then_failed() {
    let env = Env::new();
    let mut config = env.config();
    config.max_recoveries = 1;
    let sup = env
        .supervisor_with(Arc::new(PtySupervisor::new()), config)
        .await;
    let (p, t) = ids("t1");
    sup.spawn(env.request("t1")).await.unwrap();

    sup.steer(&p, &t, "crash").await.unwrap();
    eventually("one recovery", async || {
        sup.tick().await.unwrap();
        let w = sup.task(&p, &t).unwrap().1;
        w.recoveries == 1 && w.current.unwrap().exited.is_none()
    })
    .await;
    let first = sup.task(&p, &t).unwrap().1.current.unwrap();
    eventually("the recovered worker", async || {
        received(&env, "t1", &first.id)
            .iter()
            .any(|l| l == "Fix the parser.")
    })
    .await;

    sup.steer(&p, &t, "crash again").await.unwrap();
    eventually("failure", async || {
        sup.tick().await.unwrap();
        sup.task(&p, &t).unwrap().0.state == TaskState::Failed
    })
    .await;
    let w = sup.task(&p, &t).unwrap().1;
    assert_eq!(w.current.unwrap().exited, Some(Some(3)));
    // Failed tasks stay down.
    assert_eq!(sup.tick().await.unwrap().recovered, 0);
}

#[tokio::test]
async fn cancel_keeps_work_and_teardown_never_discards_it() {
    let env = Env::new();
    let sup = env.supervisor(Arc::new(PtySupervisor::new())).await;
    let (p, t) = ids("t1");
    let w = sup.spawn(env.request("t1")).await.unwrap();
    let path = w.worktree.unwrap().path;

    assert!(matches!(
        sup.teardown(&p, &t).await,
        Err(CoreError::Refused(_))
    ));
    sup.steer(&p, &t, "commit").await.unwrap();
    eventually("a commit", async || {
        git(&path, &["rev-list", "--count", "main..HEAD"]).trim() == "1"
    })
    .await;

    sup.cancel(&p, &t).await.unwrap();
    let (record, worker) = sup.task(&p, &t).unwrap();
    assert_eq!(record.state, TaskState::Failed);
    let g = worker.current.unwrap();
    assert!(g.exited.is_some());
    assert!(received(&env, "t1", &g.id).iter().any(|l| l == "/exit"));
    assert_eq!(sup.tick().await.unwrap().recovered, 0);

    match sup.teardown(&p, &t).await.unwrap() {
        ReturnOutcome::Kept { work } => assert_eq!(work.unpushed_commits, 1),
        other => panic!("{other:?}"),
    }
    assert!(path.exists());
    assert_eq!(env.worktrees.held(), 1);

    // Only an explicit discard gives it back.
    assert_eq!(
        sup.discard_teardown(&p, &t).await.unwrap(),
        ReturnOutcome::Returned
    );
    assert!(!path.exists());
    assert_eq!(env.worktrees.held(), 0);
    assert!(sup.task(&p, &t).unwrap().1.is_released());
}

#[tokio::test]
async fn landed_task_gives_its_worktree_back() {
    let env = Env::new();
    let sup = env.supervisor(Arc::new(PtySupervisor::new())).await;
    let (p, t) = ids("t1");
    let w = sup.spawn(env.request("t1")).await.unwrap();
    let path = w.worktree.unwrap().path;
    sup.steer(&p, &t, "ship").await.unwrap();
    eventually("review", async || {
        sup.tick().await.unwrap();
        sup.task(&p, &t).unwrap().0.state == TaskState::InReview
    })
    .await;
    // The pull request merged.
    sup.complete(&p, &t).await.unwrap();
    assert_eq!(sup.task(&p, &t).unwrap().0.state, TaskState::Done);
    assert_eq!(sup.teardown(&p, &t).await.unwrap(), ReturnOutcome::Returned);
    assert!(!path.exists());
    assert!(sup.task(&p, &t).unwrap().1.is_released());
    assert_eq!(sup.tick().await.unwrap(), Default::default());
    assert!(matches!(
        sup.relaunch(&p, &t, Relaunch::default()).await,
        Err(CoreError::Refused(_))
    ));
}

#[tokio::test]
async fn relaunch_switches_effort_and_tells_the_new_worker() {
    let env = Env::new();
    let sup = env.supervisor(Arc::new(PtySupervisor::new())).await;
    let (p, t) = ids("t1");
    let old = sup.spawn(env.request("t1")).await.unwrap().current.unwrap();
    assert_eq!(old.effort.as_deref(), Some("high"));

    let new = sup
        .relaunch(
            &p,
            &t,
            Relaunch {
                effort: Some("low".into()),
                note: "Take over: the parser half is done.".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_ne!(new.id, old.id);
    assert_eq!(new.cause, Cause::Relaunch);
    assert_eq!(new.effort.as_deref(), Some("low"));
    let w = sup.task(&p, &t).unwrap().1;
    assert_eq!(w.recoveries, 0);
    eventually("the note", async || {
        received(&env, "t1", &new.id)
            .iter()
            .any(|l| l == "Take over: the parser half is done.")
    })
    .await;
    assert!(received(&env, "t1", &old.id).iter().any(|l| l == "/exit"));

    let bad = Relaunch {
        effort: Some("max".into()),
        ..Default::default()
    };
    assert!(matches!(
        sup.relaunch(&p, &t, bad).await,
        Err(CoreError::Invalid(_))
    ));
}

#[tokio::test]
async fn interrupted_spawn_is_finished_and_orphans_are_killed() {
    let env = Env::new();
    let pty = Arc::new(PtySupervisor::new());
    let (p, t) = ids("t1");
    let req = env.request("t1");
    // A crash right after the assignment was recorded.
    {
        let log = env.log();
        let ledger = quark_eventlog::TaskLedger::open(log.clone(), "h".into())
            .await
            .unwrap();
        ledger
            .record(
                p.clone(),
                t.clone(),
                TaskEvent::Queued { title: "x".into() },
            )
            .await
            .unwrap();
        let e = SupervisorEvent::Assigned {
            assignment: req.assignment.clone(),
        };
        log.append(NewEvent::new(
            "h".into(),
            p.clone(),
            Some(t.clone()),
            e.kind(),
            serde_json::to_value(&e).unwrap(),
        ))
        .await
        .unwrap();
    }
    // And a session started by a supervisor that died before recording it.
    let orphan = pty
        .create(&SessionSpec {
            task: Some(TaskId::from("t1")),
            name: "t1".into(),
            cwd: env.dir.path().into(),
            argv: vec!["sleep".into(), "30".into()],
            env: Default::default(),
            size: Default::default(),
        })
        .await
        .unwrap();

    let sup = env.supervisor(pty.clone()).await;
    assert_eq!(sup.recover().await.unwrap(), 1);
    eventually("the orphan to die", async || {
        pty.list()
            .await
            .unwrap()
            .iter()
            .all(|s| s.id != orphan.id || !s.alive)
    })
    .await;
    assert_eq!(sup.tick().await.unwrap().resumed_spawns, 1);
    let (record, worker) = sup.task(&p, &t).unwrap();
    assert_eq!(record.state, TaskState::Running);
    assert!(worker.worktree.is_some());
    assert!(matches!(sup.spawn(req).await, Err(CoreError::Invalid(_))));
}

/// A Kiro-style start: a selector dialog defaulting to "No, exit", read
/// one key at a time, before the agent's prompt.
const DIALOG_AGENT: &str = r#"#!/bin/sh
dir=$(dirname "$QUARK_STATUS_FILE")
stty -icanon -echo
sel=0
draw() {
  printf '\033[2J\033[H'
  echo "You are running in trust all tools mode."
  for i in 0 1 2; do
    case $i in 0) o="No, exit" ;; 1) o="Yes, I accept" ;; 2) o="Yes, and don't ask again" ;; esac
    if [ "$i" = "$sel" ]; then echo "  ❯ $o"; else echo "    $o"; fi
  done
  echo " ↑↓ to navigate · enter to select"
}
draw
while :; do
  k=$(dd bs=1 count=1 2>/dev/null)
  if [ -z "$k" ]; then break; fi
  if [ "$k" = "$(printf '\033')" ]; then
    rest=$(dd bs=1 count=2 2>/dev/null)
    case "$rest" in "[A") [ $sel -gt 0 ] && sel=$((sel - 1)) ;; "[B") [ $sel -lt 2 ] && sel=$((sel + 1)) ;; esac
    draw
  fi
done
echo "chose $sel" >> "$dir/received"
[ "$sel" = 1 ] || exit 9
stty icanon echo
printf '\033[2J\033[H'
echo "ready for work"
while IFS= read -r line; do
  printf '%s\n' "$line" | tr -d '\033\r' | sed 's/\[20[01]~//g' >> "$dir/received"
done
"#;

#[tokio::test]
async fn start_up_dialogs_are_answered_before_the_brief() {
    let mut env = Env::new();
    let agent = env.dir.path().join("dialog.sh");
    std::fs::write(&agent, DIALOG_AGENT).unwrap();
    let manifest = quark_harness::parse(&format!(
        r#"
schema = 1
id = "fake"
name = "Fake agent"
efforts = ["low", "high"]
[detect]
bins = ["sh"]
[launch]
argv = ["sh", "{agent}"]
prompt_via = "paste"
[[launch.dialogs]]
detect = ["running in trust all tools mode", "No, exit", "Yes, I accept", "to navigate"]
options = ["No, exit", "Yes, I accept", "Yes, and don't ask again"]
choose = "Yes, I accept"
cursor = "❯"
[turn_signals]
busy = "none"
turn_end = "none"
idle_patterns = ["ready for work"]
[keys]
interrupt = ["C-c"]
exit = "/exit"
"#,
        agent = agent.display()
    ))
    .unwrap();
    env.manifests = Arc::new(quark_harness::ManifestRegistry::with([manifest]));
    let pty = Arc::new(PtySupervisor::new());
    let sup = env.supervisor(pty.clone()).await;
    let w = sup.spawn(env.request("t1")).await.unwrap();
    let g = w.current.unwrap();
    eventually("the brief after the dialog", async || {
        let got = received(&env, "t1", &g.id);
        got.first().map(String::as_str) == Some("chose 1")
            && got.iter().any(|l| l == "Fix the parser.")
    })
    .await;
}

#[tokio::test]
async fn a_model_the_catalog_does_not_list_is_refused() {
    let mut env = Env::new();
    let mut fake = (**env.manifests.get("fake").unwrap()).clone();
    fake.models.list = vec![
        "sh".into(),
        "-c".into(),
        r#"echo '{"models":[{"model_id":"auto"}]}'"#.into(),
    ];
    env.manifests = Arc::new(quark_harness::ManifestRegistry::with([fake]));
    let pty = Arc::new(PtySupervisor::new());
    let sup = env.supervisor(pty.clone()).await;
    let mut req = env.request("t1");
    req.assignment.model = Some("gpt-9".into());
    let err = sup.spawn(req).await.unwrap_err().to_string();
    assert!(
        err.contains("does not list model gpt-9 (it lists auto)"),
        "{err}"
    );
    let (p, t) = ids("t1");
    assert_eq!(sup.task(&p, &t).unwrap().0.state, TaskState::Failed);

    let mut req = env.request("t2");
    req.assignment.model = Some("auto".into());
    sup.spawn(req).await.unwrap();
}

/// An agent that draws its idle prompt a moment before it takes input and
/// throws away what was typed before then, as Kiro does when it switches
/// its terminal into raw mode.
const LATE_READER_AGENT: &str = r#"#!/bin/sh
dir=$(dirname "$QUARK_STATUS_FILE")
echo "ready for work"
sleep 1
python3 -c 'import termios; termios.tcflush(0, termios.TCIFLUSH)'
while IFS= read -r line; do
  printf '%s\n' "$line" | tr -d '\033\r' | sed 's/\[20[01]~//g' >> "$dir/received"
done
"#;

#[tokio::test]
async fn the_brief_waits_until_the_idle_prompt_has_settled() {
    let mut env = Env::new();
    let agent = env.dir.path().join("late.sh");
    std::fs::write(&agent, LATE_READER_AGENT).unwrap();
    let manifest = quark_harness::parse(&format!(
        r#"
schema = 1
id = "fake"
name = "Fake agent"
efforts = ["low", "high"]
[detect]
bins = ["sh"]
[launch]
argv = ["sh", "{agent}"]
prompt_via = "paste"
[turn_signals]
busy = "none"
turn_end = "none"
idle_patterns = ["ready for work"]
[keys]
interrupt = ["C-c"]
exit = "/exit"
"#,
        agent = agent.display()
    ))
    .unwrap();
    env.manifests = Arc::new(quark_harness::ManifestRegistry::with([manifest]));
    let mut config = env.config();
    config.ready_settle = std::time::Duration::from_millis(1500);
    let pty = Arc::new(PtySupervisor::new());
    let sup = env.supervisor_with(pty.clone(), config).await;
    let w = sup.spawn(env.request("t1")).await.unwrap();
    let g = w.current.unwrap();
    eventually("the brief", async || {
        received(&env, "t1", &g.id)
            .iter()
            .any(|l| l == "Fix the parser.")
    })
    .await;
}
