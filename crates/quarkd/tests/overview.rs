//! The Project dashboard's Overview tab reads the event log the firstmate
//! bridge fills: live status, and a digest after a given `seq`.

use std::sync::Arc;

use quark_eventlog::{FirstmateBridge, SqliteEventLog};
use quark_systems::{CreateProject, TaskKind, TaskState};
use quarkd::engine::{EngineTask, FleetSnapshot};
use quarkd::event_ingest::{self, EventIngest};
use quarkd::store::Store;
use serde_json::Value;

async fn get(app: &axum::Router, uri: &str) -> (axum::http::StatusCode, Value) {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let req = axum::http::Request::builder()
        .uri(uri)
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn overview_reports_live_status_and_what_changed_since() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let state = workspace.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("t1.status"), "working: started\n").unwrap();

    let store = Arc::new(Store::open_in_memory().unwrap());
    let project = store
        .create_project(CreateProject {
            name: "Quark".into(),
            workspace_path: Some(workspace.path().to_string_lossy().into_owned()),
            ..Default::default()
        })
        .unwrap();
    let harnesses = Arc::new(quarkd::harness::HarnessRegistry::new(
        quarkd::harness::builtin(),
        quarkd::harness::HostEnv::default(),
    ));
    // The daemon's ingest writes the file the endpoint reads.
    let log = SqliteEventLog::open(home.path().join(quark_eventlog::FILE_NAME)).unwrap();
    let app = quarkd::api::router(quarkd::api::AppState {
        store: store.clone(),
        engine: Arc::new(quarkd::engine::StubEngine::new()),
        harnesses: harnesses.clone(),
        accounts: Arc::new(quarkd::accounts::Accounts::new(
            store.clone(),
            harnesses,
            Arc::new(quarkd::accounts::StubQuota::new()),
            &["CLAUDE_CONFIG_DIR"],
        )),
        sessions: quarkd::sessions::Sessions::disabled("not used in this test"),
        layout: quarkd::provision::Layout::new(home.path()),
        chat: Arc::new(quarkd::chat::RecordingInput::new()),
        forge: Arc::new(quarkd::forge::StubForge::new()),
        events: log.clone(),
        triggers: None,
        beads: Default::default(),
    });
    // t1 is a task the daemon knows, so the Overview names and links it.
    store
        .apply_snapshot(
            &project.id,
            &FleetSnapshot {
                tasks: vec![EngineTask {
                    id: "t1".into(),
                    title: "Event stream".into(),
                    kind: Some(TaskKind::Ship),
                    state: TaskState::Running,
                    state_note: None,
                    state_source: None,
                    harness: Some("claude".into()),
                    pull_request_url: None,
                    terminal: None,
                    worktree: None,
                }],
            },
        )
        .unwrap();
    let ingest = EventIngest::new(store, FirstmateBridge::new(log, event_ingest::host()));
    ingest.ingest_all().await.unwrap();

    let uri = format!("/v1/projects/{}/overview", project.id);
    let (status, first) = get(&app, &uri).await;
    assert_eq!(status, 200, "{first}");
    assert_eq!(first["head"], 1);
    assert_eq!(first["live"]["counts"]["working"], 1);
    assert_eq!(first["live"]["tasks"][0]["engine_task"], "t1");
    assert_eq!(first["live"]["tasks"][0]["title"], "Event stream");
    assert!(first["live"]["tasks"][0]["task_id"].is_string());
    assert!(first["digest"].is_null());

    std::fs::write(
        state.join("t1.status"),
        "working: started\nneeds-decision: [key=api] REST or gRPC?\n",
    )
    .unwrap();
    std::fs::write(
        state.join("t2.status"),
        "done: PR https://github.com/o/r/pull/3 checks green\n",
    )
    .unwrap();
    ingest.ingest_all().await.unwrap();

    let (_, now) = get(&app, &format!("{uri}?since=1")).await;
    assert_eq!(now["head"], 3);
    let counts = &now["live"]["counts"];
    assert_eq!(
        (counts["needs_decision"].as_u64(), counts["done"].as_u64()),
        (Some(1), Some(1))
    );
    assert_eq!(now["live"]["tasks"][0]["open_decisions"][0], "api");
    let digest = &now["digest"];
    assert_eq!(digest["events"], 2);
    assert_eq!(digest["decisions_opened"], 1);
    assert_eq!(digest["pull_requests"], 1);
    assert_eq!(digest["highlights"][0]["kind"], "done");
    assert_eq!(
        digest["highlights"][0]["url"],
        "https://github.com/o/r/pull/3"
    );

    let (status, _) = get(&app, "/v1/projects/nope/overview").await;
    assert_eq!(status, 404);
}
