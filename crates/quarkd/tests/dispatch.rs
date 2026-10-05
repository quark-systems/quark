//! Dispatch profiles (ADR-11): each refresh compiles the Project repo's
//! `dispatch.yaml` on `main` into the engine's crew dispatch config.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use quark_systems::{AgentConfig, CreateProject, DispatchPreset, ProjectStatus};
use quarkd::engine::{StubEngine, StubWrite};
use quarkd::projector::Projector;
use quarkd::store::Store;
use serde_json::{json, Value};

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@x"])
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

/// Commit `dispatch.yaml` in the checkout and push it to `main`.
fn push_dispatch(checkout: &Path, yaml: &str) {
    std::fs::write(checkout.join("dispatch.yaml"), yaml).unwrap();
    git(checkout, &["commit", "-qam", "dispatch"]);
    git(checkout, &["push", "-q", "origin", "HEAD:main"]);
}

fn configs(engine: &StubEngine) -> Vec<Value> {
    engine
        .writes()
        .into_iter()
        .filter_map(|w| match w {
            StubWrite::CrewDispatch { config, .. } => Some(serde_json::from_str(&config).unwrap()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn dispatch_yaml_on_main_reaches_the_engine_and_bad_files_keep_the_last_good_config() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (bare, checkout) = (dir.path().join("p.git"), workspace.join("project"));

    let store = Arc::new(Store::open_in_memory().unwrap());
    let project = store
        .create_project(CreateProject {
            name: "Quark".into(),
            workspace_path: Some(workspace.to_string_lossy().into_owned()),
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
    let engine = Arc::new(StubEngine::new());
    let projector =
        Projector::new(store.clone(), engine.clone()).with_session_roots(Default::default());
    let calls = || {
        store
            .recent_adapter_calls(&project.id, "dispatch", 10)
            .unwrap()
    };

    // The file project creation wrote compiles, harness ids mapped to the
    // engine's and the account pool left out, and is applied once.
    projector.refresh_all().await.unwrap();
    projector.refresh_all().await.unwrap();
    assert_eq!(
        configs(&engine),
        [json!({
            "classifier": {"provider": "none"},
            "default_select": "ordered",
            "rules": [{
                "name": "trivial-edit",
                "when": "A trivial mechanical edit such as a rename, typo or one-line fix.",
                "use": [{"harness": "claude", "model": "claude-sonnet-5", "effort": "low"}]
            }],
            "default": [{"harness": "claude", "model": "claude-sonnet-5", "effort": "medium"}]
        })]
    );
    assert_eq!(calls(), [(true, None)]);

    // A change on main reaches the engine on the next refresh.
    push_dispatch(
        &checkout,
        "classifier:\n  provider: \"none\"\ndefault_select: \"quota-balanced\"\nrules:\n\
         - name: \"big\"\n  when: \"A big feature.\"\n  select: \"ordered\"\n  use:\n\
         \x20   - { harness: \"claude-code\", effort: \"high\" }\n\
         \x20   - { harness: \"codex\", model: \"gpt-5.5\", effort: \"high\" }\n\
         default: { harness: \"codex\" }\n",
    );
    projector.refresh_all().await.unwrap();
    let all = configs(&engine);
    assert_eq!(all.len(), 2, "{all:?}");
    assert_eq!(
        all[1]["rules"][0]["use"],
        json!([
            {"harness": "claude", "effort": "high"},
            {"harness": "codex", "model": "gpt-5.5", "effort": "high"}
        ])
    );
    assert_eq!(all[1]["rules"][0]["select"], "ordered");
    assert_eq!(all[1]["default"], json!({"harness": "codex"}));
    assert_eq!(all[1]["default_select"], "quota-balanced");

    // An invalid file is recorded once as a failed adapter call and nothing
    // reaches the engine, so its last good config stays.
    push_dispatch(&checkout, "rules:\n  - when: \"x\"\n    use: []\n");
    projector.refresh_all().await.unwrap();
    projector.refresh_all().await.unwrap();
    assert_eq!(configs(&engine).len(), 2);
    let (ok, detail) = calls().remove(0);
    assert!(!ok);
    let detail = detail.unwrap();
    assert!(
        detail.contains("dispatch.yaml: rule 1: use: needs at least one profile"),
        "{detail}"
    );
    assert_eq!(calls().len(), 3, "the bad file is recorded once");

    // An engine failure is recorded and retried on the next refresh.
    push_dispatch(&checkout, "default: { harness: \"pi\" }\n");
    engine.fail_writes(Some("fm-crew-dispatch.sh exited with Some(2)"));
    projector.refresh_all().await.unwrap();
    assert_eq!(configs(&engine).len(), 2);
    assert!(!calls()[0].0);
    engine.fail_writes(None);
    projector.refresh_all().await.unwrap();
    let all = configs(&engine);
    assert_eq!(all.len(), 3);
    assert_eq!(all[2], json!({"rules": [], "default": {"harness": "pi"}}));
    assert_eq!(calls()[0], (true, None));
}

fn exe(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

async fn post(app: &axum::Router, uri: &str, body: Value) -> (axum::http::StatusCode, Value) {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let req = axum::http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn a_description_is_tested_against_the_rules_on_main() {
    use quark_systems::{DispatchCandidate, DispatchChoice, DispatchRule, DispatchStatus};
    use quarkd::engine::EngineResolution;

    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (bare, checkout) = (dir.path().join("p.git"), workspace.join("project"));

    // A machine with Claude Code installed and logged in, and no Codex.
    let (bin, home) = (dir.path().join("bin"), dir.path().join("home"));
    exe(&bin.join("claude"), "echo '2.1.200 (Claude Code)'");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::write(home.join(".claude/.credentials.json"), "{}").unwrap();
    let harnesses = Arc::new(quarkd::harness::HarnessRegistry::new(
        quarkd::harness::builtin(),
        quarkd::harness::HostEnv {
            path: Some(bin.into_os_string()),
            home: Some(home),
            vars: Default::default(),
        },
    ));

    let store = Arc::new(Store::open_in_memory().unwrap());
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
        layout: quarkd::provision::Layout::new(dir.path()),
        chat: Arc::new(quarkd::chat::RecordingInput::new()),
        forge: Arc::new(quarkd::forge::StubForge::new()),
    });

    let project = store
        .create_project(CreateProject {
            name: "Quark".into(),
            workspace_path: Some(workspace.to_string_lossy().into_owned()),
            agent_config: Some(AgentConfig {
                harness: "claude-code".into(),
                model: Some("claude-sonnet-5".into()),
                effort: Some("medium".into()),
                pool: None,
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
    push_dispatch(
        &checkout,
        "classifier:\n  provider: \"none\"\nrules:\n\
         - name: \"trivial-edit\"\n  when: \"A trivial mechanical edit.\"\n  use:\n\
         \x20   - { harness: \"claude-code\", model: \"claude-sonnet-5\", effort: \"low\" }\n\
         \x20   - { harness: \"codex\", model: \"gpt-5.5\", effort: \"low\" }\n\
         default:\n  - { harness: \"claude-code\", model: \"claude-sonnet-5\", effort: \"medium\" }\n",
    );
    let uri = format!("/v1/projects/{}/dispatch:test", project.id);
    let ask = json!({"description": "  Rename a field in the settings struct.\n"});

    // No classifier: the coordinator would pick, and every profile the
    // rules list is a candidate with its four checks.
    let off = EngineResolution {
        status: DispatchStatus::Off,
        rule: None,
        reason: None,
        notes: vec![],
        candidates: vec![],
        profile: None,
        classifier_consulted: false,
        classifier_model: None,
        confidence: None,
        output: None,
    };
    engine.set_description_resolution(Ok(off.clone()));
    let (status, t) = post(&app, &uri, ask.clone()).await;
    assert_eq!(status, 200, "{t}");
    assert_eq!(
        engine.described(),
        ["Rename a field in the settings struct."]
    );
    assert_eq!(t["project_id"], json!(project.id));
    assert_eq!(t["decided_by"], "coordinator");
    assert_eq!(t["classifier"]["provider"], "none");
    assert_eq!(t["resolution"]["status"], "off");
    assert_eq!(t["rule"], Value::Null);
    assert_eq!(t["chosen"], Value::Null);
    assert_eq!(
        t["summary"],
        "No classifier is configured (provider: none), so the coordinator would pick. \
         2 of 3 candidates could start a worker now."
    );
    let c = t["candidates"].as_array().unwrap();
    assert_eq!(c.len(), 3);
    assert_eq!(c[0]["rule"]["id"], "rule_1");
    assert_eq!(c[0]["rule_name"], "trivial-edit");
    assert_eq!(c[0]["passed"], true);
    assert_eq!(c[0]["reason"], "eligible");
    let kinds: Vec<_> = c[0]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["check"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "harness_installed",
            "model_accepted",
            "account_health",
            "quota_headroom"
        ]
    );
    assert_eq!(c[1]["harness"], "codex");
    assert_eq!(c[1]["passed"], false);
    assert!(c[1]["reason"]
        .as_str()
        .unwrap()
        .starts_with("Codex is not installed"));
    assert_eq!(c[2]["rule"]["id"], "default");
    assert_eq!(c[2]["effort"], "medium");

    // A classifier that matches a rule: the rule, the selected profile, and
    // the candidates the resolution weighed.
    engine.set_description_resolution(Ok(EngineResolution {
        status: DispatchStatus::Clear,
        rule: Some(DispatchRule {
            id: "rule_1".into(),
            when: Some("A trivial mechanical edit.".into()),
        }),
        candidates: vec![
            DispatchCandidate {
                harness: "claude".into(),
                model: Some("claude-sonnet-5".into()),
                passed: true,
                reason: "eligible".into(),
                evidence: Some("provider=claude scope=all_models remaining=79%".into()),
            },
            DispatchCandidate {
                harness: "codex".into(),
                model: Some("gpt-5.5".into()),
                passed: true,
                reason: "eligible".into(),
                evidence: None,
            },
        ],
        profile: Some(DispatchChoice {
            harness: "claude".into(),
            model: Some("claude-sonnet-5".into()),
            effort: Some("low".into()),
            account: None,
        }),
        classifier_consulted: true,
        classifier_model: Some("jev-1.13.0".into()),
        confidence: Some(0.9),
        output: Some("dispatch-resolve:\n  status: clear".into()),
        ..off
    }));
    let (status, t) = post(&app, &uri, ask.clone()).await;
    assert_eq!(status, 200, "{t}");
    assert_eq!(t["decided_by"], "classifier");
    assert_eq!(t["rule"]["id"], "rule_1");
    assert_eq!(t["classifier"]["provider"], "system1");
    assert_eq!(t["classifier"]["confidence"], 0.9);
    assert_eq!(t["chosen"]["harness"], "claude-code");
    assert_eq!(t["chosen"]["effort"], "low");
    let c = t["candidates"].as_array().unwrap();
    assert_eq!(c.len(), 2);
    assert_eq!(
        c[0]["evidence"],
        "provider=claude scope=all_models remaining=79%"
    );
    assert_eq!(
        c[1]["passed"], false,
        "eligible by quota, but not installed"
    );

    // A resolution that fails still lists the candidates.
    engine.set_description_resolution(Err("quota-axi timed out".into()));
    let (status, t) = post(&app, &uri, ask.clone()).await;
    assert_eq!(status, 200, "{t}");
    assert_eq!(t["resolution"]["status"], "error");
    assert_eq!(t["decided_by"], "coordinator");
    assert_eq!(t["candidates"].as_array().unwrap().len(), 3);

    // Refusals: no description, an unknown Project, rules that do not compile.
    let (status, e) = post(&app, &uri, json!({"description": " "})).await;
    assert_eq!(
        (status.as_u16(), &e["error"]["code"]),
        (400, &json!("invalid_request"))
    );
    let (status, _) = post(&app, "/v1/projects/prj_nope/dispatch:test", ask.clone()).await;
    assert_eq!(status, 404);
    push_dispatch(&checkout, "rules: [{ when: \"x\" }]\n");
    let (status, e) = post(&app, &uri, ask).await;
    assert_eq!(
        (status.as_u16(), &e["error"]["code"]),
        (409, &json!("dispatch_invalid"))
    );
    assert!(e["error"]["message"]
        .as_str()
        .unwrap()
        .starts_with("dispatch.yaml: "));
}
