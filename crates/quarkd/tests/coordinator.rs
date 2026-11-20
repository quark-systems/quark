//! The native coordinator's whole loop (slice 6), in quarkd: a worker's
//! question wakes the coordinator through its Brain, the coordinator asks
//! the user over MCP, its turn-end hook closes the turn, and the user's
//! answer through the API wakes it again.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use quark_core::worker::{Transport, WorkerEnvelope, WorkerMessage};
use quark_core::{event::kinds, EventLog, HostId, NewEvent, ProjectId, TaskId};
use quark_systems::{CreateProject, DecisionState};
use quark_transcript::SessionRoots;
use quarkd::accounts::{Accounts, StubQuota};
use quarkd::api::{self, AppState};
use quarkd::chat::RecordingInput;
use quarkd::coordinator_hands::{EngineHands, InputBrain};
use quarkd::coordinator_http;
use quarkd::engine::StubEngine;
use quarkd::harness::{HarnessRegistry, HostEnv};
use quarkd::native_coordinator::{NativeCoordinator, Wiring};
use quarkd::provision::Layout;
use quarkd::store::Store;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn post(uri: &str, body: Value) -> Request<Body> {
    Request::post(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// A Project with a native coordinator typing into a [`RecordingInput`],
/// and the daemon's API with the coordinator's own routes.
struct Rig {
    _dir: tempfile::TempDir,
    store: Arc<Store>,
    project: quark_systems::Project,
    pid: ProjectId,
    engine: Arc<StubEngine>,
    log: quark_eventlog::SqliteEventLog,
    typed: Arc<RecordingInput>,
    coordinator: NativeCoordinator,
    app: Router,
    /// The coordinator's MCP endpoint and its `Stop` hook.
    mcp_path: String,
    hook_path: String,
}

async fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let project = store
        .create_project(CreateProject {
            name: "Quark".into(),
            goal: None,
            workspace_path: Some(ws.display().to_string()),
            repos: vec![],
            agent_config: None,
            dispatch_preset: None,
            delivery: None,
        })
        .unwrap();
    let pid = ProjectId::new(project.id.clone());
    let engine = Arc::new(StubEngine::new());
    let log = quark_eventlog::SqliteEventLog::open(dir.path().join("events.db")).unwrap();
    let typed = Arc::new(RecordingInput::new());
    let layout = Layout::new(dir.path().join("home"));
    let coordinator = NativeCoordinator::open(
        store.clone(),
        Arc::new(log.clone()),
        layout.clone(),
        SessionRoots::default(),
        Wiring {
            brain: Arc::new(InputBrain::new(store.clone(), typed.clone())),
            hands: Arc::new(EngineHands::new(store.clone(), engine.clone())),
            base_url: "http://127.0.0.1:7777".into(),
        },
    )
    .await
    .unwrap();
    let harnesses = Arc::new(HarnessRegistry::new(
        quarkd::harness::builtin(),
        HostEnv {
            path: None,
            home: None,
            vars: HashMap::new(),
            ..Default::default()
        },
    ));
    let accounts = Arc::new(Accounts::new(
        store.clone(),
        harnesses.clone(),
        Arc::new(StubQuota::new()),
        &[],
    ));
    let app = api::router(AppState {
        store: store.clone(),
        engine: engine.clone(),
        harnesses,
        accounts,
        sessions: quarkd::sessions::Sessions::disabled("not used in this test"),
        layout,
        chat: typed.clone(),
        forge: Arc::new(quarkd::forge::StubForge::new()),
        events: log.clone(),
        triggers: None,
        compaction: Default::default(),
        beads: Default::default(),
        coordinator: None,
    })
    .nest(
        coordinator_http::PREFIX,
        coordinator_http::router(
            coordinator.coordinator(),
            coordinator.tokens(),
            store.clone(),
            SessionRoots::default(),
        ),
    );

    // The first pass points the coordinator at its tools; nothing wakes it.
    coordinator.pass().await.unwrap();
    assert!(typed.sent().is_empty());
    let mcp: Value =
        serde_json::from_str(&std::fs::read_to_string(ws.join(".mcp.json")).unwrap()).unwrap();
    let url = mcp["mcpServers"]["quark-coordinator"]["url"]
        .as_str()
        .unwrap()
        .to_string();
    let mcp_path = url
        .strip_prefix("http://127.0.0.1:7777")
        .unwrap()
        .to_string();
    let hook_path = mcp_path.replace("/mcp", "/hooks/Stop");
    Rig {
        _dir: dir,
        store,
        project,
        pid,
        engine,
        log,
        typed,
        coordinator,
        app,
        mcp_path,
        hook_path,
    }
}

impl Rig {
    /// A worker asks `question`, which needs the coordinator's judgment.
    async fn worker_asks(&self, task: &str, key: &str, question: &str) {
        let ask = WorkerEnvelope {
            task: TaskId::from(task),
            generation: "g1".into(),
            via: Transport::Mcp,
            message: WorkerMessage::Ask {
                key: key.into(),
                question: question.into(),
            },
        };
        self.log
            .append(
                NewEvent::typed(
                    HostId::from("h"),
                    self.pid.clone(),
                    Some(TaskId::from(task)),
                    kinds::WORKER,
                    &ask,
                )
                .unwrap(),
            )
            .await
            .unwrap();
    }

    /// The coordinator's harness reports its turn's end.
    async fn turn_ends(&self) {
        let (s, b) = send(
            &self.app,
            post(
                &self.hook_path,
                json!({"usage": {"input": 900, "output": 80}}),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        // Claude Code's `Stop` gets an empty decision, which lets the
        // session stop.
        assert_eq!(b, json!({}));
    }

    async fn woken(&self) -> usize {
        self.log
            .read(quark_core::Seq::ZERO, 10_000)
            .await
            .unwrap()
            .iter()
            .filter(|e| e.kind.as_str() == "coordinator.woken")
            .count()
    }
}

#[tokio::test]
async fn ask_the_user_and_wake_on_the_answer() {
    let rig = rig().await;
    let Rig {
        store,
        project,
        pid,
        engine,
        typed,
        coordinator,
        app,
        mcp_path,
        ..
    } = &rig;

    // A worker's question wakes it: the briefing is typed into its session.
    rig.worker_asks("t1", "schema", "Drop the legacy table?")
        .await;
    coordinator.pass().await.unwrap();
    let sent = typed.sent();
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0].1.contains("Drop the legacy table?"),
        "{}",
        sent[0].1
    );

    // It judges this one is the user's and asks over MCP.
    let call = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
        "name": "ask_user",
        "arguments": {
            "key": "legacy-table",
            "question": "Drop the legacy table? It cannot be undone.",
            "context": "The migration can drop it now or keep it for a release.",
            "recommended_why": "keeps a rollback",
            "options": ["Drop", {"label": "Keep", "consequence": "the migration stays two-step"}],
            "recommended": "Keep",
            "blocks": ["t1"]
        }
    }});
    let (s, b) = send(app, post(mcp_path, call)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["result"]["isError"], false, "{b}");
    let open = store
        .list_project_decisions(Some(&project.id), Some(DecisionState::Open))
        .unwrap();
    assert_eq!(open.len(), 1);
    let decision = open[0].clone();
    assert_eq!(decision.brief.asked_by.as_deref(), Some("coordinator"));

    // Its turn-end hook closes the turn.
    rig.turn_ends().await;
    assert!(coordinator.coordinator().open_turn(pid).await.is_none());

    // The user answers in the app; the answer wakes the coordinator.
    let (s, b) = send(
        app,
        post(
            &format!("/v1/decisions/{}:answer", decision.id),
            json!({"answer": "Keep", "answered_by": "Matt"}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["state"], "answered");
    assert!(
        engine.writes().is_empty(),
        "the engine holds no such question"
    );
    coordinator.pass().await.unwrap();
    let sent = typed.sent();
    assert_eq!(sent.len(), 2);
    assert!(
        sent[1]
            .1
            .contains("Matt answered your question legacy-table: Keep"),
        "{}",
        sent[1].1
    );
}

#[tokio::test]
async fn a_wake_waits_for_the_turn_the_user_started_and_is_typed_once() {
    let rig = rig().await;
    let Rig {
        project,
        pid,
        typed,
        coordinator,
        app,
        ..
    } = &rig;

    // The user writes to the coordinator, which starts working on it.
    let (s, b) = send(
        app,
        post(
            &format!("/v1/coordinators/{}/messages", project.id),
            json!({"text": "How is the migration going?"}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{b}");
    assert_eq!(typed.sent().len(), 1);

    // A worker's question arrives while it works. Typed now, the wake would
    // land inside the user's turn; it waits, with no turn opened for it.
    rig.worker_asks("t1", "schema", "Drop the legacy table?")
        .await;
    for _ in 0..3 {
        coordinator.pass().await.unwrap();
    }
    assert_eq!(typed.sent().len(), 1);
    assert_eq!(rig.woken().await, 0);
    assert!(coordinator.coordinator().open_turn(pid).await.is_none());
    assert_eq!(coordinator.coordinator().pending(pid).await.len(), 1);

    // The user's turn ends: the wake goes in, once.
    rig.turn_ends().await;
    for _ in 0..3 {
        coordinator.pass().await.unwrap();
    }
    let sent = typed.sent();
    assert_eq!(sent.len(), 2);
    assert!(sent[1].1.starts_with("Wake "), "{}", sent[1].1);
    assert!(
        sent[1].1.contains("Drop the legacy table?"),
        "{}",
        sent[1].1
    );
    assert_eq!(rig.woken().await, 1);

    // A second question waits for the wake's own turn, as before, and
    // follows it at once when that turn ends.
    rig.worker_asks("t2", "index", "Add the index now?").await;
    coordinator.pass().await.unwrap();
    assert_eq!(typed.sent().len(), 2);
    rig.turn_ends().await;
    coordinator.pass().await.unwrap();
    let sent = typed.sent();
    assert_eq!(sent.len(), 3);
    assert!(sent[2].1.contains("Add the index now?"), "{}", sent[2].1);
}
