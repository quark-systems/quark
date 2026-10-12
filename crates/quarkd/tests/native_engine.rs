//! The native engine through the adapter seam, with no firstmate: a real
//! supervisor on real pseudo-terminals and git worktrees, a shell script
//! standing in for the agent.

#[path = "../../quark-supervisor/tests/common/mod.rs"]
mod common;

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{eventually, received, Env};
use quark_core::fake::MemoryHosts;
use quark_core::host::{Capacity, Health, Host, HostRegistry, Platform, RuntimeKind};
use quark_dispatch::classify::{Answer, Classification};
use quark_dispatch::{FixedQuota, Given, ProviderFamilies, Settings};
use quark_harness::ManifestRegistry;
use quark_sessions::pty::PtySupervisor;
use quark_systems::{
    AgentConfig, CreateProject, DeliveryPolicy, DispatchStatus, ProjectStatus, TaskState,
};
use quark_transcript::SessionRoots;
use quarkd::accounts::{Accounts, StubQuota};
use quarkd::engine::native::NativeEngine;
use quarkd::engine::native_dispatch_engine::{DispatchParts, NativeDispatch};
use quarkd::engine::native_project::ProjectSetup;
use quarkd::engine::{
    EngineAdapter, EngineError, NewTask, SourceRepo, StartOutcome, TaskControl, WorkspacePlan,
    WorkspaceRef,
};
use quarkd::harness::HarnessRegistry;
use quarkd::provision::Layout;
use quarkd::recovery::CoordinatorRecovery;
use quarkd::sessions::Sessions;
use quarkd::store::Store;

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
    // Built without dispatch, it starts nothing.
    let task = NewTask {
        title: "t".into(),
        brief: "b".into(),
        repo: None,
        agent: None,
    };
    assert!(engine.start_task(&ws, &task).await.is_err());
    assert!(engine
        .resolve_description(&ws, "b")
        .await
        .unwrap()
        .is_none());
    // Without Project setup, creating a Project is refused.
    let source = SourceRepo {
        name: "repo".into(),
        url: env.repo.display().to_string(),
    };
    assert!(engine
        .add_source(&ws.root, &source, DeliveryPolicy::Gated)
        .await
        .is_err());
    assert!(engine
        .coordinator_terminals(&ws.root)
        .await
        .unwrap()
        .is_empty());
}

const RULES: &str = r#"{"classifier":{"provider":"system1"},
  "rules":[{"when":"anything","use":{"harness":"fake","effort":"low","pricing":"budget"}}],
  "default":{"harness":"fake"}}"#;

fn one_worker_host() -> Host {
    Host {
        id: "h".into(),
        name: "h".into(),
        runtime: RuntimeKind::Local,
        platform: Platform {
            os: "linux".into(),
            arch: "x86_64".into(),
        },
        capacity: Capacity {
            cpus: 4,
            memory_bytes: 8 << 30,
            disk_bytes: 0,
            max_workers: 1,
        },
        health: Health::Healthy,
        projects: Vec::new(),
        tasks: Vec::new(),
    }
}

#[tokio::test]
async fn starts_tasks_through_native_dispatch() {
    let env = Env::new();
    let sup = Arc::new(env.supervisor(Arc::new(PtySupervisor::new())).await);
    let hosts = Arc::new(MemoryHosts::default());
    hosts.register(one_worker_host()).await.unwrap();
    let answer = Classification::Answered {
        answer: Answer {
            choice: "rule_1".into(),
            confidence: 0.9,
            probabilities: Default::default(),
            model: Some("jev-1".into()),
            latency_ms: None,
            input_tokens: None,
            output_tokens: None,
        },
    };
    let dispatch = NativeDispatch::open(
        DispatchParts {
            log: env.log(),
            host: "h".into(),
            hosts,
            classifier: Arc::new(Given(answer)),
            quota: Arc::new(FixedQuota(Some(Default::default()))),
            families: ProviderFamilies::builtin(),
            settings: Settings::default(),
        },
        sup.clone(),
    )
    .await
    .unwrap();
    let engine = NativeEngine::new(env.log(), sup.clone()).with_dispatch(dispatch.clone());
    // The Project workspace, with its one repo under repos/.
    let root = env.dir.path().join("ws");
    std::fs::create_dir_all(root.join("repos")).unwrap();
    std::os::unix::fs::symlink(&env.repo, root.join("repos/app")).unwrap();
    let ws = WorkspaceRef {
        project_id: "p".into(),
        root: root.clone(),
    };

    // The rules land in the workspace; rules that do not parse are refused.
    assert!(matches!(
        engine.set_crew_dispatch(&ws, "{").await,
        Err(EngineError::Invalid(_))
    ));
    engine.set_crew_dispatch(&ws, RULES).await.unwrap();
    assert!(root.join("config/crew-dispatch.json").is_file());
    let described = engine
        .resolve_description(&ws, "Rename a variable.")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(described.status, DispatchStatus::Clear);
    assert_eq!(described.profile.unwrap().harness, "fake");

    // The rules pick the agent and the worker starts in its own worktree.
    let fix = NewTask {
        title: "Fix the parser".into(),
        brief: "It drops trailing commas.".into(),
        repo: None,
        agent: None,
    };
    let s = engine.start_task(&ws, &fix).await.unwrap();
    assert_eq!(s.task_id, "fix-the-parser");
    assert!(
        matches!(&s.outcome, StartOutcome::Started { harness, effort, .. }
            if harness == "fake" && effort.as_deref() == Some("low")),
        "{s:?}"
    );
    let (_, w) = sup.task(&"p".into(), &"fix-the-parser".into()).unwrap();
    let wt = w.worktree.clone().unwrap();
    assert_ne!(wt.path, env.repo);
    assert_eq!(wt.branch, "quark/fix-the-parser");
    let r = engine
        .resolve_dispatch(&ws, "fix-the-parser", None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.status, DispatchStatus::Clear);
    assert_eq!(r.rule.unwrap().id, "rule_1");
    assert_eq!(r.classifier_model.as_deref(), Some("jev-1"));
    assert_eq!(r.profile.unwrap().harness, "fake");

    // The host takes one worker: the next task waits for it, and shows as
    // queued.
    let s2 = engine.start_task(&ws, &fix).await.unwrap();
    assert_eq!(s2.task_id, "fix-the-parser-2");
    let StartOutcome::Waiting { reason } = &s2.outcome else {
        panic!("{s2:?}");
    };
    assert!(reason.contains("is full"), "{reason}");
    let snap = engine.snapshot(&ws).await.unwrap();
    let queued = snap
        .tasks
        .iter()
        .find(|t| t.id == "fix-the-parser-2")
        .unwrap();
    assert_eq!(queued.state, TaskState::Queued);
    assert_eq!(queued.title, "Fix the parser");

    // A pool with no accounts escalates; the coordinator's pick goes on.
    let pooled = NewTask {
        title: "Write docs".into(),
        brief: "Document the parser.".into(),
        repo: Some("app".into()),
        agent: Some(AgentConfig {
            harness: "fake".into(),
            model: None,
            effort: None,
            pool: Some("team".into()),
        }),
    };
    let s3 = engine.start_task(&ws, &pooled).await.unwrap();
    assert!(
        matches!(&s3.outcome, StartOutcome::Escalated { reason } if reason.contains("team")),
        "{s3:?}"
    );
    let pick = AgentConfig {
        harness: "fake".into(),
        model: None,
        effort: Some("high".into()),
        pool: None,
    };
    let s3 = engine
        .choose_profile(&ws, "write-docs", &pick)
        .await
        .unwrap();
    assert!(
        matches!(&s3.outcome, StartOutcome::Waiting { .. }),
        "{s3:?}"
    );
    let r = engine
        .resolve_dispatch(&ws, "write-docs", None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.status, DispatchStatus::NotConsulted);
    assert!(engine.choose_profile(&ws, "nope", &pick).await.is_err());

    // When the first task ends, the oldest waiting one takes the host.
    engine
        .control(&ws, "fix-the-parser", &TaskControl::Cancel)
        .await
        .unwrap();
    dispatch.place().await.unwrap();
    assert!(sup.task(&"p".into(), &"fix-the-parser-2".into()).is_some());
    assert!(sup.task(&"p".into(), &"write-docs".into()).is_none());
    assert!(engine
        .start_task(
            &ws,
            &NewTask {
                repo: Some("other".into()),
                ..fix
            }
        )
        .await
        .is_err());
}

fn sha(dir: &std::path::Path, rev: &str) -> String {
    common::git(dir, &["rev-parse", rev]).trim().to_string()
}

#[tokio::test]
async fn merges_through_the_gates_and_the_guard_then_lands_the_task() {
    use quark_systems::GateState;
    use quark_verify::health::CheckRun;
    use quark_verify::{FakeForge, PullHead, PullState};

    let env = Env::new();
    let origin = env.dir.path().join("origin.git");
    common::git(
        env.dir.path(),
        &["init", "-q", "--bare", "-b", "main", "origin.git"],
    );
    common::git(
        &env.repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    common::git(&env.repo, &["push", "-q", "origin", "main"]);
    let sup = Arc::new(env.supervisor(Arc::new(PtySupervisor::new())).await);
    let forge = Arc::new(FakeForge::new());
    let engine = NativeEngine::with_forge(env.log(), sup.clone(), forge.clone());
    let ws = ws(&env);

    // Gates are fm.gates.v1, keyed by the repo's directory name.
    assert!(engine.set_gates(&ws, r#"{"schema":"x"}"#).await.is_err());
    let gates = r#"{"schema":"fm.gates.v1","repos":{"repo":{"checks":[{"name":"test","run":"test -f file.txt"}]}}}"#;
    engine.set_gates(&ws, gates).await.unwrap();
    assert!(engine.gate_evidence(&ws, "t1").await.unwrap().is_none());

    // The worker commits, pushes its branch and opens pull request 7.
    sup.spawn(env.request("t1")).await.unwrap();
    engine.send_message(&ws, "t1", "commit").await.unwrap();
    eventually("a commit", async || {
        sha(&env.repo, "quark/t1") != sha(&env.repo, "main")
    })
    .await;
    common::git(&env.repo, &["push", "-q", "origin", "quark/t1"]);
    engine.send_message(&ws, "t1", "ship it").await.unwrap();
    eventually("review", async || {
        state(&engine, &ws, "t1").await == TaskState::InReview
    })
    .await;
    let url = "https://github.com/o/r/pull/7";
    let (main, head) = (sha(&env.repo, "main"), sha(&env.repo, "quark/t1"));
    let green = || vec![CheckRun::new(1, "ci", "completed", Some("success"))];
    forge.set_tip("o/r", "main", &main);
    forge.set_checks(&main, green(), vec![]);
    forge.set_behind(&main, &head, 0);
    forge.set_pull(
        "o/r",
        7,
        PullHead {
            sha: head.clone(),
            branch: "quark/t1".into(),
            base: "main".into(),
            state: PullState::Open,
        },
    );

    // Only the pull request the task opened.
    let other = engine
        .merge_pull_request(&ws, "t1", "https://github.com/o/r/pull/8", None)
        .await;
    assert!(other.is_err());

    // Its checks are still running: refused, but the gates ran and passed.
    forge.set_checks(
        &head,
        vec![CheckRun::new(1, "ci", "in_progress", None)],
        vec![],
    );
    let refused = engine.merge_pull_request(&ws, "t1", url, None).await;
    let message = refused.unwrap_err().to_string();
    assert!(message.contains("still running"), "{message}");
    assert_eq!(state(&engine, &ws, "t1").await, TaskState::InReview);
    let e = engine.gate_evidence(&ws, "t1").await.unwrap().unwrap();
    assert_eq!(
        (e.state, e.head_sha.as_deref()),
        (GateState::Passed, Some(head.as_str()))
    );
    let artifact = e.gates[0].cases[0].artifacts[0].id.clone();
    assert!(engine
        .gate_artifact(&ws, "t1", &artifact)
        .unwrap()
        .is_file());
    assert!(engine.gate_artifact(&ws, "t1", "../evidence.json").is_err());
    assert!(engine.gate_artifact(&ws, "t1", "logs/other.log").is_err());

    // Green: it merges exactly the verified head and the task lands.
    forge.set_checks(&head, green(), vec![]);
    engine
        .merge_pull_request(&ws, "t1", url, None)
        .await
        .unwrap();
    let merged = vec![(
        "o/r".to_string(),
        7,
        head.clone(),
        quark_verify::MergeMethod::Squash,
    )];
    assert_eq!(forge.merges(), merged);
    assert_eq!(state(&engine, &ws, "t1").await, TaskState::Done);
    // Merging again is a no-op.
    engine
        .merge_pull_request(&ws, "t1", url, None)
        .await
        .unwrap();
    assert_eq!(forge.merges(), merged);

    engine
        .set_standing_approval(&ws, &["repo".to_string()], true)
        .await
        .unwrap();
    let approvals = engine.verify().standing_approvals(&ws).await.unwrap();
    assert_eq!(approvals.len(), 1);
    assert!(approvals[0].on);
}

fn tmux_installed() -> bool {
    std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Claude Code replaced by a script that logs each start, then waits.
fn fake_claude(dir: &Path) -> Arc<ManifestRegistry> {
    let script = dir.join("fake-claude");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"start $*\" | head -c 200 >> '{}'\necho >> '{0}'\nexec sleep 600\n",
            dir.join("starts").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut m = (**ManifestRegistry::builtin().get("claude-code").unwrap()).clone();
    m.launch.argv = vec![script.display().to_string()];
    Arc::new(ManifestRegistry::with([m]))
}

async fn starts(dir: &Path, want: usize) -> Vec<String> {
    for _ in 0..100 {
        let text = std::fs::read_to_string(dir.join("starts")).unwrap_or_default();
        let lines: Vec<String> = text
            .lines()
            .filter(|l| l.starts_with("start"))
            .map(str::to_string)
            .collect();
        if lines.len() >= want {
            return lines;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the coordinator did not start {want} times");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn creates_a_project_and_recovers_its_coordinator_with_no_firstmate() {
    if !tmux_installed() {
        eprintln!("tmux is not installed; skipping");
        return;
    }
    let env = Env::new();
    let sup = Arc::new(env.supervisor(Arc::new(PtySupervisor::new())).await);
    let dir = env.dir.path();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new("tmux", dir.join("run"), store.clone()).unwrap();
    let server = sessions.server().unwrap().clone();
    let layout = Layout::new(dir.join("home"));
    let setup = ProjectSetup::new(sessions.clone(), layout.clone(), "http://127.0.0.1:1")
        .with_manifests(fake_claude(dir))
        .with_roots(SessionRoots {
            claude: vec![dir.join("claude")],
            ..Default::default()
        });
    let engine = Arc::new(NativeEngine::new(env.log(), sup).with_project_setup(Arc::new(setup)));
    let agent = AgentConfig {
        harness: "claude-code".into(),
        model: None,
        effort: None,
        pool: None,
    };
    let project = store
        .create_project(CreateProject {
            name: "p".into(),
            agent_config: Some(agent.clone()),
            ..Default::default()
        })
        .unwrap();
    let command = layout.command_workspace();
    let source = SourceRepo {
        name: "repo".into(),
        url: env.repo.display().to_string(),
    };

    // Provisioning's steps, through the adapter.
    engine
        .add_source(&command, &source, DeliveryPolicy::Gated)
        .await
        .unwrap();
    let plan = WorkspacePlan {
        project_id: project.id.clone(),
        name: "p".into(),
        goal: None,
        sources: vec![source],
        root: layout.workspace(&project.id),
        user_memory: None,
    };
    let root = engine.seed_workspace(&command, &plan).await.unwrap();
    assert!(root.join("repos/repo/README").is_file());
    let ws = WorkspaceRef {
        project_id: project.id.clone(),
        root: root.clone(),
    };
    engine
        .start_coordinator(&command, &ws, &agent, &[], false)
        .await
        .unwrap();
    assert_eq!(starts(dir, 1).await.len(), 1);
    let target = format!("quark:coordinator-{}", project.id);
    assert_eq!(
        engine.coordinator_terminals(&command).await.unwrap(),
        HashMap::from([(project.id.clone(), target.clone())])
    );
    store
        .set_project_status(
            &project.id,
            ProjectStatus::Ready,
            None,
            Some(&root.display().to_string()),
            None,
        )
        .unwrap();

    // Recovery leaves a running coordinator alone, and starts a lost one
    // again: fresh, since it never logged a conversation to resume.
    let accounts = Arc::new(Accounts::new(
        store.clone(),
        Arc::new(HarnessRegistry::builtin()),
        Arc::new(StubQuota::new()),
        engine.account_envs(),
    ));
    let recovery = CoordinatorRecovery::new(
        store.clone(),
        engine.clone(),
        accounts,
        sessions.clone(),
        command.clone(),
        layout.home.clone(),
    );
    recovery.pass().await;
    assert_eq!(starts(dir, 1).await.len(), 1);
    server.kill().await;
    recovery.pass().await;
    let all = starts(dir, 2).await;
    assert!(!all[1].contains("--continue"), "{}", all[1]);
    assert!(server
        .list_panes()
        .await
        .unwrap()
        .iter()
        .any(|p| p.target == target));

    sessions.detach_all();
    server.kill().await;
}
