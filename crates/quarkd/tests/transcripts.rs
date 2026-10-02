//! Transcript tap and coordinator chat, end to end through the API.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use quark_systems::{Event, EventType, TaskKind, TaskState};
use quark_transcript::SessionRoots;
use quarkd::api::{self, AppState};
use quarkd::chat::{Delivery, RecordingInput};
use quarkd::engine::{EngineTask, FleetSnapshot, StubEngine};
use quarkd::projector::Projector;
use quarkd::provision::Layout;
use quarkd::store::Store;
use serde_json::{json, Value};
use tower::ServiceExt;

struct Harness {
    app: Router,
    store: Arc<Store>,
    engine: Arc<StubEngine>,
    chat: Arc<RecordingInput>,
    roots: SessionRoots,
    dir: tempfile::TempDir,
}

fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = Arc::new(StubEngine::new());
    let chat = Arc::new(RecordingInput::new());
    let app = api::router(AppState {
        harnesses: Arc::new(quarkd::harness::HarnessRegistry::new(
            quarkd::harness::builtin(),
            quarkd::harness::HostEnv::default(),
        )),
        store: store.clone(),
        engine: engine.clone(),
        chat: chat.clone(),
        forge: Arc::new(quarkd::forge::StubForge::new()),
        sessions: quarkd::sessions::Sessions::disabled("not used in this test"),
        layout: Layout::new(dir.path().join("home")),
    });
    let roots = SessionRoots {
        claude: vec![dir.path().join("claude")],
        codex: vec![dir.path().join("codex")],
        pi: vec![dir.path().join("pi")],
    };
    Harness {
        app,
        store,
        engine,
        chat,
        roots,
        dir,
    }
}

impl Harness {
    fn projector(&self) -> Projector {
        Projector::new(self.store.clone(), self.engine.clone())
            .with_session_roots(self.roots.clone())
    }

    fn events(&self, after: i64) -> Vec<Event> {
        self.store.events_after(after, 1000).unwrap()
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

fn slug(p: &Path) -> String {
    p.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
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
}

fn claude_user(text: &str) -> Value {
    json!({"type": "user", "message": {"role": "user", "content": text}, "timestamp": "2026-10-02T10:00:00Z"})
}

fn claude_reply(text: &str) -> Value {
    json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": text}]}})
}

#[tokio::test]
async fn session_logs_become_coordinator_and_worker_events() {
    let h = harness();
    let workspace = h.dir.path().join("workspace");
    let worktree = h.dir.path().join("worktrees/fix-42");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&worktree).unwrap();

    let (_, project) = call(
        &h.app,
        "POST",
        "/v1/projects",
        Some(json!({"name": "Quark", "workspace_path": workspace})),
    )
    .await;
    let pid = project["id"].as_str().unwrap().to_string();
    h.engine.set_snapshot(FleetSnapshot {
        tasks: vec![EngineTask {
            id: "fix-42".into(),
            title: "Fix #42".into(),
            kind: Some(TaskKind::Ship),
            state: TaskState::Running,
            state_note: None,
            harness: Some("claude".into()),
            pull_request_url: None,
            worktree: Some(worktree.clone()),
            terminal: None,
        }],
    });

    // The coordinator runs Pi in the workspace root; the worker runs Claude
    // Code in its worktree.
    let pi_log = h.dir.path().join(format!(
        "pi/sessions/--{}--/2026-10-02T10-00-00-000Z_s.jsonl",
        workspace
            .to_string_lossy()
            .trim_start_matches('/')
            .replace('/', "-")
    ));
    append(
        &pi_log,
        &[
            json!({"type": "session", "version": 3, "id": "s", "timestamp": "t", "cwd": workspace}),
            json!({"type": "message", "id": "1", "parentId": null, "timestamp": "2026-10-02T10:00:00Z",
                   "message": {"role": "user", "content": "fix #42", "timestamp": 1}}),
        ],
    );
    let claude_log = h
        .dir
        .path()
        .join(format!("claude/projects/{}/w.jsonl", slug(&worktree)));
    append(
        &claude_log,
        &[claude_user("Fix #42"), claude_reply("On it.")],
    );

    let projector = h.projector();
    projector.refresh_all().await.unwrap();
    let events = h.events(0);
    let types: Vec<_> = events.iter().map(|e| e.event_type).collect();
    assert_eq!(
        types,
        vec![
            EventType::ProjectUpdated,
            EventType::TaskCreated,
            EventType::CoordinatorMessage,
            EventType::WorkerTranscript,
            EventType::WorkerTranscript,
        ]
    );
    let tid = events[1].payload["id"].as_str().unwrap();
    assert_eq!(events[2].payload["coordinator_id"], pid.as_str());
    assert_eq!(events[2].payload["entry"]["role"], "user");
    assert_eq!(events[2].payload["entry"]["text"], "fix #42");
    assert_eq!(events[3].payload["task_id"], tid);
    assert_eq!(events[4].payload["entry"]["role"], "assistant");
    assert_eq!(events[4].payload["entry"]["text"], "On it.");
    let last = events.last().unwrap().seq;

    let (status, history) = call(&h.app, "GET", &format!("/v1/tasks/{tid}/transcript"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(history.as_array().unwrap().len(), 2);
    assert_eq!(history[1]["id"], events[4].seq);
    assert_eq!(history[1]["role"], "assistant");
    assert_eq!(history[1]["text"], "On it.");
    let (_, paged) = call(
        &h.app,
        "GET",
        &format!("/v1/tasks/{tid}/transcript?after={}", events[3].seq),
        None,
    )
    .await;
    assert_eq!(paged.as_array().unwrap().len(), 1);
    let (_, chat) = call(
        &h.app,
        "GET",
        &format!("/v1/coordinators/{pid}/messages"),
        None,
    )
    .await;
    assert_eq!(chat.as_array().unwrap().len(), 1);
    assert_eq!(chat[0]["role"], "user");
    let (status, _) = call(&h.app, "GET", "/v1/tasks/tsk_missing/transcript", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Nothing new: nothing emitted.
    projector.refresh_all().await.unwrap();
    assert!(h.events(last).is_empty());

    // Only what was appended is emitted, including after a daemon restart,
    // because the offset is stored with the events.
    append(&claude_log, &[claude_reply("Tests pass.")]);
    let restarted = h.projector();
    restarted.refresh_all().await.unwrap();
    let new = h.events(last);
    assert_eq!(new.len(), 1);
    assert_eq!(new[0].event_type, EventType::WorkerTranscript);
    assert_eq!(new[0].payload["entry"]["text"], "Tests pass.");
}

#[tokio::test]
async fn coordinator_messages_are_typed_into_the_session() {
    let h = harness();
    let (status, body) = call(
        &h.app,
        "POST",
        "/v1/coordinators/prj_missing/messages",
        Some(json!({"text": "hello"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let (_, bare) = call(
        &h.app,
        "POST",
        "/v1/projects",
        Some(json!({"name": "Bare"})),
    )
    .await;
    let bare = bare["id"].as_str().unwrap();
    let (status, body) = call(
        &h.app,
        "POST",
        &format!("/v1/coordinators/{bare}/messages"),
        Some(json!({"text": "hello"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "no_workspace");

    let (_, project) = call(
        &h.app,
        "POST",
        "/v1/projects",
        Some(json!({"name": "Quark", "workspace_path": "/tmp/quark-ws"})),
    )
    .await;
    let pid = project["id"].as_str().unwrap();
    let uri = format!("/v1/coordinators/{pid}/messages");

    for bad in ["   ", "\u{1b}[2Jgotcha"] {
        let (status, body) = call(&h.app, "POST", &uri, Some(json!({"text": bad}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid_request");
    }
    assert!(h.chat.sent().is_empty());

    let (status, body) = call(
        &h.app,
        "POST",
        &uri,
        Some(json!({"text": "fix #42 and add tests for the parser"})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["coordinator_id"], pid);
    assert_eq!(body["confirmed"], true);
    let sent = h.chat.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0.root, Path::new("/tmp/quark-ws"));
    assert_eq!(sent[0].1, "fix #42 and add tests for the parser");

    h.chat.set_outcome(Ok(Delivery::Unconfirmed));
    let (status, body) = call(&h.app, "POST", &uri, Some(json!({"text": "again"}))).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["confirmed"], false);

    h.chat.set_outcome(Err("pane is gone".into()));
    let (status, body) = call(&h.app, "POST", &uri, Some(json!({"text": "again"}))).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"]["code"], "delivery_failed");
}

#[tokio::test]
async fn without_a_session_client_messages_are_refused() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let app = api::router(AppState {
        harnesses: Arc::new(quarkd::harness::HarnessRegistry::new(
            quarkd::harness::builtin(),
            quarkd::harness::HostEnv::default(),
        )),
        store: store.clone(),
        engine: Arc::new(StubEngine::new()),
        chat: Arc::new(quarkd::chat::NoSessions),
        forge: Arc::new(quarkd::forge::StubForge::new()),
        sessions: quarkd::sessions::Sessions::disabled("not used in this test"),
        layout: Layout::new(std::env::temp_dir().join("quark-test-home")),
    });
    let (_, project) = call(
        &app,
        "POST",
        "/v1/projects",
        Some(json!({"name": "Quark", "workspace_path": "/tmp/quark-ws"})),
    )
    .await;
    let pid = project["id"].as_str().unwrap();
    let (status, body) = call(
        &app,
        "POST",
        &format!("/v1/coordinators/{pid}/messages"),
        Some(json!({"text": "hello"})),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "session_unavailable");
}
