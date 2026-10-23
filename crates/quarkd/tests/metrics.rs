//! The Project dashboard's Metrics tab, read from the event log.

use std::sync::Arc;

use quark_core::event::kinds;
use quark_core::{EventLog, HostId, NewEvent, ProjectId, TaskId};
use quark_eventlog::SqliteEventLog;
use quark_systems::CreateProject;
use quarkd::store::Store;
use serde_json::{json, Value};
use time::OffsetDateTime;

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

async fn append(
    log: &SqliteEventLog,
    ago_s: i64,
    project: &str,
    task: &str,
    kind: &str,
    payload: Value,
) {
    let mut e = NewEvent::new(
        HostId::from("local"),
        ProjectId::from(project),
        Some(TaskId::from(task)),
        kind,
        payload,
    );
    e.ts = OffsetDateTime::now_utc() - time::Duration::seconds(ago_s);
    log.append(e).await.unwrap();
}

async fn spawn(log: &SqliteEventLog, ago_s: i64, project: &str, task: &str) {
    let payload = json!({"type": "launched", "generation": "g1", "session": "s",
        "backend": "tmux", "cause": "spawn", "dir": "/tmp", "harness": "claude",
        "model": null, "effort": null});
    append(log, ago_s, project, task, "supervisor.launched", payload).await;
}

async fn task(log: &SqliteEventLog, project: &str, task: &str, payload: Value) {
    append(log, 0, project, task, kinds::TASK, payload).await;
}

#[tokio::test]
async fn metrics_count_the_projects_own_events() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let harnesses = Arc::new(quarkd::harness::HarnessRegistry::new(
        quarkd::harness::builtin(),
        quarkd::harness::HostEnv::default(),
    ));
    let events = SqliteEventLog::open(dir.path().join("events.db")).unwrap();
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
        layout: quarkd::provision::Layout::new(dir.path()),
        chat: Arc::new(quarkd::chat::RecordingInput::new()),
        forge: Arc::new(quarkd::forge::StubForge::new()),
        events: events.clone(),
        triggers: None,
        compaction: Default::default(),
        beads: Default::default(),
    });
    let project = store
        .create_project(CreateProject {
            name: "Quark".into(),
            ..Default::default()
        })
        .unwrap();
    let p = project.id.as_str();

    spawn(&events, 3600, p, "a").await;
    task(
        &events,
        p,
        "a",
        json!({"type": "started", "generation": "g1"}),
    )
    .await;
    task(&events, p, "a", json!({"type": "completed"})).await;
    spawn(&events, 600, p, "b").await;
    let ask = json!({"type": "decision_needed", "key": "k", "question": "q"});
    task(&events, p, "b", ask).await;
    task(
        &events,
        p,
        "b",
        json!({"type": "decision_answered", "key": "k"}),
    )
    .await;
    task(&events, p, "b", json!({"type": "failed", "reason": "x"})).await;
    // Another Project's work is not counted.
    task(&events, "other", "a", json!({"type": "completed"})).await;

    let (code, m) = get(&app, &format!("/v1/projects/{p}/metrics")).await;
    assert_eq!(code, 200, "{m}");
    assert_eq!(m["days"], 7);
    assert_eq!(m["throughput"]["done"], 1);
    assert_eq!(m["throughput"]["failed"], 1);
    assert_eq!(m["throughput"]["per_day"].as_array().unwrap().len(), 7);
    assert_eq!(m["gates"]["pass_rate"], 0.5);
    assert_eq!(m["gates"]["first_time_green"], 1);
    assert_eq!(m["interventions"]["decisions"], 1);
    assert_eq!(m["lead_time"]["tasks"], 1);
    let lead = m["lead_time"]["median_s"].as_u64().unwrap();
    assert!((3600..3700).contains(&lead), "{lead}");
    assert!(m["log_started_at"].is_string());
    assert_eq!(m["accounts"], json!([]));
    assert_eq!(m["unavailable"].as_array().unwrap().len(), 2);

    let (_, m) = get(&app, &format!("/v1/projects/{p}/metrics?days=30")).await;
    assert_eq!(m["throughput"]["per_day"].as_array().unwrap().len(), 30);

    let (code, _) = get(&app, "/v1/projects/nope/metrics").await;
    assert_eq!(code, 404);
}
