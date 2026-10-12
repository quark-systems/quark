//! The native engine through the adapter seam, with no firstmate: a real
//! supervisor on real pseudo-terminals and git worktrees, a shell script
//! standing in for the agent.

#[path = "../../quark-supervisor/tests/common/mod.rs"]
mod common;

use std::sync::Arc;

use common::{eventually, received, Env};
use quark_sessions::pty::PtySupervisor;
use quark_systems::TaskState;
use quarkd::engine::native::NativeEngine;
use quarkd::engine::{EngineAdapter, TaskControl, WorkspaceRef};

fn ws(env: &Env) -> WorkspaceRef {
    WorkspaceRef {
        project_id: "p".into(),
        root: env.dir.path().to_path_buf(),
    }
}

async fn state(engine: &NativeEngine, ws: &WorkspaceRef, task: &str) -> TaskState {
    engine.supervisor().tick().await.unwrap();
    engine
        .snapshot(ws)
        .await
        .unwrap()
        .tasks
        .into_iter()
        .find(|t| t.id == task)
        .map_or(TaskState::Unknown, |t| t.state)
}

#[tokio::test]
async fn reads_steers_answers_and_controls_natively() {
    let env = Env::new();
    let sup = Arc::new(env.supervisor(Arc::new(PtySupervisor::new())).await);
    let engine = NativeEngine::new(env.log(), sup.clone());
    let ws = ws(&env);

    let w = sup.spawn(env.request("t1")).await.unwrap();
    let g = w.current.clone().unwrap();
    let snap = engine.snapshot(&ws).await.unwrap();
    let t = &snap.tasks[0];
    assert_eq!((t.id.as_str(), t.state), ("t1", TaskState::Running));
    assert_eq!(t.title, "task t1");
    assert_eq!(t.harness.as_deref(), Some("fake"));
    assert_eq!(t.worktree, w.worktree.as_ref().map(|w| w.path.clone()));
    assert_eq!(
        engine.spawn(&ws, "t1").await.unwrap().unwrap().generation,
        g.id
    );

    // A question becomes a hold with the worker's wording.
    engine.send_message(&ws, "t1", "ask now").await.unwrap();
    eventually("a decision", async || {
        state(&engine, &ws, "t1").await == TaskState::NeedsDecision
    })
    .await;
    let holds = engine.holds(&ws).await.unwrap();
    assert_eq!(holds.len(), 1);
    assert_eq!(holds[0].id, "t1:k1");
    let tail = engine.status_tail(&ws, "t1", 0).await.unwrap();
    let last = tail.entries.last().unwrap();
    assert_eq!(last.kind, "needs-decision");
    assert_eq!(last.decision_key.as_deref(), Some("k1"));
    assert_eq!(holds[0].question, last.note);
    // A tail resumes after what it returned.
    let again = engine
        .status_tail(&ws, "t1", tail.next_offset)
        .await
        .unwrap();
    assert!(again.entries.is_empty());

    engine.answer(&ws, "t1:k1", "left", "matt").await.unwrap();
    assert!(engine.holds(&ws).await.unwrap().is_empty());
    eventually("the answer", async || {
        received(&env, "t1", &g.id)
            .iter()
            .any(|l| l == "Answer to [k1]: left")
    })
    .await;

    // Relaunch replaces the worker in the same worktree.
    engine
        .control(
            &ws,
            "t1",
            &TaskControl::Relaunch {
                harness: None,
                model: None,
                effort: None,
                note: "Carry on.".into(),
                account_env: Vec::new(),
            },
        )
        .await
        .unwrap();
    let g2 = engine.spawn(&ws, "t1").await.unwrap().unwrap();
    assert_ne!(g2.generation, g.id);

    engine.send_message(&ws, "t1", "ship it").await.unwrap();
    eventually("review", async || {
        state(&engine, &ws, "t1").await == TaskState::InReview
    })
    .await;
    let t = engine.snapshot(&ws).await.unwrap().tasks.remove(0);
    assert_eq!(
        t.pull_request_url.as_deref(),
        Some("https://github.com/o/r/pull/7")
    );

    engine
        .control(&ws, "t1", &TaskControl::Cancel)
        .await
        .unwrap();
    let t = engine.snapshot(&ws).await.unwrap().tasks.remove(0);
    assert_eq!(t.state, TaskState::Failed);
    assert!(t.worktree.is_some(), "cancel keeps the worktree");
}

#[tokio::test]
async fn unknown_tasks_and_unserved_operations_are_refused() {
    let env = Env::new();
    let sup = Arc::new(env.supervisor(Arc::new(PtySupervisor::new())).await);
    let engine = NativeEngine::new(env.log(), sup);
    let ws = ws(&env);
    assert!(engine.snapshot(&ws).await.unwrap().tasks.is_empty());
    assert!(engine.send_message(&ws, "nope", "hi").await.is_err());
    assert!(engine.answer(&ws, "nope:k", "x", "m").await.is_err());
    assert!(engine.answer(&ws, "no-colon", "x", "m").await.is_err());
    assert!(engine
        .merge_pull_request(&ws, "t", "https://github.com/o/r/pull/1", None)
        .await
        .is_err());
}
