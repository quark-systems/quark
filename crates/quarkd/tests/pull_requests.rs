//! The PR center end to end over the stub engine and stub forge.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use quark_systems::{
    Check, CheckStatus, CreateProject, EventType, MergeMethod, Mergeability, ProjectStatus,
    PullRequestState, RepoSource, ReviewDecision, TaskKind, TaskState,
};
use quarkd::api::{self, AppState};
use quarkd::chat::RecordingInput;
use quarkd::engine::{EngineTask, FleetSnapshot, StubEngine, StubWrite};
use quarkd::forge::{ForgePr, StubForge};
use quarkd::harness::{HarnessRegistry, HostEnv};
use quarkd::pr_center::PrCenter;
use quarkd::provision::Layout;
use quarkd::store::Store;
use serde_json::{json, Value};
use tower::ServiceExt;

const URL: &str = "https://github.com/quark-systems/quark/pull/16";

struct Harness {
    _dir: tempfile::TempDir,
    app: Router,
    store: Arc<Store>,
    engine: Arc<StubEngine>,
    forge: Arc<StubForge>,
    center: PrCenter,
    project_id: String,
}

/// A ready Project with one repo and a workspace, and one task that opened
/// the pull request at [`URL`].
async fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = Arc::new(StubEngine::new());
    let forge = Arc::new(StubForge::new());
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
        layout: Layout::new(dir.path().join("home")),
        chat: Arc::new(RecordingInput::new()),
        forge: forge.clone(),
        events: quark_eventlog::SqliteEventLog::open(":memory:").unwrap(),
        triggers: None,
    });
    let project = store
        .create_project(CreateProject {
            name: "Quark".into(),
            repos: vec![RepoSource {
                url: "https://github.com/quark-systems/quark.git".into(),
                name: Some("quark".into()),
            }],
            ..Default::default()
        })
        .unwrap();
    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    store
        .set_project_status(
            &project.id,
            ProjectStatus::Ready,
            None,
            Some(ws.to_str().unwrap()),
            None,
        )
        .unwrap();
    store
        .apply_snapshot(
            &project.id,
            &FleetSnapshot {
                tasks: vec![EngineTask {
                    id: "ship-pr-center".into(),
                    title: "PR center".into(),
                    kind: Some(TaskKind::Ship),
                    state: TaskState::InReview,
                    state_note: None,
                    state_source: None,
                    harness: Some("claude".into()),
                    pull_request_url: Some(URL.into()),
                    terminal: None,
                    worktree: None,
                }],
            },
        )
        .unwrap();
    let center = PrCenter::new(store.clone(), engine.clone(), forge.clone());
    Harness {
        _dir: dir,
        app,
        store,
        engine,
        forge,
        center,
        project_id: project.id,
    }
}

fn forge_pr(state: PullRequestState, check: CheckStatus) -> ForgePr {
    ForgePr {
        title: "PR center".into(),
        author: Some("claude".into()),
        state,
        head_ref: Some("claude/pr-center".into()),
        base_ref: Some("main".into()),
        head_sha: Some("abc123".into()),
        mergeable: Mergeability::Mergeable,
        review_decision: ReviewDecision::None,
        additions: Some(10),
        deletions: Some(2),
        changed_files: Some(2),
        opened_at: Some("2026-10-02T16:00:00Z".into()),
        updated_at: Some("2026-10-02T16:10:00Z".into()),
        merged_at: None,
        closed_at: None,
        checks: vec![Check {
            name: "CI / test".into(),
            status: check,
            conclusion: None,
            details_url: Some("https://github.com/quark-systems/quark/actions/runs/1".into()),
            started_at: None,
            completed_at: None,
        }],
        reviews: vec![],
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

fn event_types(store: &Store) -> Vec<EventType> {
    store
        .events_after(0, 1000)
        .unwrap()
        .into_iter()
        .map(|e| e.event_type)
        .collect()
}

#[tokio::test]
async fn lists_pull_requests_with_checks_and_reviews() {
    let h = harness().await;
    // Before the forge answers, the pull request is listed with the error.
    h.center.refresh().await;
    let (status, list) = call(&h.app, "GET", "/v1/pull-requests", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["number"], 16);
    assert_eq!(list[0]["repo"], "quark-systems/quark");
    assert!(list[0]["sync_error"]
        .as_str()
        .unwrap()
        .contains("no pull request"));

    h.forge
        .set(URL, forge_pr(PullRequestState::Open, CheckStatus::Pending));
    h.center.refresh().await;
    let (_, list) = call(&h.app, "GET", "/v1/pull-requests?state=open", None).await;
    let pr = &list[0];
    assert_eq!(pr["title"], "PR center");
    assert_eq!(pr["checks_state"], "pending");
    assert_eq!(pr["checks"][0]["name"], "CI / test");
    assert_eq!(pr["sync_error"], Value::Null);
    assert_eq!(pr["evidence"], Value::Null);
    let task_id = pr["task_id"].as_str().unwrap().to_string();
    assert_eq!(
        h.store
            .get_task(&task_id)
            .unwrap()
            .pull_request_url
            .as_deref(),
        Some(URL)
    );

    let (_, other) = call(&h.app, "GET", "/v1/pull-requests?project_id=prj_none", None).await;
    assert_eq!(other, json!([]));
    let (_, merged) = call(&h.app, "GET", "/v1/pull-requests?state=merged", None).await;
    assert_eq!(merged, json!([]));

    let id = pr["id"].as_str().unwrap();
    let (status, one) = call(&h.app, "GET", &format!("/v1/pull-requests/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one["id"], id);
    let (status, _) = call(&h.app, "GET", "/v1/pull-requests/pr_nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let types = event_types(&h.store);
    assert!(types.contains(&EventType::PrUpdated));
    assert!(types.contains(&EventType::CheckUpdated));
}

#[tokio::test]
async fn diff_whole_and_per_file() {
    let h = harness().await;
    h.forge
        .set(URL, forge_pr(PullRequestState::Open, CheckStatus::Success));
    h.forge.set_diff(
        URL,
        "diff --git a/a.rs b/a.rs\n+a\ndiff --git a/b.rs b/b.rs\n+b\n",
    );
    h.center.refresh().await;
    let (_, list) = call(&h.app, "GET", "/v1/pull-requests", None).await;
    let id = list[0]["id"].as_str().unwrap();

    let (status, d) = call(&h.app, "GET", &format!("/v1/pull-requests/{id}/diff"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(d["patch"].as_str().unwrap().contains("b.rs"));
    assert_eq!(d["truncated"], false);

    let (_, d) = call(
        &h.app,
        "GET",
        &format!("/v1/pull-requests/{id}/diff?path=b.rs"),
        None,
    )
    .await;
    assert_eq!(d["patch"], "diff --git a/b.rs b/b.rs\n+b\n");
    assert_eq!(d["path"], "b.rs");

    let (status, _) = call(
        &h.app,
        "GET",
        &format!("/v1/pull-requests/{id}/diff?path=c.rs"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn comments_reach_the_owning_worker() {
    let h = harness().await;
    h.center.refresh().await;
    let (_, list) = call(&h.app, "GET", "/v1/pull-requests", None).await;
    let id = list[0]["id"].as_str().unwrap();

    let (status, _) = call(
        &h.app,
        "POST",
        &format!("/v1/pull-requests/{id}/comments"),
        Some(json!({"text": "Use a constant here.", "path": "src/a.rs", "line": 12})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let writes = h.engine.writes();
    let StubWrite::Message { task_id, text } = &writes[0] else {
        panic!("expected a message, got {writes:?}");
    };
    assert_eq!(task_id, "ship-pr-center");
    assert!(text.starts_with(&format!(
        "Review comment on your pull request {URL} on src/a.rs line 12:"
    )));
    assert!(text.contains("Use a constant here."));

    for bad in [
        json!({"text": "  "}),
        json!({"text": "x", "line": 3}),
        json!({"text": "x", "path": "a.rs", "line": 0}),
    ] {
        let (status, _) = call(
            &h.app,
            "POST",
            &format!("/v1/pull-requests/{id}/comments"),
            Some(bad.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    assert_eq!(h.engine.writes().len(), 1);
}

#[tokio::test]
async fn merge_goes_through_the_engine_and_reads_back() {
    let h = harness().await;
    h.forge
        .set(URL, forge_pr(PullRequestState::Open, CheckStatus::Success));
    h.center.refresh().await;
    let (_, list) = call(&h.app, "GET", "/v1/pull-requests", None).await;
    let id = list[0]["id"].as_str().unwrap().to_string();

    // A refusal from the engine is a 409 with its reasons.
    h.engine
        .fail_writes(Some("checks are not green at head abc123"));
    let (status, body) = call(
        &h.app,
        "POST",
        &format!("/v1/pull-requests/{id}:merge"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "merge_refused");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("not green"));
    h.engine.fail_writes(None);

    let mut merged = forge_pr(PullRequestState::Merged, CheckStatus::Success);
    merged.merged_at = Some("2026-10-02T17:00:00Z".into());
    h.forge.set(URL, merged);
    let (status, pr) = call(
        &h.app,
        "POST",
        &format!("/v1/pull-requests/{id}:merge"),
        Some(json!({"method": "rebase"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{pr}");
    assert_eq!(pr["state"], "merged");
    assert_eq!(
        h.engine.writes(),
        vec![StubWrite::MergePullRequest {
            task_id: "ship-pr-center".into(),
            url: URL.into(),
            method: Some(MergeMethod::Rebase),
        }]
    );

    // Merging again is refused before the engine runs.
    let (status, body) = call(
        &h.app,
        "POST",
        &format!("/v1/pull-requests/{id}:merge"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "merge_refused");
    assert_eq!(h.engine.writes().len(), 1);

    let (status, _) = call(
        &h.app,
        "POST",
        &format!("/v1/pull-requests/{id}:close"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn standing_approval_merges_green_pull_requests_once_per_head() {
    let h = harness().await;
    h.forge
        .set(URL, forge_pr(PullRequestState::Open, CheckStatus::Success));
    h.center.refresh().await;
    assert!(h.engine.writes().is_empty(), "off by default");

    let (status, project) = call(
        &h.app,
        "PATCH",
        &format!("/v1/projects/{}", h.project_id),
        Some(json!({"standing_approval": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{project}");
    assert_eq!(project["standing_approval"], true);
    assert_eq!(
        h.engine.writes(),
        vec![StubWrite::StandingApproval {
            repos: vec!["quark".into()],
            on: true,
        }]
    );

    // The merge is refused (say, a hold); it is not retried on the same head.
    h.engine.fail_writes(Some("held for the captain"));
    h.center.refresh().await;
    h.center.refresh().await;
    h.engine.fail_writes(None);
    h.center.refresh().await;
    assert_eq!(h.engine.writes().len(), 1);

    // A new head is tried again.
    let mut moved = forge_pr(PullRequestState::Open, CheckStatus::Success);
    moved.head_sha = Some("def456".into());
    h.forge.set(URL, moved);
    h.center.refresh().await;
    let writes = h.engine.writes();
    assert_eq!(writes.len(), 2);
    assert!(matches!(
        &writes[1],
        StubWrite::MergePullRequest { method: None, .. }
    ));

    // A red pull request is never merged.
    let mut red = forge_pr(PullRequestState::Open, CheckStatus::Failure);
    red.head_sha = Some("0ff".into());
    h.forge.set(URL, red);
    h.center.refresh().await;
    assert_eq!(h.engine.writes().len(), 2);

    // Turning it off reaches the engine too.
    let (status, _) = call(
        &h.app,
        "PATCH",
        &format!("/v1/projects/{}", h.project_id),
        Some(json!({"standing_approval": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        h.engine.writes()[2],
        StubWrite::StandingApproval {
            repos: vec!["quark".into()],
            on: false,
        }
    );
}

#[tokio::test]
async fn gate_evidence_and_artifacts() {
    use quark_systems::{
        ArtifactKind, Evidence, EvidenceArtifact, Gate, GateCase, GateKind, GateState,
    };
    let h = harness().await;
    h.forge
        .set(URL, forge_pr(PullRequestState::Open, CheckStatus::Success));
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("journeys")).unwrap();
    std::fs::write(dir.path().join("journeys/trace.zip"), b"PK-trace").unwrap();
    let artifact = |kind, path: &str| EvidenceArtifact {
        id: path.into(),
        kind,
        name: path.rsplit('/').next().unwrap().into(),
        content_type: "application/zip".into(),
        size_bytes: Some(8),
        url: String::new(),
    };
    let evidence = Evidence {
        head_sha: Some("abc123".into()),
        state: GateState::Failed,
        stale: false,
        started_at: None,
        completed_at: None,
        gates: vec![Gate {
            kind: GateKind::Journeys,
            state: GateState::Failed,
            summary: Some("1 of 1 failed".into()),
            started_at: None,
            completed_at: None,
            cases: vec![GateCase {
                name: "merge a PR".into(),
                state: GateState::Failed,
                duration_ms: Some(1200),
                message: Some("timed out".into()),
                artifacts: vec![
                    artifact(ArtifactKind::Trace, "journeys/trace.zip"),
                    artifact(ArtifactKind::Screenshot, "journeys/gone.png"),
                ],
            }],
        }],
    };
    h.engine
        .set_evidence("ship-pr-center", evidence, dir.path().to_path_buf());
    h.center.refresh().await;

    let (_, list) = call(&h.app, "GET", "/v1/pull-requests", None).await;
    let pr = &list[0];
    let e = &pr["evidence"];
    assert_eq!(e["state"], "failed");
    assert_eq!(e["stale"], false);
    assert_eq!(e["gates"][0]["kind"], "journeys");
    let trace = &e["gates"][0]["cases"][0]["artifacts"][0];
    assert_eq!(trace["kind"], "trace");
    let url = trace["url"].as_str().unwrap().to_string();
    assert!(url.starts_with(&format!(
        "/v1/pull-requests/{}/evidence/artifacts/",
        pr["id"].as_str().unwrap()
    )));

    let req = Request::builder().uri(&url).body(Body::empty()).unwrap();
    let res = h.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["content-type"], "application/zip");
    assert_eq!(res.headers()["content-security-policy"], "sandbox");
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"PK-trace");

    // A listed file that is gone, and ids the evidence does not list.
    let gone = e["gates"][0]["cases"][0]["artifacts"][1]["url"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, _) = call(&h.app, "GET", &gone, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let id = pr["id"].as_str().unwrap();
    let escape = quarkd::store::artifact_id("../../etc/passwd");
    let (status, _) = call(
        &h.app,
        "GET",
        &format!("/v1/pull-requests/{id}/evidence/artifacts/{escape}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Unchanged evidence emits nothing; a moved head marks it stale.
    let seq = h.store.last_seq().unwrap();
    h.center.refresh().await;
    assert_eq!(h.store.last_seq().unwrap(), seq);
    let mut moved = forge_pr(PullRequestState::Open, CheckStatus::Success);
    moved.head_sha = Some("def456".into());
    h.forge.set(URL, moved);
    h.center.refresh().await;
    let (_, one) = call(&h.app, "GET", &format!("/v1/pull-requests/{id}"), None).await;
    assert_eq!(one["evidence"]["stale"], true);
}

#[tokio::test]
async fn a_merged_pull_request_moves_its_task_to_done_live() {
    let h = harness().await;
    let mut rx = h.store.subscribe();
    let mut merged = forge_pr(PullRequestState::Merged, CheckStatus::Success);
    merged.merged_at = Some("2026-10-02T17:00:00Z".into());
    h.forge.set(URL, merged);
    h.center.refresh().await;

    let task = &h.store.list_tasks(&h.project_id).unwrap()[0];
    assert_eq!(task.state, TaskState::Done);
    let mut changed = None;
    while let Ok(event) = rx.try_recv() {
        if event.event_type == EventType::TaskStateChanged {
            changed = Some(event);
        }
    }
    let changed = changed.expect("the board is told the task moved");
    assert_eq!(changed.payload["task"]["state"], "done");
    assert_eq!(changed.payload["previous_state"], "in_review");

    // The engine cleans the task up; the board keeps it done.
    h.store
        .apply_snapshot(&h.project_id, &FleetSnapshot { tasks: vec![] })
        .unwrap();
    assert_eq!(
        h.store.list_tasks(&h.project_id).unwrap()[0].state,
        TaskState::Done
    );
}
