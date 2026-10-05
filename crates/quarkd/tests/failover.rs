//! Failover to another pool account on rate limits (ADR-11), with a stub
//! harness that reports a rate limit the way Claude Code and Codex do: as a
//! line in the session log under the account's config directory.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use quark_systems::{EventType, TaskKind, TaskState};
use quark_transcript::SessionRoots;
use quarkd::accounts::{Accounts, StubQuota};
use quarkd::api::{self, AppState};
use quarkd::engine::{
    EngineSpawn, EngineTask, FleetSnapshot, Hold, StubEngine, StubWrite, TaskControl,
};
use quarkd::failover::Failover;
use quarkd::harness::{HarnessRegistry, HostEnv};
use quarkd::projector::Projector;
use quarkd::provision::Layout;
use quarkd::store::Store;
use serde_json::{json, Value};
use tower::ServiceExt;

struct Setup {
    dir: tempfile::TempDir,
    app: Router,
    store: Arc<Store>,
    engine: Arc<StubEngine>,
    projector: Projector,
    /// The worker's worktree.
    worktree: PathBuf,
}

/// A relaunch the engine was asked for: `(task, account_env, note)`.
type Relaunch = (String, Vec<(String, String)>, String);

fn exe(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Claude Code and Codex installed and logged in on a fake machine.
fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    let home = dir.path().join("home");
    exe(&bin.join("claude"), "echo '2.1.200 (Claude Code)'");
    exe(&bin.join("codex"), "echo 'codex-cli 0.145.0'");
    for (config, auth) in [(".claude", ".credentials.json"), (".codex", "auth.json")] {
        std::fs::create_dir_all(home.join(config)).unwrap();
        std::fs::write(home.join(config).join(auth), "{}").unwrap();
    }
    let env = HostEnv {
        path: Some(bin.into_os_string()),
        home: Some(home.clone()),
        vars: HashMap::new(),
    };
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = Arc::new(StubEngine::new());
    let harnesses = Arc::new(HarnessRegistry::new(quarkd::harness::builtin(), env));
    let accounts = Arc::new(Accounts::new(
        store.clone(),
        harnesses.clone(),
        Arc::new(StubQuota::new()),
        &["CLAUDE_CONFIG_DIR", "CODEX_HOME"],
    ));
    let app = api::router(AppState {
        store: store.clone(),
        engine: engine.clone(),
        harnesses: harnesses.clone(),
        accounts: accounts.clone(),
        sessions: quarkd::sessions::Sessions::disabled("not used in this test"),
        layout: Layout::new(dir.path().join("quark-home")),
        chat: Arc::new(quarkd::chat::RecordingInput::new()),
        forge: Arc::new(quarkd::forge::StubForge::new()),
    });
    let failover = Failover::new(store.clone(), engine.clone(), accounts, harnesses);
    // Default accounts keep their logs under the fake home; added accounts
    // under their own config directories.
    let roots = SessionRoots {
        claude: vec![home.join(".claude")],
        codex: vec![home.join(".codex")],
        pi: Vec::new(),
    };
    let worktree = dir.path().join("worktrees/fix-login");
    std::fs::create_dir_all(&worktree).unwrap();
    Setup {
        projector: Projector::new(store.clone(), engine.clone())
            .with_session_roots(roots)
            .with_failover(Arc::new(failover)),
        dir,
        app,
        store,
        engine,
        worktree,
    }
}

impl Setup {
    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    /// A logged-in config directory for another account of a harness.
    fn account_dir(&self, name: &str, auth: &str) -> String {
        let d = self.dir.path().join("accounts").join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(auth), "{}").unwrap();
        d.to_string_lossy().into_owned()
    }

    /// A Project whose agent config is `harness`, from `pool` when given.
    async fn project(&self, harness: &str, pool: Option<&str>) -> String {
        let ws = self.dir.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let (status, p) = call(
            &self.app,
            "POST",
            "/v1/projects",
            Some(json!({"name": "quark", "workspace_path": ws,
                "agent_config": {"harness": harness, "pool": pool}})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{p}");
        p["id"].as_str().unwrap().to_string()
    }

    /// The engine reports one running worker of `harness` in the worktree.
    fn worker(&self, harness: &str) {
        self.engine.set_snapshot(FleetSnapshot {
            tasks: vec![EngineTask {
                id: "fix-login".into(),
                title: "Fix login".into(),
                kind: Some(TaskKind::Ship),
                state: TaskState::Running,
                state_note: None,
                harness: Some(harness.into()),
                pull_request_url: None,
                terminal: None,
                worktree: Some(self.worktree.clone()),
            }],
        });
    }

    async fn task(&self, pid: &str) -> Value {
        let (_, tasks) = call(&self.app, "GET", &format!("/v1/projects/{pid}/tasks"), None).await;
        tasks[0].clone()
    }

    async fn open_decisions(&self) -> Vec<Value> {
        let (_, d) = call(&self.app, "GET", "/v1/decisions?state=open", None).await;
        d.as_array().unwrap().clone()
    }

    /// Every relaunch the engine was asked for.
    fn relaunches(&self) -> Vec<Relaunch> {
        self.engine
            .writes()
            .into_iter()
            .filter_map(|w| match w {
                StubWrite::Control {
                    task_id,
                    action:
                        TaskControl::Relaunch {
                            account_env, note, ..
                        },
                } => Some((task_id, account_env, note)),
                _ => None,
            })
            .collect()
    }

    /// The stub Claude Code: a session under `config_dir` in the worktree,
    /// one log per session as the real one keeps them.
    fn claude_session(&self, config_dir: &Path, session: &str, lines: &[Value]) {
        let slug: String = self
            .worktree
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let path = config_dir
            .join("projects")
            .join(slug)
            .join(format!("{session}.jsonl"));
        append(&path, lines);
    }

    /// The stub Codex: a rollout under `codex_home` for the worktree.
    fn codex_session(&self, codex_home: &Path, session: &str, lines: &[Value]) {
        let path = codex_home
            .join("sessions/2026/10/05")
            .join(format!("rollout-2026-10-05T10-00-00-{session}.jsonl"));
        if !path.exists() {
            let meta = json!({"timestamp": "2026-10-05T10:00:00.000Z", "type": "session_meta",
                "payload": {"id": session, "cwd": self.worktree, "originator": "codex_cli_rs"}});
            append(&path, &[meta]);
        }
        append(&path, lines);
    }
}

fn append(path: &Path, lines: &[Value]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    for l in lines {
        writeln!(f, "{l}").unwrap();
    }
    // Logs are told apart by modification time; keep them distinct.
    std::thread::sleep(std::time::Duration::from_millis(20));
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

async fn add(s: &Setup, body: Value) -> String {
    let (status, account) = call(&s.app, "POST", "/v1/accounts", Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{account}");
    account["id"].as_str().unwrap().to_string()
}

/// The engine's record of a worker launch; `generation` changes on relaunch.
fn spawn(generation: &str) -> EngineSpawn {
    EngineSpawn {
        generation: generation.into(),
        harness: "claude".into(),
        model: None,
        effort: None,
        spawned_at: None,
        project: None,
    }
}

fn claude_working() -> Vec<Value> {
    vec![
        json!({"type": "user", "message": {"role": "user", "content": "Fix the login bug"}}),
        json!({"type": "assistant", "message": {"role": "assistant",
            "content": [{"type": "text", "text": "Reading the handler."}]}}),
    ]
}

/// What Claude Code writes when its account is out: a synthetic assistant
/// line marked as an API error of kind `rate_limit`.
fn claude_rate_limit() -> Value {
    json!({"type": "assistant", "isApiErrorMessage": true, "error": "rate_limit",
        "message": {"model": "<synthetic>", "role": "assistant",
            "content": [{"type": "text", "text": "You've hit your session limit · resets 3pm"}]}})
}

/// What Codex writes when its account is out: a `token_count` event whose
/// `rate_limits` names the limit reached.
fn codex_rate_limit() -> Value {
    json!({"timestamp": "2099-01-01T00:00:00.000Z", "type": "event_msg",
        "payload": {"type": "token_count", "info": null, "rate_limits": {
            "limit_id": "codex",
            "primary": {"used_percent": 100.0, "window_minutes": 300, "resets_at": 1781555503},
            "secondary": {"used_percent": 40.0, "window_minutes": 10080, "resets_at": 1781802993},
            "plan_type": "plus", "rate_limit_reached_type": "rate_limit_reached"}}})
}

#[tokio::test]
async fn a_rate_limited_claude_worker_moves_through_its_pool_then_opens_a_decision() {
    let s = setup();
    let work = s.account_dir("claude-work", ".credentials.json");
    let acc_work = add(
        &s,
        json!({"harness": "claude-code", "config_dir": work, "label": "Work", "pools": ["max"]}),
    )
    .await;
    let (status, _) = call(
        &s.app,
        "PATCH",
        "/v1/accounts/default-claude-code",
        Some(json!({"pools": ["max"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let pid = s.project("claude-code", Some("max")).await;
    s.worker("claude");
    s.engine.set_spawn("fix-login", spawn("s1"));

    // Ordinary work is not a rate limit.
    s.claude_session(&s.home().join(".claude"), "s1", &claude_working());
    s.projector.refresh_all().await.unwrap();
    assert!(s.relaunches().is_empty());
    let task = s.task(&pid).await;
    let tid = task["id"].as_str().unwrap().to_string();
    assert_eq!(task["account_id"], "default-claude-code");
    assert_eq!(task["failovers"], json!([]));

    // The stub harness reports a rate limit: the worker is relaunched in
    // place under the pool's other account.
    s.claude_session(&s.home().join(".claude"), "s1", &[claude_rate_limit()]);
    s.projector.refresh_all().await.unwrap();
    let relaunches = s.relaunches();
    assert_eq!(relaunches.len(), 1, "{relaunches:?}");
    assert_eq!(relaunches[0].0, "fix-login");
    assert_eq!(
        relaunches[0].1,
        [("CLAUDE_CONFIG_DIR".to_string(), work.clone())]
    );
    assert!(
        relaunches[0].2.contains("rate limit"),
        "{}",
        relaunches[0].2
    );

    // The task records the failover, and its activity log notes it.
    let task = s.task(&pid).await;
    assert_eq!(task["account_id"], acc_work.as_str());
    assert_eq!(task["failovers"].as_array().unwrap().len(), 1, "{task}");
    let f = &task["failovers"][0];
    assert_eq!(f["from_account_id"], "default-claude-code");
    assert_eq!(f["to_account_id"], acc_work.as_str());
    assert_eq!(f["pool"], "max");
    assert_eq!(f["outcome"], "relaunched");
    assert_eq!(
        f["signal"],
        "claude: assistant isApiErrorMessage error=rate_limit"
    );
    assert_eq!(f["detail"], "You've hit your session limit · resets 3pm");
    let (_, events) = call(&s.app, "GET", &format!("/v1/tasks/{tid}/events"), None).await;
    let note = events
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "failover")
        .expect("a failover entry")["note"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        note,
        "rate limit on Claude Code account \"Default\"; relaunched under Claude Code account \"Work\""
    );
    let moved = s
        .store
        .events_after(0, 1000)
        .unwrap()
        .into_iter()
        .filter(|e| e.event_type == EventType::TaskStateChanged)
        .any(|e| e.payload["task"]["failovers"][0]["outcome"] == "relaunched");
    assert!(moved, "task.state_changed carries the failover");

    // The relaunched worker works under the new account, in a new session
    // log there. The report is not acted on twice.
    s.engine.set_spawn("fix-login", spawn("s2"));
    s.claude_session(Path::new(&work), "s2", &claude_working());
    s.projector.refresh_all().await.unwrap();
    assert_eq!(s.relaunches().len(), 1);
    assert!(s.open_decisions().await.is_empty());

    // The relaunch's dispatch record notes the failover.
    let (_, records) = call(&s.app, "GET", &format!("/v1/tasks/{tid}/dispatch"), None).await;
    let records = records.as_array().unwrap();
    assert_eq!(records.len(), 2, "{records:?}");
    assert_eq!(records[0]["failover"], Value::Null);
    assert_eq!(records[0]["chosen"]["account"], "default-claude-code");
    assert_eq!(records[1]["trigger"], "relaunch");
    assert_eq!(records[1]["chosen"]["account"], acc_work.as_str());
    assert_eq!(records[1]["failover"], task["failovers"][0]);
    assert!(
        records[1]["summary"].as_str().unwrap().ends_with(&format!(
            "Quark moved the worker from account default-claude-code to {acc_work} after a rate limit."
        )),
        "{}",
        records[1]["summary"]
    );

    // Then that account runs out too. The task never goes back to the
    // account it left, so with no healthy account left it opens a decision
    // instead of relaunching.
    s.claude_session(Path::new(&work), "s2", &[claude_rate_limit()]);
    s.projector.refresh_all().await.unwrap();
    assert_eq!(s.relaunches().len(), 1, "no relaunch without an account");
    let decisions = s.open_decisions().await;
    assert_eq!(decisions.len(), 1, "{decisions:?}");
    let decision = &decisions[0];
    assert_eq!(decision["task_id"], tid.as_str());
    let question = decision["question"].as_str().unwrap();
    assert!(
        question.starts_with(
            "Fix login: the worker hit a rate limit on Claude Code account \"Work\" and no other account in pool `max` is healthy."
        ),
        "{question}"
    );
    let task = s.task(&pid).await;
    assert_eq!(task["account_id"], acc_work.as_str());
    assert_eq!(task["failovers"][1]["outcome"], "no_healthy_account");
    assert_eq!(task["failovers"][1]["to_account_id"], Value::Null);

    // It does not loop: later refreshes, and a hold read that knows nothing
    // of the daemon's own question, leave one open decision and no relaunch.
    s.engine.set_holds(vec![Hold {
        id: "fix-login:api".into(),
        task_id: Some("fix-login".into()),
        question: "REST or gRPC?".into(),
        answer: None,
        answered_by: None,
    }]);
    s.projector.refresh_all().await.unwrap();
    s.engine.set_holds(Vec::new());
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    s.projector.refresh_all().await.unwrap();
    let decisions = s.open_decisions().await;
    assert_eq!(decisions.len(), 1, "{decisions:?}");
    assert_eq!(decisions[0]["id"], decision["id"]);
    assert_eq!(s.relaunches().len(), 1);
    assert_eq!(s.task(&pid).await["failovers"].as_array().unwrap().len(), 2);

    // Answering relaunches the worker with the answer as its note: under
    // its own account, since no other is healthy yet.
    let (status, answered) = call(
        &s.app,
        "POST",
        &format!("/v1/decisions/{}:answer", decision["id"].as_str().unwrap()),
        Some(json!({"answer": "The limit reset, carry on.", "answered_by": "matt"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answered}");
    assert_eq!(answered["state"], "answered");
    let relaunches = s.relaunches();
    assert_eq!(relaunches.len(), 2);
    assert_eq!(relaunches[1].1, [("CLAUDE_CONFIG_DIR".to_string(), work)]);
    assert!(
        relaunches[1]
            .2
            .ends_with("A person answered: The limit reset, carry on."),
        "{}",
        relaunches[1].2
    );
    // The engine was not asked to answer a hold it never had.
    assert!(!s
        .engine
        .writes()
        .iter()
        .any(|w| matches!(w, StubWrite::Answer { .. })));
    assert!(s.open_decisions().await.is_empty());
}

#[tokio::test]
async fn a_rate_limited_codex_worker_is_relaunched_under_another_codex_home() {
    let s = setup();
    let work = s.account_dir("codex-work", "auth.json");
    let acc_work = add(
        &s,
        json!({"harness": "codex", "config_dir": work, "pools": ["plus"]}),
    )
    .await;
    call(
        &s.app,
        "PATCH",
        "/v1/accounts/default-codex",
        Some(json!({"pools": ["plus"]})),
    )
    .await;
    // The Project's own agent is Claude Code, so the pool is the one the
    // Codex worker's account is in.
    let pid = s.project("claude-code", None).await;
    s.worker("codex");

    s.codex_session(&s.home().join(".codex"), "0199", &[codex_rate_limit()]);
    s.projector.refresh_all().await.unwrap();
    let relaunches = s.relaunches();
    assert_eq!(relaunches.len(), 1, "{relaunches:?}");
    assert_eq!(relaunches[0].1, [("CODEX_HOME".to_string(), work)]);
    let task = s.task(&pid).await;
    assert_eq!(task["account_id"], acc_work.as_str());
    let f = &task["failovers"][0];
    assert_eq!(f["outcome"], "relaunched");
    assert_eq!(f["from_account_id"], "default-codex");
    assert_eq!(f["pool"], Value::Null);
    assert_eq!(
        f["signal"],
        "codex: token_count rate_limits.rate_limit_reached_type"
    );
    assert_eq!(f["detail"], "rate_limit_reached");
}

#[tokio::test]
async fn a_relaunch_the_engine_refuses_opens_a_decision_and_answering_moves_the_worker() {
    let s = setup();
    let work = s.account_dir("claude-work", ".credentials.json");
    let acc_work = add(
        &s,
        json!({"harness": "claude-code", "config_dir": work, "label": "Work", "pools": ["max"]}),
    )
    .await;
    call(
        &s.app,
        "PATCH",
        "/v1/accounts/default-claude-code",
        Some(json!({"pools": ["max"]})),
    )
    .await;
    let pid = s.project("claude-code", Some("max")).await;
    s.worker("claude");

    s.engine.fail_writes(Some("the composer is not empty"));
    s.claude_session(&s.home().join(".claude"), "s1", &[claude_rate_limit()]);
    s.projector.refresh_all().await.unwrap();
    let task = s.task(&pid).await;
    assert_eq!(task["account_id"], "default-claude-code");
    assert_eq!(task["failovers"][0]["outcome"], "relaunch_failed");
    assert!(
        task["failovers"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("the composer is not empty"),
        "{task}"
    );
    let decisions = s.open_decisions().await;
    assert_eq!(decisions.len(), 1, "{decisions:?}");
    let did = decisions[0]["id"].as_str().unwrap().to_string();

    // While the engine still refuses, the answer is not recorded.
    let answer = json!({"answer": "Cleared the composer."});
    let (status, _) = call(
        &s.app,
        "POST",
        &format!("/v1/decisions/{did}:answer"),
        Some(answer.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(s.open_decisions().await.len(), 1);

    // Once it works, the worker moves to the healthy account.
    s.engine.fail_writes(None);
    let (status, _) = call(
        &s.app,
        "POST",
        &format!("/v1/decisions/{did}:answer"),
        Some(answer),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        s.relaunches()[0].1,
        [("CLAUDE_CONFIG_DIR".to_string(), work)]
    );
    let task = s.task(&pid).await;
    assert_eq!(task["account_id"], acc_work.as_str());
    assert_eq!(task["failovers"][1]["outcome"], "relaunched");
    assert_eq!(
        task["failovers"][1]["signal"],
        "quark: rate-limit decision answered"
    );
    assert!(s.open_decisions().await.is_empty());
}

#[tokio::test]
async fn a_limit_the_worker_passed_and_a_finished_task_are_left_alone() {
    let s = setup();
    let work = s.account_dir("claude-work", ".credentials.json");
    add(
        &s,
        json!({"harness": "claude-code", "config_dir": work, "pools": ["max"]}),
    )
    .await;
    call(
        &s.app,
        "PATCH",
        "/v1/accounts/default-claude-code",
        Some(json!({"pools": ["max"]})),
    )
    .await;
    let pid = s.project("claude-code", Some("max")).await;
    s.worker("claude");

    // The worker carried on after the limit (it reset): nothing to do.
    let mut lines = vec![claude_rate_limit()];
    lines.extend(claude_working());
    s.claude_session(&s.home().join(".claude"), "s1", &lines);
    s.projector.refresh_all().await.unwrap();
    assert!(s.relaunches().is_empty());
    assert_eq!(s.task(&pid).await["failovers"], json!([]));

    // A finished task's worker is not relaunched.
    s.engine.set_snapshot(FleetSnapshot {
        tasks: vec![EngineTask {
            id: "fix-login".into(),
            title: "Fix login".into(),
            kind: Some(TaskKind::Ship),
            state: TaskState::Done,
            state_note: None,
            harness: Some("claude".into()),
            pull_request_url: None,
            terminal: None,
            worktree: Some(s.worktree.clone()),
        }],
    });
    s.claude_session(&s.home().join(".claude"), "s1", &[claude_rate_limit()]);
    s.projector.refresh_all().await.unwrap();
    assert!(s.relaunches().is_empty());

    // The log is being read: once the task runs again, a new report counts.
    s.worker("claude");
    s.claude_session(&s.home().join(".claude"), "s1", &[claude_rate_limit()]);
    s.projector.refresh_all().await.unwrap();
    assert_eq!(s.relaunches().len(), 1);
}
