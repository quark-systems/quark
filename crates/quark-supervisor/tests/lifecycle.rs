//! The supervisor against real pseudo-terminals, real git worktrees and the
//! SQLite event log, with a shell script standing in for the agent.

mod common;

use std::sync::Arc;

use common::{eventually, git, received, Env};
use quark_core::session::{SessionBackend, SessionSpec};
use quark_core::worktree::ReturnOutcome;
use quark_core::{CoreError, NewEvent, ProjectId, TaskEvent, TaskId, WorkerProtocol};
use quark_sessions::pty::PtySupervisor;
use quark_supervisor::{Cause, Relaunch, SupervisorEvent};
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
