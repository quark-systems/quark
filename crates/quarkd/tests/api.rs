use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use futures_util::StreamExt;
use http_body_util::BodyExt;
use quark_systems::{
    DispatchCandidate, DispatchChoice, DispatchRule, DispatchStatus, Event, EventType, TaskKind,
    TaskState,
};
use quarkd::api::{self, ApiDoc, AppState};
use quarkd::chat::RecordingInput;
use quarkd::engine::{
    EngineResolution, EngineSpawn, EngineTask, FleetSnapshot, Hold, StubEngine, StubWrite,
    TaskControl,
};
use quarkd::harness::{HarnessRegistry, HostEnv};
use quarkd::projector::Projector;
use quarkd::provision::Layout;
use quarkd::store::Store;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

struct Harness {
    _home: tempfile::TempDir,
    app: Router,
    addr: std::net::SocketAddr,
    engine: Arc<StubEngine>,
    projector: Projector,
}

async fn harness() -> Harness {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = Arc::new(StubEngine::new());
    let chat = Arc::new(RecordingInput::new());
    let home = tempfile::tempdir().unwrap();
    let harnesses = Arc::new(HarnessRegistry::new(
        quarkd::harness::builtin(),
        HostEnv::default(),
    ));
    let app = api::router(AppState {
        store: store.clone(),
        engine: engine.clone(),
        harnesses: harnesses.clone(),
        accounts: Arc::new(quarkd::accounts::Accounts::new(
            store.clone(),
            harnesses,
            Arc::new(quarkd::accounts::StubQuota::new()),
            &["CLAUDE_CONFIG_DIR"],
        )),
        sessions: quarkd::sessions::Sessions::disabled("not used in this test"),
        layout: Layout::new(home.path()),
        chat,
        forge: Arc::new(quarkd::forge::StubForge::new()),
        events: quark_eventlog::SqliteEventLog::open(":memory:").unwrap(),
        triggers: None,
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = app.clone();
    tokio::spawn(async move { axum::serve(listener, served).await.unwrap() });
    Harness {
        _home: home,
        app,
        addr,
        projector: Projector::new(store, engine.clone()).with_session_roots(Default::default()),
        engine,
    }
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

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(addr: std::net::SocketAddr, query: &str) -> Ws {
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/v1/events{query}"))
        .await
        .unwrap();
    ws
}

async fn next_event(ws: &mut Ws) -> Event {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("event within 5s")
            .unwrap()
            .unwrap();
        if let Message::Text(t) = msg {
            return serde_json::from_str(&t).unwrap();
        }
    }
}

async fn assert_quiet(ws: &mut Ws) {
    let r = tokio::time::timeout(Duration::from_millis(200), ws.next()).await;
    assert!(r.is_err(), "expected no event, got {r:?}");
}

fn task(state: TaskState) -> EngineTask {
    EngineTask {
        id: "fix-login".into(),
        title: "Fix login redirect".into(),
        kind: Some(TaskKind::Ship),
        state,
        state_note: None,
        state_source: None,
        harness: Some("claude".into()),
        pull_request_url: None,
        worktree: None,
        terminal: None,
    }
}

#[tokio::test]
async fn projects_tasks_decisions_and_event_replay() {
    let h = harness().await;

    let (status, health) = call(&h.app, "GET", "/v1/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(health["engine"], "stub");
    assert_eq!(health["last_seq"], 0);

    // A live-only subscriber (no cursor) sees events from now on.
    let mut live = connect(h.addr, "").await;

    let (status, project) = call(
        &h.app,
        "POST",
        "/v1/projects",
        Some(json!({"name": "Quark", "workspace_path": "/tmp/quark-ws"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let pid = project["id"].as_str().unwrap().to_string();

    let e = next_event(&mut live).await;
    assert_eq!((e.seq, e.event_type), (1, EventType::ProjectUpdated));
    assert_eq!(e.project_id.as_deref(), Some(pid.as_str()));

    h.engine.set_snapshot(FleetSnapshot {
        tasks: vec![task(TaskState::Running)],
    });
    h.projector.refresh_all().await.unwrap();
    let e = next_event(&mut live).await;
    assert_eq!((e.seq, e.event_type), (2, EventType::TaskCreated));
    let tid = e.payload["id"].as_str().unwrap().to_string();

    // A reconnecting client replays everything after its cursor.
    let mut replay = connect(h.addr, "?cursor=1").await;
    assert_eq!(next_event(&mut replay).await.seq, 2);
    assert_quiet(&mut replay).await;

    h.engine.set_snapshot(FleetSnapshot {
        tasks: vec![task(TaskState::NeedsDecision)],
    });
    h.engine.set_holds(vec![Hold {
        id: "hold-1".into(),
        task_id: Some("fix-login".into()),
        question: "Ship behind a flag?".into(),
        answer: None,
        answered_by: None,
    }]);
    h.projector.refresh_all().await.unwrap();
    // Nothing changed upstream: a second refresh emits nothing.
    h.projector.refresh_all().await.unwrap();

    for ws in [&mut live, &mut replay] {
        let e = next_event(ws).await;
        assert_eq!((e.seq, e.event_type), (3, EventType::TaskStateChanged));
        assert_eq!(e.payload["previous_state"], "running");
        let e = next_event(ws).await;
        assert_eq!((e.seq, e.event_type), (4, EventType::DecisionOpened));
        assert_eq!(e.payload["task_id"], tid.as_str());
        assert_quiet(ws).await;
    }

    let mut full = connect(h.addr, "?cursor=0").await;
    for seq in 1..=4 {
        assert_eq!(next_event(&mut full).await.seq, seq);
    }

    let (status, tasks) = call(&h.app, "GET", &format!("/v1/projects/{pid}/tasks"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(tasks.as_array().unwrap().len(), 1);
    assert_eq!(tasks[0]["state"], "needs_decision");

    let (status, t) = call(&h.app, "GET", &format!("/v1/tasks/{tid}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(t["title"], "Fix login redirect");

    let (_, open) = call(&h.app, "GET", "/v1/decisions?state=open", None).await;
    assert_eq!(open.as_array().unwrap().len(), 1);
    let (_, answered) = call(&h.app, "GET", "/v1/decisions?state=answered", None).await;
    assert!(answered.as_array().unwrap().is_empty());

    let (status, patched) = call(
        &h.app,
        "PATCH",
        &format!("/v1/projects/{pid}"),
        Some(json!({"goal": "Ship the MVP"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(patched["goal"], "Ship the MVP");
    assert_eq!(patched["name"], "Quark");
}

/// A Project with `workspace_path` and one projected task; returns the task id.
async fn project_with_task(h: &Harness, workspace_path: Option<&str>) -> String {
    let (status, _) = call(
        &h.app,
        "POST",
        "/v1/projects",
        Some(json!({"name": "Quark", "workspace_path": workspace_path})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    h.engine.set_snapshot(FleetSnapshot {
        tasks: vec![task(TaskState::Running)],
    });
    if workspace_path.is_none() {
        return String::new();
    }
    h.projector.refresh_all().await.unwrap();
    let (_, tasks) = call(&h.app, "GET", "/v1/projects", None).await;
    let pid = tasks[0]["id"].as_str().unwrap();
    let (_, tasks) = call(&h.app, "GET", &format!("/v1/projects/{pid}/tasks"), None).await;
    tasks[0]["id"].as_str().unwrap().to_string()
}

fn spawn_now(generation: &str, harness: &str, model: Option<&str>) -> EngineSpawn {
    EngineSpawn {
        generation: generation.into(),
        harness: harness.into(),
        model: model.map(str::to_string),
        effort: Some("high".into()),
        spawned_at: Some(time::OffsetDateTime::now_utc().unix_timestamp()),
        project: Some("quark".into()),
    }
}

#[tokio::test]
async fn each_spawn_records_why_this_agent() {
    let h = harness().await;
    let mut live = connect(h.addr, "").await;
    h.engine
        .set_spawn("fix-login", spawn_now("s1", "claude", Some("sonnet")));
    h.engine.set_resolution(
        "fix-login",
        Ok(EngineResolution {
            status: DispatchStatus::Clear,
            rule: Some(DispatchRule {
                id: "rule_1".into(),
                when: Some("A trivial mechanical edit.".into()),
            }),
            reason: None,
            notes: vec!["rule matched".into()],
            candidates: vec![
                DispatchCandidate {
                    harness: "claude".into(),
                    model: Some("sonnet".into()),
                    passed: true,
                    reason: "eligible".into(),
                    evidence: Some("provider=claude remaining=79%".into()),
                },
                DispatchCandidate {
                    harness: "codex".into(),
                    model: Some("gpt-5.6-luna".into()),
                    passed: false,
                    reason: "0% remaining at all_models".into(),
                    evidence: None,
                },
            ],
            profile: Some(DispatchChoice {
                harness: "claude".into(),
                model: Some("sonnet".into()),
                effort: Some("high".into()),
                account: None,
            }),
            fallback: None,
            classifier_consulted: true,
            classifier_model: Some("jev-1.13.0".into()),
            confidence: Some(0.93),
            output: Some("dispatch-resolve:\n  status: clear".into()),
        }),
    );
    let tid = project_with_task(&h, Some("/tmp/quark-ws")).await;

    let e = next_event(&mut live).await;
    assert_eq!(e.event_type, EventType::ProjectUpdated);
    assert_eq!(
        next_event(&mut live).await.event_type,
        EventType::TaskCreated
    );
    let e = next_event(&mut live).await;
    assert_eq!(e.event_type, EventType::DispatchRecorded);
    assert_eq!(e.payload["task_id"], tid.as_str());
    assert_eq!(e.payload["decided_by"], "classifier");
    assert_eq!(e.payload["rule"]["id"], "rule_1");
    assert_eq!(e.payload["classifier"]["provider"], "system1");
    assert_eq!(e.payload["classifier"]["confidence"], 0.93);
    assert_eq!(e.payload["chosen"]["harness"], "claude");
    assert_eq!(e.payload["chosen"]["effort"], "high");
    // The account the task runs under (quark#23) is the one recorded.
    let (_, task) = call(&h.app, "GET", &format!("/v1/tasks/{tid}"), None).await;
    assert!(task["account_id"].is_string());
    assert_eq!(e.payload["chosen"]["account"], task["account_id"]);
    assert_eq!(e.payload["candidates"][1]["passed"], false);
    assert_eq!(
        e.payload["candidates"][1]["reason"],
        "0% remaining at all_models"
    );
    // The model the worker was started with is on its task.
    let changed = next_event(&mut live).await;
    assert_eq!(changed.event_type, EventType::TaskStateChanged);
    assert_eq!(changed.payload["task"]["model"], "sonnet");

    // The same spawn is recorded once, and the resolution ran once.
    h.projector.refresh_all().await.unwrap();
    assert_quiet(&mut live).await;
    assert_eq!(h.engine.resolved(), ["fix-login"]);

    let (status, records) = call(&h.app, "GET", &format!("/v1/tasks/{tid}/dispatch"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(records.as_array().unwrap().len(), 1);
    assert_eq!(records[0], e.payload);

    // A relaunch is a new spawn: recorded without running the resolution.
    h.engine
        .set_spawn("fix-login", spawn_now("s2", "codex", None));
    h.projector.refresh_all().await.unwrap();
    let e = next_event(&mut live).await;
    assert_eq!(e.event_type, EventType::DispatchRecorded);
    assert_eq!(e.payload["trigger"], "relaunch");
    assert_eq!(e.payload["decided_by"], "relaunch");
    assert_eq!(e.payload["resolution"]["status"], "not_consulted");
    assert_eq!(e.payload["classifier"]["provider"], "none");
    assert_eq!(h.engine.resolved(), ["fix-login"]);

    // Records outlive the task: gone from the engine, still served.
    h.engine.set_snapshot(FleetSnapshot { tasks: vec![] });
    h.projector.refresh_all().await.unwrap();
    let (_, records) = call(&h.app, "GET", &format!("/v1/tasks/{tid}/dispatch"), None).await;
    assert_eq!(records.as_array().unwrap().len(), 2);
    assert_eq!(records[0]["trigger"], "spawn");
    assert_eq!(records[1]["chosen"]["harness"], "codex");

    let (status, body) = call(&h.app, "GET", "/v1/tasks/tsk_nope/dispatch", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");
}

#[tokio::test]
async fn without_a_classifier_the_coordinator_picked() {
    let h = harness().await;
    h.engine
        .set_spawn("fix-login", spawn_now("s1", "claude", None));
    h.engine.set_resolution(
        "fix-login",
        Ok(EngineResolution {
            status: DispatchStatus::Off,
            rule: None,
            reason: None,
            notes: vec![],
            candidates: vec![],
            profile: None,
            fallback: None,
            classifier_consulted: false,
            classifier_model: None,
            confidence: None,
            output: None,
        }),
    );
    let tid = project_with_task(&h, Some("/tmp/quark-ws")).await;
    let (_, records) = call(&h.app, "GET", &format!("/v1/tasks/{tid}/dispatch"), None).await;
    let r = &records[0];
    assert_eq!(r["decided_by"], "coordinator");
    assert_eq!(r["resolution"]["status"], "off");
    assert_eq!(r["classifier"]["provider"], "none");
    assert_eq!(r["classifier"]["model"], Value::Null);
    assert_eq!(r["classifier"]["confidence"], Value::Null);
    assert!(r["summary"]
        .as_str()
        .unwrap()
        .contains("coordinator picked claude"));

    // A worker older than the freshness window is recorded without resolving.
    let h = harness().await;
    let mut old = spawn_now("s1", "claude", None);
    old.spawned_at = Some(old.spawned_at.unwrap() - 3600);
    h.engine.set_spawn("fix-login", old);
    let tid = project_with_task(&h, Some("/tmp/quark-ws")).await;
    let (_, records) = call(&h.app, "GET", &format!("/v1/tasks/{tid}/dispatch"), None).await;
    assert_eq!(records[0]["resolution"]["status"], "not_consulted");
    assert!(h.engine.resolved().is_empty());

    // A failing resolution is recorded as an error, not retried.
    let h = harness().await;
    h.engine
        .set_spawn("fix-login", spawn_now("s1", "claude", None));
    h.engine
        .set_resolution("fix-login", Err("quota-axi --json failed".into()));
    let tid = project_with_task(&h, Some("/tmp/quark-ws")).await;
    h.projector.refresh_all().await.unwrap();
    let (_, records) = call(&h.app, "GET", &format!("/v1/tasks/{tid}/dispatch"), None).await;
    assert_eq!(records.as_array().unwrap().len(), 1);
    assert_eq!(records[0]["resolution"]["status"], "error");
    assert_eq!(h.engine.resolved().len(), 1);
}

#[tokio::test]
async fn steer_cancel_and_relaunch_reach_the_engine_task() {
    let h = harness().await;
    let tid = project_with_task(&h, Some("/tmp/quark-ws")).await;

    let (status, _) = call(
        &h.app,
        "POST",
        &format!("/v1/tasks/{tid}/messages"),
        Some(json!({"text": "also cover the empty-input case"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = call(&h.app, "POST", &format!("/v1/tasks/{tid}:cancel"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // No body: keep the harness, use the default note.
    let (status, _) = call(&h.app, "POST", &format!("/v1/tasks/{tid}:relaunch"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(
        &h.app,
        "POST",
        &format!("/v1/tasks/{tid}:relaunch"),
        Some(json!({"harness": "codex", "note": "tests pass; open the PR"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let writes = h.engine.writes();
    assert_eq!(writes.len(), 4);
    // The engine sees its own task id, never the API id.
    assert_eq!(
        writes[0],
        StubWrite::Message {
            task_id: "fix-login".into(),
            text: "also cover the empty-input case".into()
        }
    );
    assert_eq!(
        writes[1],
        StubWrite::Control {
            task_id: "fix-login".into(),
            action: TaskControl::Cancel
        }
    );
    let StubWrite::Control {
        action: TaskControl::Relaunch { harness, note, .. },
        ..
    } = &writes[2]
    else {
        panic!("expected a relaunch, got {:?}", writes[2]);
    };
    assert_eq!(harness, &None);
    assert!(!note.trim().is_empty());
    let StubWrite::Control {
        action: TaskControl::Relaunch { harness, note, .. },
        ..
    } = &writes[3]
    else {
        panic!("expected a relaunch, got {:?}", writes[3]);
    };
    assert_eq!(harness.as_deref(), Some("codex"));
    assert_eq!(note, "tests pass; open the PR");
}

#[tokio::test]
async fn answering_a_decision_reaches_the_engine_and_records_who() {
    let h = harness().await;
    let tid = project_with_task(&h, Some("/tmp/quark-ws")).await;
    h.engine.set_holds(vec![Hold {
        id: "fix-login:flag".into(),
        task_id: Some("fix-login".into()),
        question: "Ship behind a flag?".into(),
        answer: None,
        answered_by: None,
    }]);
    h.projector.refresh_all().await.unwrap();
    let (_, open) = call(&h.app, "GET", "/v1/decisions?state=open", None).await;
    let did = open[0]["id"].as_str().unwrap().to_string();
    assert_eq!(open[0]["task_id"], tid.as_str());
    let mut live = connect(h.addr, &format!("?cursor={}", 0)).await;

    for (body, want) in [
        (
            json!({"answer": "  ", "answered_by": "matt"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"answer": "yes", "answered_by": "a\nb"}),
            StatusCode::BAD_REQUEST,
        ),
        (json!({"answered_by": "matt"}), StatusCode::BAD_REQUEST),
    ] {
        let (status, _) = call(
            &h.app,
            "POST",
            &format!("/v1/decisions/{did}:answer"),
            Some(body),
        )
        .await;
        assert_eq!(status, want);
    }
    let (status, _) = call(
        &h.app,
        "POST",
        "/v1/decisions/nope:answer",
        Some(json!({"answer": "yes", "answered_by": "matt"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(h.engine.writes().is_empty());

    let (status, d) = call(
        &h.app,
        "POST",
        &format!("/v1/decisions/{did}:answer"),
        Some(json!({"answer": "Yes, behind a flag", "answered_by": "matt"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(d["state"], "answered");
    assert_eq!(d["answer"], "Yes, behind a flag");
    assert_eq!(d["answered_by"], "matt");
    assert!(d["answered_at"].is_string());
    assert_eq!(
        h.engine.writes(),
        vec![StubWrite::Answer {
            hold_id: "fix-login:flag".into(),
            answer: "Yes, behind a flag".into(),
            answered_by: "matt".into(),
        }]
    );

    let answered = loop {
        let e = next_event(&mut live).await;
        if e.event_type == EventType::DecisionAnswered {
            break e;
        }
    };
    assert_eq!(answered.payload["id"], did.as_str());
    assert_eq!(answered.payload["answered_by"], "matt");

    // The engine dropped the hold; refreshing keeps the recorded answer.
    h.projector.refresh_all().await.unwrap();
    assert_quiet(&mut live).await;
    let (_, all) = call(&h.app, "GET", "/v1/decisions?state=answered", None).await;
    assert_eq!(all[0]["answered_by"], "matt");

    let (status, err) = call(
        &h.app,
        "POST",
        &format!("/v1/decisions/{did}:answer"),
        Some(json!({"answer": "no", "answered_by": "ana"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(err["error"]["code"], "already_answered");
    let (status, _) = call(&h.app, "POST", &format!("/v1/decisions/{did}"), None).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);

    // Without answered_by, the daemon's own user answers.
    h.engine.set_holds(vec![Hold {
        id: "fix-login:copy".into(),
        task_id: Some("fix-login".into()),
        question: "Which wording?".into(),
        answer: None,
        answered_by: None,
    }]);
    h.projector.refresh_all().await.unwrap();
    let (_, open) = call(&h.app, "GET", "/v1/decisions?state=open", None).await;
    let did = open[0]["id"].as_str().unwrap().to_string();
    let (status, d) = call(
        &h.app,
        "POST",
        &format!("/v1/decisions/{did}:answer"),
        Some(json!({"answer": "the short one", "answered_by": null})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!d["answered_by"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn task_writes_report_errors() {
    let h = harness().await;
    let tid = project_with_task(&h, Some("/tmp/quark-ws")).await;

    let (status, body) = call(
        &h.app,
        "POST",
        &format!("/v1/tasks/{tid}/messages"),
        Some(json!({"text": "  "})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_request");

    let (status, _) = call(&h.app, "POST", "/v1/tasks/nope:cancel", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    h.engine.fail_writes(Some("endpoint is gone"));
    let (status, body) = call(&h.app, "POST", &format!("/v1/tasks/{tid}:cancel"), None).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"]["code"], "engine_failed");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("endpoint is gone"));
    assert!(h.engine.writes().is_empty());
}

#[tokio::test]
async fn errors_use_the_error_body() {
    let h = harness().await;
    let (status, body) = call(&h.app, "GET", "/v1/tasks/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");

    let (status, body) = call(&h.app, "POST", "/v1/projects", Some(json!({"name": "  "}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_request");

    let (status, _) = call(&h.app, "GET", "/v1/projects/nope/tasks", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// The committed OpenAPI document is the frozen contract. Regenerate it with
/// `cargo run -p quarkd -- openapi > api/openapi.json` after an API change.
#[tokio::test]
async fn committed_openapi_matches() {
    let committed = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../api/openapi.json"
    ))
    .unwrap();
    assert_eq!(
        committed,
        ApiDoc::json(),
        "api/openapi.json is stale; run `cargo run -p quarkd -- openapi > api/openapi.json`"
    );

    let h = harness().await;
    let (status, served) = call(&h.app, "GET", "/v1/openapi.json", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(served, serde_json::from_str::<Value>(&committed).unwrap());
}

#[tokio::test]
async fn terminal_routes_without_tmux() {
    let h = harness().await;
    let (status, project) = call(&h.app, "POST", "/v1/projects", Some(json!({"name": "p"}))).await;
    assert_eq!(status, StatusCode::CREATED);
    let id = project["id"].as_str().unwrap();

    let (status, list) = call(&h.app, "GET", &format!("/v1/projects/{id}/terminals"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list, json!([]));
    let (status, _) = call(&h.app, "GET", "/v1/projects/nope/terminals", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, body) = call(&h.app, "GET", "/v1/terminals/tsk_x", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "unavailable");

    let (status, body) = call(
        &h.app,
        "POST",
        "/v1/terminals/tsk_x/input",
        Some(json!({"data_b64": "not base64!"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_request");
}

/// Polls the Project until it leaves `provisioning`.
async fn settled(h: &Harness, id: &str) -> Value {
    for _ in 0..200 {
        let (_, p) = call(&h.app, "GET", &format!("/v1/projects/{id}"), None).await;
        if p["status"] != "provisioning" {
            return p;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("Project {id} still provisioning");
}

fn git_show(repo: &str, spec: &str) -> String {
    let out = std::process::Command::new("git")
        .args(["-C", repo, "show", spec])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[tokio::test]
async fn creating_a_project_provisions_workspace_repo_and_coordinator() {
    let h = harness().await;
    let mut ws = connect(h.addr, "").await;
    let (status, created) = call(
        &h.app,
        "POST",
        "/v1/projects",
        Some(json!({
            "name": "Quark MVP",
            "goal": "Build the MVP",
            "repos": [
                {"url": "https://github.com/quark-systems/quark.git"},
                {"url": "git@github.com:quark-systems/firstmate.git", "name": "engine"}
            ],
            "agent_config": {"harness": "claude-code", "model": "claude-sonnet-5", "effort": "high"},
            "dispatch_preset": "light_trivial",
            "delivery": "direct"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["status"], "provisioning");
    assert_eq!(created["repos"][0]["name"], "quark");
    let id = created["id"].as_str().unwrap().to_string();

    let p = settled(&h, &id).await;
    assert_eq!(p["status"], "ready", "{p}");
    assert_eq!(p["status_detail"], Value::Null);
    let home = h._home.path();
    let workspace = home.join("workspaces").join(&id);
    assert_eq!(p["workspace_path"], workspace.to_str().unwrap());
    let repo = home.join("projects").join(format!("{id}.git"));
    assert_eq!(p["project_repo_path"], repo.to_str().unwrap());

    assert_eq!(
        h.engine.writes(),
        [
            StubWrite::AddSource {
                name: "quark".into(),
                url: "https://github.com/quark-systems/quark.git".into()
            },
            StubWrite::AddSource {
                name: "engine".into(),
                url: "git@github.com:quark-systems/firstmate.git".into()
            },
            StubWrite::SeedWorkspace {
                project_id: id.clone(),
                sources: vec!["quark".into(), "engine".into()]
            },
            StubWrite::StartCoordinator {
                project_id: id.clone(),
                harness: "claude-code".into(),
                account_env: Vec::new(),
                resume: false,
            },
        ]
    );

    let repo = repo.to_str().unwrap();
    let project_yaml = git_show(repo, "main:project.yaml");
    assert!(
        project_yaml.contains(&format!("id: \"{id}\"")),
        "{project_yaml}"
    );
    assert!(project_yaml.contains("goal: \"Build the MVP\""));
    assert!(project_yaml.contains("policy: \"direct\""));
    assert!(git_show(repo, "main:dispatch.yaml").contains("trivial-edit"));
    assert!(git_show(repo, "main:instructions.md").starts_with("# Quark MVP\n"));
    git_show(repo, "main:memory/.gitkeep");
    assert!(workspace.join("project/instructions.md").is_file());

    // Every step is announced, ending with the ready Project.
    let mut details = Vec::new();
    loop {
        let e = next_event(&mut ws).await;
        assert_eq!(e.event_type, EventType::ProjectUpdated);
        details.push(
            e.payload["status_detail"]
                .as_str()
                .unwrap_or("")
                .to_string(),
        );
        if e.payload["status"] == "ready" {
            break;
        }
    }
    assert_eq!(
        details,
        [
            "",
            "Cloning quark",
            "Cloning engine",
            "Seeding the Project workspace",
            "Writing the Project repo",
            "Starting the coordinator",
            ""
        ]
    );

    // The new workspace is now refreshed like any attached one.
    h.engine.set_snapshot(FleetSnapshot {
        tasks: vec![EngineTask {
            id: "fix-42".into(),
            title: "Fix #42".into(),
            kind: Some(TaskKind::Ship),
            state: TaskState::Running,
            state_note: None,
            state_source: None,
            harness: Some("claude".into()),
            pull_request_url: None,
            worktree: None,
            terminal: None,
        }],
    });
    h.projector.refresh_all().await.unwrap();
    let (_, tasks) = call(&h.app, "GET", &format!("/v1/projects/{id}/tasks"), None).await;
    assert_eq!(tasks[0]["title"], "Fix #42");

    // The refresh handed the engine the Project repo's (empty) gate config.
    let gates = |h: &Harness| -> Vec<Value> {
        h.engine
            .writes()
            .into_iter()
            .filter_map(|w| match w {
                StubWrite::Gates { config, .. } => Some(serde_json::from_str(&config).unwrap()),
                _ => None,
            })
            .collect()
    };
    assert_eq!(gates(&h), [json!({"schema": "fm.gates.v1", "repos": {}})]);

    // Declaring gates and adding holdout tests in the Project repo reaches
    // the engine on the next refresh, once.
    let checkout = workspace.join("project");
    let yaml = std::fs::read_to_string(checkout.join("project.yaml")).unwrap();
    let yaml = yaml.replacen(
        "      url: \"https://github.com/quark-systems/quark.git\"\n",
        "      url: \"https://github.com/quark-systems/quark.git\"\n      verification:\n        checks:\n          - { name: \"test\", run: \"cargo test\" }\n",
        1,
    );
    std::fs::write(checkout.join("project.yaml"), yaml).unwrap();
    std::fs::create_dir_all(checkout.join("holdout/quark/api")).unwrap();
    std::fs::write(checkout.join("holdout/quark/api/run"), "#!/bin/sh\n").unwrap();
    for args in [
        &["add", "-A"][..],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@x",
            "commit",
            "-qm",
            "gates",
        ],
        &["push", "-q", "origin", "HEAD:main"],
    ] {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }
    h.projector.refresh_all().await.unwrap();
    h.projector.refresh_all().await.unwrap();
    let all = gates(&h);
    assert_eq!(all.len(), 2, "{all:?}");
    let quark = &all[1]["repos"]["quark"];
    assert_eq!(quark["checks"][0]["run"], "cargo test");
    assert_eq!(quark["holdout"]["path"], "holdout/quark");
    assert_eq!(quark["holdout"]["repo"], repo);
    assert!(all[1]["repos"].get("engine").is_none());
}

#[tokio::test]
async fn failed_provisioning_reports_the_step_and_can_be_retried() {
    let h = harness().await;
    h.engine.fail_writes(Some("could not resolve host"));
    let (_, created) = call(
        &h.app,
        "POST",
        "/v1/projects",
        Some(json!({
            "name": "P",
            "repos": [{"url": "https://example.com/r.git"}],
            "agent_config": {"harness": "codex"}
        })),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();
    let p = settled(&h, &id).await;
    assert_eq!(p["status"], "failed");
    assert_eq!(
        p["status_detail"],
        "Cloning r: engine command failed: could not resolve host"
    );
    assert_eq!(p["workspace_path"], Value::Null);

    h.engine.fail_writes(None);
    let (status, retried) = call(
        &h.app,
        "POST",
        &format!("/v1/projects/{id}:provision"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{retried}");
    assert_eq!(retried["status"], "provisioning");
    assert_eq!(settled(&h, &id).await["status"], "ready");

    // Only a failed Project is retried.
    let (status, body) = call(
        &h.app,
        "POST",
        &format!("/v1/projects/{id}:provision"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "conflict");
    let (status, _) = call(&h.app, "POST", &format!("/v1/projects/{id}:archive"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_validates_provisioning_input() {
    let h = harness().await;
    for (body, needle) in [
        (
            json!({"name": "P", "repos": [{"url": "https://h/r.git"}]}),
            "agent_config is required",
        ),
        (
            json!({"name": "P", "repos": [{"url": "https://h/r.git"}, {"url": "https://g/r"}],
                   "agent_config": {"harness": "codex"}}),
            "used twice",
        ),
        (
            json!({"name": "P", "repos": [{"url": "https://h/r.git"}], "workspace_path": "/w",
                   "agent_config": {"harness": "codex"}}),
            "not both",
        ),
        (
            json!({"name": "P", "repos": [{"url": "https://h/r.git"}],
                   "agent_config": {"harness": "codex", "effort": "ultra"}}),
            "effort",
        ),
    ] {
        let (status, err) = call(&h.app, "POST", "/v1/projects", Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            err["error"]["message"].as_str().unwrap().contains(needle),
            "{err}"
        );
    }
    assert!(h.engine.writes().is_empty());
}

#[tokio::test]
async fn cors_allows_only_the_desktop_app() {
    let h = harness().await;
    let preflight = |origin: &'static str| {
        Request::builder()
            .method("OPTIONS")
            .uri("/v1/projects")
            .header("origin", origin)
            .header("access-control-request-method", "POST")
            .header("access-control-request-headers", "content-type")
            .body(Body::empty())
            .unwrap()
    };
    for origin in api::APP_ORIGINS {
        let res = h.app.clone().oneshot(preflight(origin)).await.unwrap();
        assert_eq!(
            res.headers().get("access-control-allow-origin").unwrap(),
            origin,
            "{origin}"
        );
    }
    let res = h
        .app
        .clone()
        .oneshot(preflight("https://example.com"))
        .await
        .unwrap();
    assert!(res.headers().get("access-control-allow-origin").is_none());
}
