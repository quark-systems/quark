use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use futures_util::StreamExt;
use http_body_util::BodyExt;
use quark_systems::{Event, EventType, TaskKind, TaskState};
use quarkd::api::{self, ApiDoc, AppState};
use quarkd::engine::{EngineTask, FleetSnapshot, Hold, StubEngine};
use quarkd::projector::Projector;
use quarkd::store::Store;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

struct Harness {
    app: Router,
    addr: std::net::SocketAddr,
    engine: Arc<StubEngine>,
    projector: Projector,
}

async fn harness() -> Harness {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = Arc::new(StubEngine::new());
    let app = api::router(AppState {
        store: store.clone(),
        engine: engine.clone(),
        sessions: quarkd::sessions::Sessions::disabled("not used in this test"),
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = app.clone();
    tokio::spawn(async move { axum::serve(listener, served).await.unwrap() });
    Harness {
        app,
        addr,
        projector: Projector::new(store, engine.clone()),
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
        harness: Some("claude".into()),
        pull_request_url: None,
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
