//! The Project dashboard's Settings tab: every per-Project switch in one
//! read, and the switches the daemon owns changed through it.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use quark_systems::{AgentConfig, CreateProject, DispatchPreset, ProjectStatus, RepoSource};
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
    assert!(o.status.success(), "git {args:?}");
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
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn settings_read_every_switch_and_change_holdout_and_standing_approval() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (bare, checkout) = (dir.path().join("p.git"), workspace.join("project"));

    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = Arc::new(StubEngine::new());
    let harnesses = Arc::new(quarkd::harness::HarnessRegistry::new(
        quarkd::harness::builtin(),
        quarkd::harness::HostEnv::default(),
    ));
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
        events: quark_eventlog::SqliteEventLog::open(":memory:").unwrap(),
        triggers: None,
        compaction: Default::default(),
        beads: Default::default(),
    });

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
                model: Some("claude-sonnet-5".into()),
                effort: Some("medium".into()),
                pool: Some("max".into()),
            }),
            dispatch_preset: Some(DispatchPreset::LightTrivial),
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
    // One holdout category for the app.
    let run = checkout.join("holdout/app/smoke/run");
    std::fs::create_dir_all(run.parent().unwrap()).unwrap();
    std::fs::write(&run, "#!/bin/sh\n").unwrap();
    git(&checkout, &["add", "--all"]);
    git(&checkout, &["commit", "-qm", "holdout"]);
    git(&checkout, &["push", "-q", "origin", "HEAD:main"]);

    let uri = format!("/v1/projects/{}/settings", project.id);
    let (status, s) = call(&app, "GET", &uri, None).await;
    assert_eq!(status, 200, "{s}");
    assert_eq!(s["standing_approval"], false);
    assert_eq!(s["delivery"], "gated");
    assert_eq!(s["agent_config"]["pool"], "max");
    assert_eq!(s["dispatch"]["rules"], 1);
    assert_eq!(s["dispatch"]["default_candidates"], 1);
    assert_eq!(s["dispatch"]["classifier"], "none");
    assert_eq!(s["memory"], json!({"entries": 0, "proposals_to_review": 0}));
    let revision = s["verification"]["revision"].as_str().unwrap().to_string();
    assert_eq!(revision, git(&bare, &["rev-parse", "main:project.yaml"]));
    assert_eq!(
        s["verification"]["sources"],
        json!([{
            "source": "app", "checks": [], "journeys": null,
            "holdout": {"enabled": true, "categories": ["smoke"], "timeout_s": null}
        }])
    );

    // Holdout off: one commit of project.yaml, and the gates compiled from
    // main no longer run the app's holdout tests.
    let commits = git(&bare, &["rev-list", "--count", "main"]);
    let off = json!({"revision": revision, "holdout": [{"source": "app", "enabled": false}]});
    let (status, s) = call(&app, "PATCH", &uri, Some(off.clone())).await;
    assert_eq!(status, 200, "{s}");
    assert_eq!(s["verification"]["sources"][0]["holdout"]["enabled"], false);
    assert_ne!(s["verification"]["revision"], json!(revision));
    assert_eq!(
        git(&bare, &["rev-list", "--count", "main"]),
        (commits.parse::<u32>().unwrap() + 1).to_string()
    );
    let declared = quarkd::gates::read_declared(&bare).unwrap().unwrap();
    let gates =
        quarkd::gates::compile(&declared.project_yaml, &bare, &declared.holdout_sources).unwrap();
    assert!(gates.repos.get("app").is_none_or(|g| g.holdout.is_none()));

    // The old revision is refused, as is a source the file does not have.
    let (status, e) = call(&app, "PATCH", &uri, Some(off)).await;
    assert_eq!(
        (status.as_u16(), &e["error"]["code"]),
        (409, &json!("settings_changed"))
    );
    let unknown = json!({"holdout": [{"source": "web", "enabled": false}]});
    let (status, e) = call(&app, "PATCH", &uri, Some(unknown)).await;
    assert_eq!(
        (status.as_u16(), &e["error"]["code"]),
        (400, &json!("settings_invalid"))
    );

    // Standing approval reaches the engine for the Project's repos.
    let (status, s) = call(
        &app,
        "PATCH",
        &uri,
        Some(json!({"standing_approval": true})),
    )
    .await;
    assert_eq!(status, 200, "{s}");
    assert_eq!(s["standing_approval"], true);
    assert!(engine.writes().iter().any(|w| matches!(
        w,
        StubWrite::StandingApproval { repos, on: true } if repos == &["app".to_string()]
    )));
    assert!(store.get_project(&project.id).unwrap().standing_approval);

    let (status, _) = call(&app, "GET", "/v1/projects/prj_missing/settings", None).await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn compaction_is_set_in_settings_and_shown_with_the_coordinator_context() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let harnesses = Arc::new(quarkd::harness::HarnessRegistry::new(
        quarkd::harness::builtin(),
        quarkd::harness::HostEnv::default(),
    ));
    let chat = Arc::new(quarkd::chat::RecordingInput::new());
    let app = quarkd::api::router(quarkd::api::AppState {
        store: store.clone(),
        engine: Arc::new(StubEngine::new()),
        harnesses: harnesses.clone(),
        accounts: Arc::new(quarkd::accounts::Accounts::new(
            store.clone(),
            harnesses,
            Arc::new(quarkd::accounts::StubQuota::new()),
            &["CLAUDE_CONFIG_DIR"],
        )),
        sessions: quarkd::sessions::Sessions::disabled("not used in this test"),
        layout: quarkd::provision::Layout::new(dir.path()),
        chat: chat.clone(),
        forge: Arc::new(quarkd::forge::StubForge::new()),
        events: quark_eventlog::SqliteEventLog::open(":memory:").unwrap(),
        triggers: None,
        compaction: Default::default(),
        beads: Default::default(),
    });
    let project = store
        .create_project(CreateProject {
            name: "Quark".into(),
            workspace_path: Some(dir.path().to_string_lossy().into_owned()),
            agent_config: Some(AgentConfig {
                harness: "claude-code".into(),
                model: Some("opus[1m]".into()),
                effort: Some("high".into()),
                pool: None,
            }),
            ..Default::default()
        })
        .unwrap();

    let context = format!("/v1/coordinators/{}/context", project.id);
    let (status, c) = call(&app, "GET", &context, None).await;
    assert_eq!(status, 200, "{c}");
    assert_eq!(c["agent_config"]["model"], "opus[1m]");
    assert_eq!(c["agent_config"]["effort"], "high");
    assert_eq!(c["window_tokens"], 1_000_000);
    assert_eq!(c["context_tokens"], Value::Null);
    assert_eq!(c["compaction"], json!({"auto": true, "at_tokens": 140_000}));
    assert_eq!(c["compacting"], "idle");

    let settings = format!("/v1/projects/{}/settings", project.id);
    let (status, e) = call(
        &app,
        "PATCH",
        &settings,
        Some(json!({"compaction": {"auto": true, "at_tokens": 500}})),
    )
    .await;
    assert_eq!(status, 400, "{e}");
    let (status, s) = call(
        &app,
        "PATCH",
        &settings,
        Some(json!({"compaction": {"auto": false, "at_tokens": 300_000}})),
    )
    .await;
    assert_eq!(status, 200, "{s}");
    assert_eq!(
        s["compaction"],
        json!({"auto": false, "at_tokens": 300_000})
    );
    let (_, c) = call(&app, "GET", &context, None).await;
    assert_eq!(
        c["compaction"],
        json!({"auto": false, "at_tokens": 300_000})
    );

    // Compact now types the harness's /compact, with what to keep.
    let (status, a) = call(
        &app,
        "POST",
        &format!("/v1/coordinators/{}:compact", project.id),
        None,
    )
    .await;
    assert_eq!(status, 202, "{a}");
    let (_, c) = call(&app, "GET", &context, None).await;
    assert_eq!(c["compacting"], "compacting");
    for _ in 0..50 {
        if !chat.sent().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let sent = chat.sent();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].1.starts_with("/compact Keep"), "{}", sent[0].1);
}
