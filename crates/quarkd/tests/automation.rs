//! A Project's automation over HTTP: the inbox, trigger rules and the away
//! policy, served by the slice 7 engine in shadow.

use std::sync::Arc;

use quark_systems::CreateProject;
use quarkd::engine::{StubEngine, StubWrite};
use quarkd::store::Store;
use serde_json::{json, Value};

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (axum::http::StatusCode, Value) {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let req = axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(match body {
            Some(b) => axum::body::Body::from(b.to_string()),
            None => axum::body::Body::empty(),
        })
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

struct Rig {
    _dir: tempfile::TempDir,
    app: axum::Router,
    engine: Arc<StubEngine>,
    shadow: quarkd::native_triggers::ShadowTriggers,
    project: String,
    home: std::path::PathBuf,
}

async fn rig(with_triggers: bool) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("workspace");
    std::fs::create_dir_all(home.join("state/inbox")).unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = Arc::new(StubEngine::new());
    let harnesses = Arc::new(quarkd::harness::HarnessRegistry::new(
        quarkd::harness::builtin(),
        quarkd::harness::HostEnv::default(),
    ));
    let events = quark_eventlog::SqliteEventLog::open(dir.path().join("events.db")).unwrap();
    let shadow =
        quarkd::native_triggers::ShadowTriggers::open(store.clone(), Arc::new(events.clone()))
            .await
            .unwrap();
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
        layout: quarkd::provision::Layout::new(dir.path()),
        chat: Arc::new(quarkd::chat::RecordingInput::new()),
        forge: Arc::new(quarkd::forge::StubForge::new()),
        events,
        triggers: with_triggers.then(|| shadow.engine()),
        beads: Default::default(),
    });
    let project = store
        .create_project(CreateProject {
            name: "Quark".into(),
            workspace_path: Some(home.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .unwrap();
    Rig {
        _dir: dir,
        app,
        engine,
        shadow,
        project: project.id,
        home,
    }
}

#[tokio::test]
async fn notes_go_to_the_coordinator_and_show_up_in_the_inbox() {
    let r = rig(true).await;
    let uri = format!("/v1/projects/{}/inbox", r.project);
    let (status, _) = call(&r.app, "POST", &uri, Some(json!({"body": "look at CI"}))).await;
    assert_eq!(status, 202);
    assert_eq!(
        r.engine.writes(),
        vec![StubWrite::InboxNote {
            text: "look at CI".into()
        }]
    );
    let (status, e) = call(&r.app, "POST", &uri, Some(json!({"body": "  "}))).await;
    assert_eq!(status, 400, "{e}");

    // firstmate writes the note; the next shadow pass mirrors it.
    std::fs::write(
        r.home.join("state/inbox/1-a.note"),
        "id=1-a\nat=2026-10-07T01:00:00Z\nsource=text\n--\nlook at CI\n",
    )
    .unwrap();
    r.shadow.pass().await.unwrap();
    let (status, a) = call(
        &r.app,
        "GET",
        &format!("/v1/projects/{}/automation", r.project),
        None,
    )
    .await;
    assert_eq!(status, 200, "{a}");
    assert_eq!(a["acting"], false);
    assert_eq!(a["inbox"][0]["body"], "look at CI");
    assert_eq!(a["inbox"][0]["at"], "2026-10-07T01:00:00Z");
    assert_eq!(a["away"]["posture"], "present");
    assert_eq!(a["away"]["routes"].as_array().unwrap().len(), 30);
}

#[tokio::test]
async fn rules_are_defined_listed_and_removed() {
    let r = rig(true).await;
    let uri = format!("/v1/projects/{}/triggers/nightly", r.project);
    let rule = json!({
        "description": "nudge the coordinator every night",
        "when": {"source": "every", "secs": 86400},
        "then": {"do": "wake", "note": "Check the nightly build."}
    });
    let (status, t) = call(&r.app, "PUT", &uri, Some(rule)).await;
    assert_eq!(status, 200, "{t}");
    assert_eq!(t["id"], "nightly");
    assert_eq!(t["enabled"], true);
    assert_eq!(t["when"]["secs"], 86400);
    assert_eq!(t["fires"], 0);

    let bad = json!({"when": {"source": "every", "secs": 0}, "then": {"do": "wake", "note": "x"}});
    let (status, _) = call(&r.app, "PUT", &uri, Some(bad)).await;
    assert_eq!(status, 400);
    let unknown = json!({"when": {"source": "sometimes"}, "then": {"do": "wake", "note": "x"}});
    let (status, _) = call(&r.app, "PUT", &uri, Some(unknown)).await;
    assert_eq!(status, 400);
    let bad_id = format!("/v1/projects/{}/triggers/no%20spaces", r.project);
    let ok_rule =
        json!({"when": {"source": "every", "secs": 60}, "then": {"do": "wake", "note": "x"}});
    let (status, _) = call(&r.app, "PUT", &bad_id, Some(ok_rule)).await;
    assert_eq!(status, 400);

    let (_, a) = call(
        &r.app,
        "GET",
        &format!("/v1/projects/{}/automation", r.project),
        None,
    )
    .await;
    assert_eq!(a["rules"].as_array().unwrap().len(), 1);

    let (status, _) = call(&r.app, "DELETE", &uri, None).await;
    assert_eq!(status, 204);
    let (status, _) = call(&r.app, "DELETE", &uri, None).await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn the_away_policy_is_replaced_and_guarded() {
    let r = rig(true).await;
    let uri = format!("/v1/projects/{}/away/policy", r.project);
    let change = json!({
        "digest_secs": 1800,
        "routes": [{"posture": "away", "occasion": "done", "wake": true, "user": "notify"}]
    });
    let (status, a) = call(&r.app, "PUT", &uri, Some(change)).await;
    assert_eq!(status, 200, "{a}");
    assert_eq!(a["digest_secs"], 1800);
    let routes = a["routes"].as_array().unwrap();
    let overridden: Vec<_> = routes.iter().filter(|r| r["overridden"] == true).collect();
    assert_eq!(overridden.len(), 1);
    assert_eq!(overridden[0]["occasion"], "done");
    assert_eq!(overridden[0]["user"], "notify");

    let lose = json!({
        "digest_secs": 1800,
        "routes": [{"posture": "away", "occasion": "decision", "wake": false, "user": "silent"}]
    });
    let (status, e) = call(&r.app, "PUT", &uri, Some(lose)).await;
    assert_eq!(status, 400, "{e}");
}

#[tokio::test]
async fn without_the_engine_it_says_so() {
    let r = rig(false).await;
    let (status, e) = call(
        &r.app,
        "GET",
        &format!("/v1/projects/{}/automation", r.project),
        None,
    )
    .await;
    assert_eq!(status, 503);
    assert_eq!(e["error"]["code"], "automation_off");
    let (status, _) = call(&r.app, "GET", "/v1/projects/nope/automation", None).await;
    assert_eq!(status, 404);
}
