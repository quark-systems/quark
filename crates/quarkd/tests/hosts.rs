//! The Hosts view and a Project's host slice read host registrations,
//! telemetry samples and worktree pool reports from the event log; the
//! engine's snapshot attributes usage and fills the pool report.

use std::path::PathBuf;
use std::sync::Arc;

use quark_core::host::{Capacity, Health, Host, HostRegistry, Platform, RuntimeKind};
use quark_core::telemetry::{HostSample, QuarkDisk, Usage};
use quark_core::{EventLog, ProjectId};
use quark_eventlog::SqliteEventLog;
use quark_hosts::{EventHosts, Workloads};
use quark_systems::{CreateProject, TaskKind, TaskState};
use quarkd::engine::{EngineTask, FleetSnapshot, StubEngine};
use quarkd::host_sources::{EngineView, EngineWorkloads, PoolReporter};
use quarkd::store::Store;
use serde_json::Value;
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

fn task(id: &str, terminal: Option<&str>, worktree: Option<PathBuf>) -> EngineTask {
    EngineTask {
        id: id.into(),
        title: "Hosts view".into(),
        kind: Some(TaskKind::Ship),
        state: TaskState::Running,
        state_note: None,
        harness: Some("claude".into()),
        pull_request_url: None,
        terminal: terminal.map(Into::into),
        worktree,
    }
}

struct Fixture {
    _home: tempfile::TempDir,
    workspace: tempfile::TempDir,
    store: Arc<Store>,
    engine: Arc<StubEngine>,
    log: SqliteEventLog,
    app: axum::Router,
    project: String,
}

fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
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
    let log = SqliteEventLog::open(home.path().join(quark_eventlog::FILE_NAME)).unwrap();
    let engine = Arc::new(StubEngine::new());
    let app = quarkd::api::router(quarkd::api::AppState {
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
        layout: quarkd::provision::Layout::new(home.path()),
        chat: Arc::new(quarkd::chat::RecordingInput::new()),
        forge: Arc::new(quarkd::forge::StubForge::new()),
        events: log.clone(),
        triggers: None,
    });
    Fixture {
        _home: home,
        workspace,
        store,
        engine,
        log,
        app,
        project: project.id,
    }
}

#[tokio::test]
async fn hosts_view_and_project_slice() {
    let f = fixture();
    let wt = f.workspace.path().join("wt-t1");
    std::fs::create_dir_all(&wt).unwrap();
    let snapshot = FleetSnapshot {
        tasks: vec![
            task("t1", Some("quark:fm-t1"), Some(wt.clone())),
            task("queued", None, None),
        ],
    };
    f.store.apply_snapshot(&f.project, &snapshot).unwrap();
    f.engine.set_snapshot(snapshot);

    let log: Arc<dyn EventLog> = Arc::new(f.log.clone());
    let registry = EventHosts::new(log.clone(), "mac".into());
    registry
        .register(Host {
            id: "mac".into(),
            name: "Matt's Mac".into(),
            runtime: RuntimeKind::Local,
            platform: Platform {
                os: "macos".into(),
                arch: "aarch64".into(),
            },
            capacity: Capacity {
                cpus: 10,
                memory_bytes: 32 << 30,
                disk_bytes: 0,
                max_workers: 6,
            },
            health: Health::Healthy,
            projects: Vec::new(),
            tasks: Vec::new(),
        })
        .await
        .unwrap();

    // Attribution names the task's worktree; without tmux it has no pids.
    let view = EngineView::new(
        f.store.clone(),
        f.engine.clone(),
        quarkd::sessions::Sessions::disabled("no tmux"),
        f.workspace.path().to_path_buf(),
    );
    let workloads = EngineWorkloads(view.clone()).current().await.unwrap();
    assert_eq!(workloads.len(), 1);
    assert_eq!(workloads[0].project.as_str(), f.project);
    assert_eq!(workloads[0].task.as_ref().unwrap().as_str(), "t1");
    assert_eq!(workloads[0].worktree.as_deref(), Some(wt.as_path()));

    // The pool report lists the task's worktree, and only records changes.
    let mut pools = PoolReporter::new(view, log.clone(), "mac".into());
    pools.report_once().await.unwrap();
    pools.report_once().await.unwrap();
    let reports = f
        .log
        .read(quark_core::Seq::ZERO, 100)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind.as_str() == quarkd::hosts::WORKTREES)
        .count();
    assert_eq!(reports, 1);

    let now = OffsetDateTime::now_utc();
    for (i, mem) in [100u64 << 20, 300 << 20].into_iter().enumerate() {
        let sample = HostSample {
            host: "mac".into(),
            ts: now - time::Duration::minutes(2 - i as i64),
            cpu: 0.25,
            memory_used_bytes: 12 << 30,
            memory_total_bytes: 32 << 30,
            memory_pressure: 0.2,
            disk_free_bytes: 200 << 30,
            quark_disk: QuarkDisk::default(),
            usage: vec![
                Usage {
                    project: ProjectId::new(&f.project),
                    task: None,
                    cpu: 0.05,
                    memory_bytes: 50 << 20,
                    disk_bytes: 0,
                },
                Usage {
                    project: ProjectId::new(&f.project),
                    task: Some("t1".into()),
                    cpu: 0.1,
                    memory_bytes: mem,
                    disk_bytes: 1 << 20,
                },
            ],
        };
        log.append(quark_hosts::recorder::sample_event(&sample).unwrap())
            .await
            .unwrap();
    }

    let (status, v) = get(&f.app, "/v1/hosts?hours=1").await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["hours"], 1);
    let h = &v["hosts"][0];
    assert_eq!(h["name"], "Matt's Mac");
    assert_eq!(h["health"]["status"], "healthy");
    assert_eq!(h["capacity"]["max_workers"], 6);
    assert_eq!(h["series"].as_array().unwrap().len(), 2);
    assert_eq!(h["projects"][0]["project_name"], "Quark");
    assert_eq!(h["projects"][0]["memory_bytes"], (350u64 << 20));
    let parts = h["projects"][0]["parts"].as_array().unwrap();
    assert!(parts[0]["engine_task"].is_null());
    assert_eq!(parts[1]["title"], "Hosts view");
    assert!(parts[1]["task_id"].is_string());
    assert_eq!(h["worktrees"]["in_use"], 1);
    assert_eq!(h["worktrees"]["slots"][0]["holder"], "t1");

    let (status, p) = get(&f.app, &format!("/v1/projects/{}/hosts", f.project)).await;
    assert_eq!(status, 200, "{p}");
    assert_eq!(p["hours"], 6);
    let s = &p["hosts"][0];
    assert_eq!(s["host_id"], "mac");
    assert_eq!(s["now"]["memory_bytes"], (350u64 << 20));
    assert_eq!(s["series"][0]["memory_bytes"], (150u64 << 20));
    assert_eq!(s["host"]["memory_total_bytes"], (32u64 << 30));
    assert_eq!(s["worktrees"].as_array().unwrap().len(), 1);

    let (status, _) = get(&f.app, "/v1/projects/nope/hosts").await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn no_hosts_yet() {
    let f = fixture();
    let (status, v) = get(&f.app, "/v1/hosts").await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["hosts"], serde_json::json!([]));
    let (_, p) = get(&f.app, &format!("/v1/projects/{}/hosts", f.project)).await;
    assert_eq!(p["hosts"], serde_json::json!([]));
}
