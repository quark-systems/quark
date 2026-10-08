//! Persona packs over the API: switching a Project's pack changes its role
//! names and labels, nothing else.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use quarkd::api::{self, AppState};
use quarkd::engine::StubEngine;
use quarkd::harness::{HarnessRegistry, HostEnv};
use quarkd::provision::Layout;
use quarkd::store::Store;
use serde_json::{json, Value};
use tower::ServiceExt;

fn app(home: &std::path::Path) -> Router {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let harnesses = Arc::new(HarnessRegistry::new(
        quarkd::harness::builtin(),
        HostEnv::default(),
    ));
    api::router(AppState {
        store: store.clone(),
        engine: Arc::new(StubEngine::new()),
        harnesses: harnesses.clone(),
        accounts: Arc::new(quarkd::accounts::Accounts::new(
            store,
            harnesses,
            Arc::new(quarkd::accounts::StubQuota::new()),
            &[],
        )),
        sessions: quarkd::sessions::Sessions::disabled("not used in this test"),
        layout: Layout::new(home),
        chat: Arc::new(quarkd::chat::RecordingInput::new()),
        forge: Arc::new(quarkd::forge::StubForge::new()),
        events: quark_eventlog::SqliteEventLog::open(":memory:").unwrap(),
        triggers: None,
        beads: Default::default(),
    })
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

async fn project(app: &Router) -> String {
    let (status, p) = call(app, "POST", "/v1/projects", Some(json!({"name": "Quark"}))).await;
    assert_eq!(status, StatusCode::CREATED);
    p["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn lists_the_builtin_packs_with_nautical_as_default() {
    let home = tempfile::tempdir().unwrap();
    let app = app(home.path());
    let (status, list) = call(&app, "GET", "/v1/personas", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["default"], "nautical");
    let ids: Vec<_> = list["packs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["plain", "nautical", "kitchen-brigade"]);
    assert_eq!(list["packs"][1]["roles"]["coordinator"], "first mate");
    assert_eq!(list["packs"][1]["builtin"], true);
    assert_eq!(list["errors"], json!([]));
}

#[tokio::test]
async fn switching_a_projects_pack_changes_its_labels() {
    let home = tempfile::tempdir().unwrap();
    let app = app(home.path());
    let id = project(&app).await;
    let uri = format!("/v1/projects/{id}/persona");

    let (status, p) = call(&app, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(p["persona"]["id"], "nautical");
    assert_eq!(p["persona"]["address"], "captain");
    assert_eq!(p["project_override"], Value::Null);

    let (status, p) = call(
        &app,
        "PUT",
        &uri,
        Some(json!({"persona": "kitchen-brigade"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(p["persona"]["roles"]["coordinator"], "expo");
    assert_eq!(p["persona"]["ui_labels"]["tasks"], "Tickets");
    assert_eq!(p["project_override"], "kitchen-brigade");

    // The Project itself is unchanged: the API keeps neutral names.
    let (_, proj) = call(&app, "GET", &format!("/v1/projects/{id}"), None).await;
    assert!(!proj.to_string().contains("kitchen"));

    // Changing the default leaves the override alone; clearing it follows the default.
    let (status, _) = call(
        &app,
        "PUT",
        "/v1/personas/default",
        Some(json!({"persona": "plain"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, p) = call(&app, "GET", &uri, None).await;
    assert_eq!(p["persona"]["id"], "kitchen-brigade");
    let (_, p) = call(&app, "PUT", &uri, Some(json!({"persona": null}))).await;
    assert_eq!(p["persona"]["id"], "plain");
    assert_eq!(p["default"], "plain");
}

#[tokio::test]
async fn a_pack_dropped_into_the_home_is_usable_without_a_release() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("personas/pirate");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("pack.toml"),
        "id = \"pirate\"\nname = \"Pirate\"\n[roles]\ncoordinator = \"quartermaster\"",
    )
    .unwrap();
    let app = app(home.path());
    let id = project(&app).await;
    let (status, p) = call(
        &app,
        "PUT",
        &format!("/v1/projects/{id}/persona"),
        Some(json!({"persona": "pirate"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(p["persona"]["roles"]["coordinator"], "quartermaster");
    assert_eq!(p["persona"]["builtin"], false);
}

#[tokio::test]
async fn refuses_unknown_packs_and_projects() {
    let home = tempfile::tempdir().unwrap();
    let app = app(home.path());
    let id = project(&app).await;
    let (status, body) = call(
        &app,
        "PUT",
        &format!("/v1/projects/{id}/persona"),
        Some(json!({"persona": "pirate"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("pirate"));
    let (status, _) = call(&app, "GET", "/v1/projects/nope/persona", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &app,
        "PUT",
        "/v1/personas/default",
        Some(json!({"persona": "pirate"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
