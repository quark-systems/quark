//! Adding repos and local folders to a Project after it was created, and
//! taking them out again.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use quark_systems::{AgentConfig, CreateProject, ProjectStatus, RepoSource};
use quarkd::engine::{StubEngine, StubWrite};
use quarkd::store::Store;
use serde_json::{json, Value};

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@x"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn app(store: Arc<Store>, engine: Arc<StubEngine>, home: &Path) -> axum::Router {
    let harnesses = Arc::new(quarkd::harness::HarnessRegistry::new(
        quarkd::harness::builtin(),
        quarkd::harness::HostEnv::default(),
    ));
    quarkd::api::router(quarkd::api::AppState {
        store: store.clone(),
        engine,
        harnesses: harnesses.clone(),
        accounts: Arc::new(quarkd::accounts::Accounts::new(
            store,
            harnesses,
            Arc::new(quarkd::accounts::StubQuota::new()),
            &["CLAUDE_CONFIG_DIR"],
        )),
        sessions: quarkd::sessions::Sessions::disabled("not used in this test"),
        layout: quarkd::provision::Layout::new(home),
        chat: Arc::new(quarkd::chat::RecordingInput::new()),
        forge: Arc::new(quarkd::forge::StubForge::new()),
        events: quark_eventlog::SqliteEventLog::open(":memory:").unwrap(),
        triggers: None,
        compaction: Default::default(),
        beads: Default::default(),
        coordinator: None,
    })
}

fn sources(bare: &Path) -> Vec<String> {
    let yaml = git(bare, &["show", "main:project.yaml"]);
    let doc: Value = serde_yaml_ng::from_str(&yaml).unwrap();
    doc["workspace"]["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn repos_are_added_and_removed_after_creation() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (bare, checkout) = (dir.path().join("p.git"), workspace.join("project"));
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = Arc::new(StubEngine::new());
    let app = app(store.clone(), engine.clone(), dir.path());

    let project = store
        .create_project(CreateProject {
            name: "Quark".into(),
            workspace_path: Some(workspace.to_string_lossy().into_owned()),
            repos: vec![RepoSource {
                url: "https://github.com/acme/app".into(),
                name: Some("app".into()),
            }],
            agent_config: Some(AgentConfig {
                harness: "claude-code".into(),
                model: None,
                effort: None,
                pool: None,
            }),
            ..Default::default()
        })
        .unwrap();
    quarkd::project_repo::init(&bare, &checkout, &project).unwrap();
    store
        .set_project_status(
            &project.id,
            ProjectStatus::Ready,
            None,
            None,
            Some(bare.to_str().unwrap()),
        )
        .unwrap();
    let uri = format!("/v1/projects/{}/repos", project.id);

    // A local folder: cloned into the workspace, recorded in project.yaml
    // and on the Project, with its name taken from the path.
    let (status, p) = call(&app, "POST", &uri, Some(json!({"url": "/src/notes"}))).await;
    assert_eq!(status, 200, "{p}");
    assert_eq!(
        p["repos"],
        json!([
            {"url": "https://github.com/acme/app", "name": "app"},
            {"url": "/src/notes", "name": "notes"}
        ])
    );
    let writes = engine.writes();
    assert!(writes.iter().any(|w| matches!(
        w, StubWrite::AddSource { name, url } if name == "notes" && url == "/src/notes"
    )));
    assert!(writes.iter().any(|w| matches!(
        w, StubWrite::SeedWorkspace { sources, .. } if sources == &["notes".to_string()]
    )));
    assert_eq!(sources(&bare), ["app", "notes"]);
    assert_eq!(store.get_project(&project.id).unwrap().repos.len(), 2);

    // The same name again is refused, as is a URL that is not one.
    let (status, e) = call(&app, "POST", &uri, Some(json!({"url": "/elsewhere/notes"}))).await;
    assert_eq!(
        (status.as_u16(), &e["error"]["code"]),
        (409, &json!("repo_exists"))
    );
    let (status, _) = call(&app, "POST", &uri, Some(json!({"url": "-x"}))).await;
    assert_eq!(status, 400);

    // A clone with a commit on no remote is kept.
    let origin = dir.path().join("notes-origin.git");
    git(
        dir.path(),
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            origin.to_str().unwrap(),
        ],
    );
    let clone = workspace.join("repos/notes");
    git(
        dir.path(),
        &[
            "clone",
            "-q",
            origin.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    git(&clone, &["commit", "-q", "--allow-empty", "-m", "one"]);
    let (status, e) = call(&app, "DELETE", &format!("{uri}/notes"), None).await;
    assert_eq!(
        (status.as_u16(), &e["error"]["code"]),
        (409, &json!("unlanded_work")),
        "{e}"
    );
    assert!(clone.is_dir());
    assert_eq!(sources(&bare), ["app", "notes"]);

    // Pushed, it is set aside and leaves project.yaml and the Project.
    git(&clone, &["push", "-q", "origin", "HEAD:main"]);
    let (status, p) = call(&app, "DELETE", &format!("{uri}/notes"), None).await;
    assert_eq!(status, 200, "{p}");
    assert_eq!(
        p["repos"],
        json!([{"url": "https://github.com/acme/app", "name": "app"}])
    );
    assert!(!clone.exists());
    let kept: Vec<_> = std::fs::read_dir(workspace.join(".quark/removed-repos"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(kept.len() == 1 && kept[0].starts_with("notes-"), "{kept:?}");
    assert_eq!(sources(&bare), ["app"]);

    // A repo the Project does not have.
    let (status, _) = call(&app, "DELETE", &format!("{uri}/notes"), None).await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn a_project_without_repos_is_provisioned_by_its_first() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = Arc::new(StubEngine::new());
    let app = app(store.clone(), engine.clone(), dir.path());
    let project = store
        .create_project(CreateProject {
            name: "Later".into(),
            agent_config: Some(AgentConfig {
                harness: "claude-code".into(),
                model: None,
                effort: None,
                pool: None,
            }),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(project.status, ProjectStatus::Ready);
    let uri = format!("/v1/projects/{}/repos", project.id);
    let (status, p) = call(
        &app,
        "POST",
        &uri,
        Some(json!({"url": "git@github.com:acme/app.git"})),
    )
    .await;
    assert_eq!(status, 200, "{p}");
    assert_eq!(p["status"], "provisioning");
    assert_eq!(p["repos"][0]["name"], "app");

    // While it is being set up, its repos stay as they are.
    let (status, e) = call(&app, "POST", &uri, Some(json!({"url": "/src/x"}))).await;
    if status.as_u16() == 409 {
        assert_eq!(e["error"]["code"], "project_busy");
    }
}
