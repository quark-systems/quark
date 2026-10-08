//! Accounts and pools per harness, with per-account quota (ADR-11).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use quark_systems::{AgentConfig, EventType, TaskKind, TaskState};
use quarkd::accounts::{Accounts, Holder, StubQuota};
use quarkd::api::{self, AppState};
use quarkd::engine::{EngineTask, FleetSnapshot, StubEngine, StubWrite, TaskControl};
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
    quota: Arc<StubQuota>,
    accounts: Arc<Accounts>,
    projector: Projector,
}

impl Setup {
    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    /// A config directory for another Claude account, logged in.
    fn claude_dir(&self, name: &str) -> String {
        let d = self.dir.path().join("accounts").join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(".credentials.json"), "{}").unwrap();
        d.to_string_lossy().into_owned()
    }
}

fn exe(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Claude Code installed and logged in on a fake machine; the engine
/// carries `forwarded` account variables.
fn setup_with(forwarded: &'static [&'static str]) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    let home = dir.path().join("home");
    exe(&bin.join("claude"), "echo '2.1.200 (Claude Code)'");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::write(home.join(".claude/.credentials.json"), "{}").unwrap();
    let env = HostEnv {
        path: Some(bin.into_os_string()),
        home: Some(home.clone()),
        vars: HashMap::new(),
    };
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = Arc::new(StubEngine::new());
    let harnesses = Arc::new(HarnessRegistry::new(quarkd::harness::builtin(), env));
    let quota = Arc::new(StubQuota::new());
    let accounts = Arc::new(Accounts::new(
        store.clone(),
        harnesses.clone(),
        quota.clone(),
        forwarded,
    ));
    let app = api::router(AppState {
        store: store.clone(),
        engine: engine.clone(),
        harnesses,
        accounts: accounts.clone(),
        sessions: quarkd::sessions::Sessions::disabled("not used in this test"),
        layout: Layout::new(dir.path().join("quark-home")),
        chat: Arc::new(quarkd::chat::RecordingInput::new()),
        forge: Arc::new(quarkd::forge::StubForge::new()),
        events: quark_eventlog::SqliteEventLog::open(":memory:").unwrap(),
        triggers: None,
        beads: Default::default(),
    });
    Setup {
        projector: Projector::new(store.clone(), engine.clone())
            .with_session_roots(Default::default()),
        dir,
        app,
        store,
        engine,
        quota,
        accounts,
    }
}

fn setup() -> Setup {
    setup_with(&["CLAUDE_CONFIG_DIR"])
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

async fn add(s: &Setup, body: Value) -> Value {
    let (status, account) = call(&s.app, "POST", "/v1/accounts", Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{account}");
    account
}

/// quota-axi's `--json` report with one window.
fn reading(provider: &str, remaining: f64) -> Result<String, String> {
    Ok(json!({
        "schemaVersion": 5,
        "providers": [{
            "provider": provider,
            "plan": "max",
            "windows": [{"id": "five_hour", "label": "session", "kind": "session",
                         "resetsAt": "2026-10-03T02:00:00.000Z", "percentRemaining": remaining}],
            "state": {"status": "fresh", "stale": false}
        }]
    })
    .to_string())
}

fn quota_events(store: &Store) -> Vec<Value> {
    store
        .events_after(0, 1000)
        .unwrap()
        .into_iter()
        .filter(|e| e.event_type == EventType::AccountQuotaChanged)
        .map(|e| e.payload)
        .collect()
}

#[tokio::test]
async fn lists_adds_updates_and_removes_accounts() {
    let s = setup();

    // The installed harness's default account is there from the start.
    let (status, list) = call(&s.app, "GET", "/v1/accounts", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1, "{list}");
    let default = &list[0];
    assert_eq!(default["id"], "default-claude-code");
    assert_eq!(default["harness"], "claude-code");
    assert_eq!(default["default"], true);
    assert_eq!(
        default["config_dir"],
        s.home().join(".claude").to_str().unwrap()
    );
    assert_eq!(default["health"]["state"], "configured");
    assert_eq!(default["quota"]["state"], "pending");

    let work = s.claude_dir("claude-work");
    let added = add(
        &s,
        json!({"harness": "claude-code", "config_dir": format!("{work}/"), "pools": ["max", "max"]}),
    )
    .await;
    let id = added["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("acc_"), "{id}");
    assert_eq!(added["label"], "claude-work");
    assert_eq!(added["config_dir"], work.as_str());
    assert_eq!(added["pools"], json!(["max"]));
    assert_eq!(added["health"]["state"], "configured");
    assert_eq!(added["launchable"], true);
    assert_eq!(added["active_tasks"], 0);

    // A directory without a login is not known to be logged in.
    let fresh = s.dir.path().join("accounts/fresh");
    let fresh = add(
        &s,
        json!({"harness": "claude-code", "label": "Fresh", "config_dir": fresh}),
    )
    .await;
    assert_eq!(fresh["health"]["state"], "unknown");

    let (_, list) = call(&s.app, "GET", "/v1/accounts?harness=claude-code", None).await;
    let ids: Vec<_> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "default-claude-code",
            id.as_str(),
            fresh["id"].as_str().unwrap()
        ]
    );
    let (_, none) = call(&s.app, "GET", "/v1/accounts?harness=codex", None).await;
    assert_eq!(none, json!([]));

    for (body, want) in [
        (
            json!({"harness": "claude-code", "config_dir": work}),
            StatusCode::CONFLICT,
        ),
        (
            json!({"harness": "claude-code", "config_dir": s.home().join(".claude")}),
            StatusCode::CONFLICT,
        ),
        (
            json!({"harness": "claude-code", "config_dir": "relative/dir"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"harness": "gemini", "config_dir": "/tmp/gemini"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"harness": "nope", "config_dir": "/tmp/x"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"harness": "claude-code", "config_dir": "/tmp/y", "pools": ["Not A Pool"]}),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (status, err) = call(&s.app, "POST", "/v1/accounts", Some(body.clone())).await;
        assert_eq!(status, want, "{body}: {err}");
        assert!(err["error"]["code"].is_string(), "{err}");
    }

    let (status, patched) = call(
        &s.app,
        "PATCH",
        &format!("/v1/accounts/{id}"),
        Some(json!({"label": "Work", "pools": ["max", "batch"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["label"], "Work");
    assert_eq!(patched["pools"], json!(["batch", "max"]));

    // A default account can join pools but keeps its label and stays.
    let (status, d) = call(
        &s.app,
        "PATCH",
        "/v1/accounts/default-claude-code",
        Some(json!({"pools": ["max"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{d}");
    assert_eq!(d["pools"], json!(["max"]));
    let (status, _) = call(
        &s.app,
        "PATCH",
        "/v1/accounts/default-claude-code",
        Some(json!({"label": "Mine"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(&s.app, "DELETE", "/v1/accounts/default-claude-code", None).await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, _) = call(&s.app, "DELETE", &format!("/v1/accounts/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&s.app, "GET", &format!("/v1/accounts/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&s.app, "DELETE", &format!("/v1/accounts/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn quota_is_read_once_per_account_and_changes_stream() {
    let s = setup();
    let work = s.claude_dir("claude-work");
    let default_dir = s.home().join(".claude");
    s.quota.set(&default_dir, reading("claude", 40.0));
    s.quota.set(&work, reading("claude", 90.0));
    let added = add(&s, json!({"harness": "claude-code", "config_dir": work})).await;
    let id = added["id"].as_str().unwrap().to_string();

    // Adding an account reads its quota in the background.
    for _ in 0..200 {
        if !quota_events(&s.store).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let events = quota_events(&s.store);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["account_id"], id.as_str());
    assert_eq!(events[0]["quota"]["remaining_percent"], 90.0);

    let (status, list) = call(&s.app, "GET", "/v1/accounts?refresh=true", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list[0]["quota"]["state"], "known");
    assert_eq!(list[0]["quota"]["remaining_percent"], 40.0);
    assert_eq!(list[0]["quota"]["plan"], "max");
    assert_eq!(list[0]["quota"]["windows"][0]["label"], "session");
    assert!(list[0]["quota"]["checked_at"].is_string());
    assert_eq!(list[1]["quota"]["remaining_percent"], 90.0);

    // One profile-only read per account, pointed at its own directory.
    let calls = s.quota.calls();
    let reads: Vec<_> = calls
        .iter()
        .map(|(p, v, d)| (p.as_str(), v.as_str(), d.clone()))
        .collect();
    assert_eq!(
        reads,
        [
            ("claude", "CLAUDE_CONFIG_DIR", PathBuf::from(&work)),
            ("claude", "CLAUDE_CONFIG_DIR", default_dir.clone()),
            ("claude", "CLAUDE_CONFIG_DIR", PathBuf::from(&work)),
        ]
    );
    // The default account's first reading and nothing for the unchanged one.
    assert_eq!(quota_events(&s.store).len(), 2);

    // An unchanged reading emits nothing; a change does.
    s.accounts.refresh_quota(None).await.unwrap();
    assert_eq!(quota_events(&s.store).len(), 2);
    s.quota.set(&work, reading("claude", 0.0));
    s.quota
        .set(&default_dir, Err("unknown argument: --profile-only".into()));
    s.accounts.refresh_quota(None).await.unwrap();
    let events = quota_events(&s.store);
    assert_eq!(events.len(), 4);
    assert_eq!(events[2]["account_id"], "default-claude-code");
    assert_eq!(events[2]["quota"]["state"], "error");
    assert_eq!(
        events[2]["quota"]["detail"],
        "unknown argument: --profile-only"
    );
    assert_eq!(events[3]["quota"]["remaining_percent"], 0.0);
}

fn agent(pool: Option<&str>) -> AgentConfig {
    AgentConfig {
        harness: "claude-code".into(),
        model: None,
        effort: None,
        pool: pool.map(Into::into),
    }
}

fn engine_task(id: &str, harness: &str, state: TaskState) -> EngineTask {
    EngineTask {
        id: id.into(),
        title: id.into(),
        kind: Some(TaskKind::Ship),
        state,
        state_note: None,
        state_source: None,
        harness: Some(harness.into()),
        pull_request_url: None,
        terminal: None,
        worktree: None,
    }
}

async fn project(s: &Setup, name: &str) -> String {
    let ws = s.dir.path().join("ws").join(name);
    std::fs::create_dir_all(&ws).unwrap();
    let (status, p) = call(
        &s.app,
        "POST",
        "/v1/projects",
        Some(json!({"name": name, "workspace_path": ws})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{p}");
    p["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn pools_are_balanced_across_tasks_and_sticky_within_one() {
    let s = setup();
    let a = s.claude_dir("a");
    let b = s.claude_dir("b");
    let acc_a = add(
        &s,
        json!({"harness": "claude-code", "config_dir": a, "pools": ["max"]}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let acc_b = add(
        &s,
        json!({"harness": "claude-code", "config_dir": b, "pools": ["max"]}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    // More quota left breaks a tie in running tasks.
    s.quota.set(&a, reading("claude", 20.0));
    s.quota.set(&b, reading("claude", 70.0));
    s.accounts.refresh_quota(None).await.unwrap();

    let p1 = project(&s, "one").await;
    let p2 = project(&s, "two").await;
    let p3 = project(&s, "three").await;
    let lease = |p: &str| {
        let p = p.to_string();
        let accounts = s.accounts.clone();
        async move {
            accounts
                .lease(&Holder::Coordinator(p), &agent(Some("max")))
                .await
                .unwrap()
                .unwrap()
        }
    };
    let l1 = lease(&p1).await;
    assert_eq!(l1.account_id, acc_b);
    assert_eq!(l1.env, [("CLAUDE_CONFIG_DIR".to_string(), b.clone())]);
    // Balanced: the other account is now the least busy.
    let l2 = lease(&p2).await;
    assert_eq!(l2.account_id, acc_a);
    // Sticky: the same holder keeps its account.
    assert_eq!(lease(&p1).await.account_id, acc_b);
    // A tie again goes to the account with more quota.
    assert_eq!(lease(&p3).await.account_id, acc_b);

    // An account out of quota is skipped.
    s.quota.set(&b, reading("claude", 0.0));
    s.accounts.refresh_quota(None).await.unwrap();
    let p4 = project(&s, "four").await;
    assert_eq!(lease(&p4).await.account_id, acc_a);

    // Without a pool, the default account and the ambient environment.
    let p5 = project(&s, "five").await;
    let l5 = s
        .accounts
        .lease(&Holder::Coordinator(p5), &agent(None))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(l5.account_id, "default-claude-code");
    assert!(l5.env.is_empty());

    // An empty or unknown pool is refused.
    let err = s
        .accounts
        .lease(&Holder::Coordinator(p1.clone()), &agent(Some("nope")))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("pool `nope`"), "{err}");

    let (_, list) = call(&s.app, "GET", "/v1/accounts", None).await;
    let busy: HashMap<_, _> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["id"].as_str().unwrap().to_string(),
                a["active_tasks"].clone(),
            )
        })
        .collect();
    assert_eq!(busy[&acc_a], 2);
    assert_eq!(busy[&acc_b], 2);
    assert_eq!(busy["default-claude-code"], 1);

    // An account in use cannot be removed.
    let (status, _) = call(&s.app, "DELETE", &format!("/v1/accounts/{acc_a}"), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn tasks_record_their_account_and_relaunch_under_it() {
    let s = setup();
    let work = s.claude_dir("work");
    let acc = add(
        &s,
        json!({"harness": "claude-code", "config_dir": work, "pools": ["max"]}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let pid = project(&s, "quark").await;
    // The coordinator runs under the added account.
    s.accounts
        .lease(&Holder::Coordinator(pid.clone()), &agent(Some("max")))
        .await
        .unwrap();

    // Workers it starts inherit it; a worker of another harness runs under
    // that harness's default account.
    s.engine.set_snapshot(FleetSnapshot {
        tasks: vec![
            engine_task("fix-login", "claude", TaskState::Running),
            engine_task("port-ui", "codex", TaskState::Running),
            engine_task("try-gemini", "gemini", TaskState::Running),
        ],
    });
    s.projector.refresh_all().await.unwrap();
    let (_, tasks) = call(&s.app, "GET", &format!("/v1/projects/{pid}/tasks"), None).await;
    let account_of = |engine: &str| {
        tasks
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["title"] == engine)
            .unwrap()["account_id"]
            .clone()
    };
    assert_eq!(account_of("fix-login"), acc.as_str());
    assert_eq!(account_of("port-ui"), "default-codex");
    assert_eq!(account_of("try-gemini"), Value::Null);
    let tid = tasks
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["title"] == "fix-login")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // A relaunch keeps the task's account and passes it to the engine.
    let (status, _) = call(&s.app, "POST", &format!("/v1/tasks/{tid}:relaunch"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let StubWrite::Control {
        action: TaskControl::Relaunch { account_env, .. },
        ..
    } = s.engine.writes().last().unwrap().clone()
    else {
        panic!("expected a relaunch");
    };
    assert_eq!(
        account_env,
        [("CLAUDE_CONFIG_DIR".to_string(), work.clone())]
    );

    // On another harness without a pool: that harness's default account.
    let (status, _) = call(
        &s.app,
        "POST",
        &format!("/v1/tasks/{tid}:relaunch"),
        Some(json!({"harness": "codex"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, task) = call(&s.app, "GET", &format!("/v1/tasks/{tid}"), None).await;
    assert_eq!(task["account_id"], "default-codex");
    let StubWrite::Control {
        action: TaskControl::Relaunch { account_env, .. },
        ..
    } = s.engine.writes().last().unwrap().clone()
    else {
        panic!("expected a relaunch");
    };
    assert!(account_env.is_empty());
}

#[tokio::test]
async fn harnesses_the_engine_cannot_switch_use_only_their_default() {
    // The engine carries only Claude's variable, like firstmate today.
    let s = setup_with(&["CLAUDE_CONFIG_DIR"]);
    let codex = s.dir.path().join("accounts/codex-work");
    std::fs::create_dir_all(&codex).unwrap();
    std::fs::write(codex.join("auth.json"), "{}").unwrap();
    let added = add(
        &s,
        json!({"harness": "codex", "config_dir": codex, "pools": ["codex-pool"]}),
    )
    .await;
    assert_eq!(added["launchable"], false);
    assert_eq!(added["health"]["state"], "configured");
    let pid = project(&s, "quark").await;
    let err = s
        .accounts
        .lease(
            &Holder::Coordinator(pid),
            &AgentConfig {
                harness: "codex".into(),
                model: None,
                effort: None,
                pool: Some("codex-pool".into()),
            },
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no account in pool"), "{err}");

    // A harness with no accounts at all cannot name a pool.
    let (status, v) = call(
        &s.app,
        "POST",
        "/v1/harnesses:validate",
        Some(json!({"config": {"harness": "gemini", "pool": "max"}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["errors"][0]["code"], "pool_unsupported");
}

#[tokio::test]
async fn a_project_coordinator_starts_under_its_pool() {
    let s = setup();
    let work = s.claude_dir("work");
    add(
        &s,
        json!({"harness": "claude-code", "config_dir": work, "pools": ["max"]}),
    )
    .await;

    // A pool with no accounts of the harness is refused up front.
    let (status, err) = call(
        &s.app,
        "POST",
        "/v1/projects",
        Some(json!({
            "name": "Quark",
            "repos": [{"url": "https://github.com/quark-systems/quark.git"}],
            "agent_config": {"harness": "claude-code", "pool": "batch"}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");

    let (status, p) = call(
        &s.app,
        "POST",
        "/v1/projects",
        Some(json!({
            "name": "Quark",
            "repos": [{"url": "https://github.com/quark-systems/quark.git"}],
            "agent_config": {"harness": "claude-code", "pool": "max"}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{p}");
    let id = p["id"].as_str().unwrap().to_string();
    for _ in 0..200 {
        let (_, p) = call(&s.app, "GET", &format!("/v1/projects/{id}"), None).await;
        if p["status"] != "provisioning" {
            assert_eq!(p["status"], "ready", "{p}");
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let start = s
        .engine
        .writes()
        .into_iter()
        .find_map(|w| match w {
            StubWrite::StartCoordinator { account_env, .. } => Some(account_env),
            _ => None,
        })
        .expect("coordinator started");
    // The pool's only account, carried to the engine's spawn.
    assert_eq!(start, [("CLAUDE_CONFIG_DIR".to_string(), work.clone())]);
    let (_, list) = call(&s.app, "GET", "/v1/accounts", None).await;
    assert_eq!(list[1]["active_tasks"], 1, "{list}");
}
