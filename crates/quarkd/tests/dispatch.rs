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
    assert_eq!(
        all[2],
        json!({"classifier": {"provider": "none"}, "rules": [], "default": {"harness": "pi"}})
    );
    assert_eq!(calls()[0], (true, None));
}

fn exe(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The API over a Project with a Project repo, on a machine with Claude Code
/// installed and logged in, and no Codex.
struct Served {
    app: axum::Router,
    store: Arc<Store>,
    engine: Arc<StubEngine>,
    project: quark_systems::Project,
    bare: std::path::PathBuf,
    checkout: std::path::PathBuf,
}

fn served(dir: &Path) -> Served {
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (bare, checkout) = (dir.join("p.git"), workspace.join("project"));

    let (bin, home) = (dir.join("bin"), dir.join("home"));
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
        layout: quarkd::provision::Layout::new(dir),
        chat: Arc::new(quarkd::chat::RecordingInput::new()),
        forge: Arc::new(quarkd::forge::StubForge::new()),
        events: quark_eventlog::SqliteEventLog::open(":memory:").unwrap(),
        triggers: None,
        beads: Default::default(),
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
    Served {
        app,
        store,
        engine,
        project,
        bare,
        checkout,
    }
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

async fn post(app: &axum::Router, uri: &str, body: Value) -> (axum::http::StatusCode, Value) {
    call(app, "POST", uri, Some(body)).await
}

fn out(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}");
    String::from_utf8(o.stdout).unwrap()
}

#[tokio::test]
async fn a_description_is_tested_against_the_rules_on_main() {
    use quark_systems::{DispatchCandidate, DispatchChoice, DispatchRule, DispatchStatus};
    use quarkd::engine::EngineResolution;

    let dir = tempfile::tempdir().unwrap();
    let Served {
        app,
        engine,
        project,
        checkout,
        ..
    } = served(dir.path());
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
        fallback: None,
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
        fallback: None,
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

#[tokio::test]
async fn rules_are_read_saved_as_a_commit_and_tested_as_a_draft() {
    let dir = tempfile::tempdir().unwrap();
    let Served {
        app,
        store,
        engine,
        project,
        bare,
        checkout,
    } = served(dir.path());
    let uri = format!("/v1/projects/{}/dispatch", project.id);

    // What project creation wrote, with Quark's harness ids.
    let (status, rules) = call(&app, "GET", &uri, None).await;
    assert_eq!(status, 200, "{rules}");
    assert_eq!(rules["project_id"], json!(project.id));
    assert_eq!(rules["classifier"], json!({"provider": "none"}));
    assert_eq!(rules["default_select"], "ordered");
    assert_eq!(
        rules["rules"],
        json!([{
            "name": "trivial-edit",
            "when": "A trivial mechanical edit such as a rename, typo or one-line fix.",
            "candidates": [{"harness": "claude-code", "model": "claude-sonnet-5", "effort": "low", "pool": null}],
            "select": null
        }])
    );
    assert_eq!(rules["default"][0]["effort"], "medium");
    let first = rules["revision"].as_str().unwrap().to_string();
    assert_eq!(
        first,
        out(&bare, &["rev-parse", "main:dispatch.yaml"]).trim()
    );
    assert_eq!(rules["commit"], out(&bare, &["rev-parse", "main"]).trim());

    // Saving what is there already makes no commit.
    let unchanged = json!({
        "revision": first,
        "default_select": rules["default_select"],
        "rules": rules["rules"],
        "default": rules["default"],
    });
    let (status, same) = call(&app, "PUT", &uri, Some(unchanged)).await;
    assert_eq!(status, 200, "{same}");
    assert_eq!(same["revision"], json!(first));
    assert_eq!(out(&bare, &["rev-list", "--count", "main"]).trim(), "1");

    // An edit: a second candidate, a new rule ahead of the first, another
    // default_select.
    let edit = json!({
        "revision": first,
        "default_select": "quota-balanced",
        "rules": [
            {
                "name": "big",
                "when": "A feature that spans \"many\" files.",
                "select": "ordered",
                "candidates": [{"harness": "claude-code", "model": "claude-opus-5", "effort": "high", "pool": "max"}]
            },
            {
                "name": "trivial-edit",
                "when": "A trivial mechanical edit such as a rename, typo or one-line fix.",
                "candidates": [
                    {"harness": "claude-code", "model": "claude-sonnet-5", "effort": "low"},
                    {"harness": "codex", "model": "gpt-5.5", "effort": "low", "pool": ""}
                ]
            }
        ],
        "default": rules["default"],
    });

    // Tested before it is saved: the draft's candidates, compiled and not
    // written, and the engine's resolution, which reads the saved rules, is
    // not run.
    let test_uri = format!("{uri}:test");
    let ask = |draft: Option<&Value>| {
        let mut body = json!({"description": "Rename a field."});
        if let Some(d) = draft {
            body["draft"] = d.clone();
        }
        body
    };
    let (status, t) = post(&app, &test_uri, ask(Some(&edit))).await;
    assert_eq!(status, 200, "{t}");
    assert!(engine.described().is_empty());
    assert_eq!(t["resolution"]["status"], "not_consulted");
    assert!(
        t["summary"]
            .as_str()
            .unwrap()
            .starts_with("These rules are not saved"),
        "{t}"
    );
    let c = t["candidates"].as_array().unwrap();
    assert_eq!(c.len(), 4);
    assert_eq!(
        (&c[0]["rule_name"], &c[0]["pool"]),
        (&json!("big"), &json!("max"))
    );
    assert_eq!(c[2]["harness"], "codex");
    assert_eq!(c[2]["passed"], false);
    assert_eq!(out(&bare, &["rev-list", "--count", "main"]).trim(), "1");
    // A draft that is the saved rules is resolved as they are.
    let (status, t) = post(&app, &test_uri, ask(Some(&same))).await;
    assert_eq!(status, 200, "{t}");
    assert_eq!(engine.described(), ["Rename a field."]);
    assert_eq!(t["candidates"].as_array().unwrap().len(), 2);
    // A draft that does not compile is refused.
    let (status, e) = post(
        &app,
        &test_uri,
        ask(Some(
            &json!({"rules": [{"when": "x", "candidates": []}], "default": []}),
        )),
    )
    .await;
    assert_eq!(
        (status.as_u16(), &e["error"]["code"]),
        (400, &json!("dispatch_invalid"))
    );

    // Saved: one commit on main that changes only dispatch.yaml.
    let (status, saved) = call(&app, "PUT", &uri, Some(edit.clone())).await;
    assert_eq!(status, 200, "{saved}");
    let second = saved["revision"].as_str().unwrap().to_string();
    assert_ne!(second, first);
    assert_eq!(saved["rules"][0]["name"], "big");
    assert_eq!(saved["rules"][1]["candidates"][1]["pool"], Value::Null);
    assert_eq!(saved["classifier"], json!({"provider": "none"}));
    let head = out(&bare, &["rev-parse", "main"]);
    assert_eq!(saved["commit"], head.trim());
    assert_eq!(
        out(&bare, &["show", "--name-only", "--format=%s", "main"]),
        "Update dispatch rules\n\ndispatch.yaml\n"
    );
    let file = out(&bare, &["show", "main:dispatch.yaml"]);
    assert_eq!(
        file,
        "# Dispatch rules for this Project, created from the \"light_trivial\" preset.\n\
         # No classifier block here: the default in ~/.quark/config.yaml applies, and with\n\
         # none there (provider: none) the coordinator picks the rule for each task.\n\
         # A classifier block in this file overrides that default field by field.\n\
         default_select: \"quota-balanced\"\n\
         rules:\n\
         \x20 - name: \"big\"\n\
         \x20   when: \"A feature that spans \\\"many\\\" files.\"\n\
         \x20   select: \"ordered\"\n\
         \x20   use:\n\
         \x20     - { harness: \"claude-code\", model: \"claude-opus-5\", effort: \"high\", pool: \"max\" }\n\
         \x20 - name: \"trivial-edit\"\n\
         \x20   when: \"A trivial mechanical edit such as a rename, typo or one-line fix.\"\n\
         \x20   use:\n\
         \x20     - { harness: \"claude-code\", model: \"claude-sonnet-5\", effort: \"low\" }\n\
         \x20     - { harness: \"codex\", model: \"gpt-5.5\", effort: \"low\" }\n\
         default:\n\
         \x20 - { harness: \"claude-code\", model: \"claude-sonnet-5\", effort: \"medium\" }\n"
    );
    assert_eq!(
        std::fs::read_to_string(checkout.join("dispatch.yaml")).unwrap(),
        file
    );
    let (_, read) = call(&app, "GET", &uri, None).await;
    assert_eq!(read, saved);

    // The saved rules reach the engine on the next refresh.
    let projector =
        Projector::new(store.clone(), engine.clone()).with_session_roots(Default::default());
    projector.refresh_all().await.unwrap();
    let applied = configs(&engine);
    assert_eq!(applied.len(), 1, "{applied:?}");
    assert_eq!(applied[0]["rules"][0]["name"], "big");
    assert_eq!(applied[0]["rules"][1]["use"][1]["harness"], "codex");
    assert_eq!(applied[0]["default_select"], "quota-balanced");

    // An edit that started from an older revision is refused, as is one
    // that does not compile; neither commits.
    let (status, e) = call(&app, "PUT", &uri, Some(edit.clone())).await;
    assert_eq!(
        (status.as_u16(), &e["error"]["code"]),
        (409, &json!("dispatch_changed"))
    );
    let mut bad = edit.clone();
    bad["revision"] = json!(second);
    bad["rules"][1]["name"] = json!("big");
    let (status, e) = call(&app, "PUT", &uri, Some(bad)).await;
    assert_eq!(
        (status.as_u16(), &e["error"]["code"]),
        (400, &json!("dispatch_invalid"))
    );
    assert_eq!(
        e["error"]["message"],
        "dispatch.yaml: rule 2 (big): another rule has this name"
    );
    let (status, e) = call(
        &app,
        "PUT",
        &uri,
        Some(json!({"rules": [{"when": " ", "candidates": [{"harness": "pi"}]}]})),
    )
    .await;
    assert_eq!(status, 400, "{e}");
    assert_eq!(
        e["error"]["message"],
        "dispatch.yaml: rule 1: when is empty"
    );
    assert_eq!(out(&bare, &["rev-parse", "main"]), head);

    // An edit of the file left uncommitted in the checkout is not overwritten.
    std::fs::write(checkout.join("dispatch.yaml"), "rules: [] # by hand\n").unwrap();
    let (status, e) = call(&app, "PUT", &uri, Some(json!({"rules": []}))).await;
    assert_eq!(
        (status.as_u16(), &e["error"]["code"]),
        (409, &json!("uncommitted_changes"))
    );
    git(&checkout, &["checkout", "-q", "--", "dispatch.yaml"]);

    // A file on main that does not compile is reported, not replaced.
    push_dispatch(&checkout, "rules: [{ when: \"x\" }]\n");
    for (method, body) in [("GET", None), ("PUT", Some(json!({"rules": []})))] {
        let (status, e) = call(&app, method, &uri, body).await;
        assert_eq!(
            (status.as_u16(), &e["error"]["code"]),
            (409, &json!("dispatch_invalid")),
            "{method}"
        );
    }
    let (status, _) = call(&app, "GET", "/v1/projects/prj_nope/dispatch", None).await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn the_user_default_classifier_applies_until_the_project_overrides_it() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (bare, checkout) = (dir.path().join("p.git"), workspace.join("project"));
    let user_config = dir.path().join("config.yaml");

    let store = Arc::new(Store::open_in_memory().unwrap());
    let project = store
        .create_project(CreateProject {
            name: "Quark".into(),
            workspace_path: Some(workspace.to_string_lossy().into_owned()),
            agent_config: Some(AgentConfig {
                harness: "codex".into(),
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
    let engine = Arc::new(StubEngine::new());
    let projector = Projector::new(store.clone(), engine.clone())
        .with_session_roots(Default::default())
        .with_user_config(user_config.clone());
    let classifiers = || -> Vec<Value> {
        configs(&engine)
            .into_iter()
            .map(|c| c["classifier"].clone())
            .collect()
    };

    // No settings file and no block in the Project: provider none.
    projector.refresh_all().await.unwrap();
    assert_eq!(classifiers(), [json!({"provider": "none"})]);

    // A user-level default reaches a Project that names no classifier, without
    // a change to the Project repo.
    std::fs::write(
        &user_config,
        "classifier:\n  provider: system1\n  model: jev-1\n\
         \x20 credential: keychain:quark/classifier\n",
    )
    .unwrap();
    projector.refresh_all().await.unwrap();
    projector.refresh_all().await.unwrap();
    let all = classifiers();
    assert_eq!(all.len(), 2, "{all:?}");
    assert_eq!(
        all[1],
        json!({
            "provider": "system1",
            "model": "jev-1",
            "credential": "keychain:quark/classifier",
            "confidence_floor": 0.6,
            "timeout_ms": 5000,
            "on_failure": "coordinator"
        })
    );

    // The Project's block overrides it field by field.
    push_dispatch(
        &checkout,
        "classifier:\n  confidence_floor: 0.8\n  timeout_ms: 900\n  on_failure: default\n\
         default: { harness: \"codex\" }\n",
    );
    projector.refresh_all().await.unwrap();
    let c = classifiers().pop().unwrap();
    assert_eq!(c["model"], "jev-1");
    assert_eq!(c["confidence_floor"], 0.8);
    assert_eq!(c["timeout_ms"], 900);
    assert_eq!(c["on_failure"], "default");

    // An inline key is refused wherever it is written, never echoed, and the
    // last good config stays.
    let applied = configs(&engine).len();
    let failure = || {
        let (ok, detail) = store
            .recent_adapter_calls(&project.id, "dispatch", 10)
            .unwrap()
            .remove(0);
        assert!(!ok);
        detail.unwrap()
    };
    std::fs::write(&user_config, "classifier:\n  credential: sk-live-abc123\n").unwrap();
    projector.refresh_all().await.unwrap();
    let detail = failure();
    assert!(
        detail.contains("config.yaml: classifier: credential must be a keychain reference"),
        "{detail}"
    );
    assert!(!detail.contains("sk-live"), "{detail}");
    std::fs::remove_file(&user_config).unwrap();
    push_dispatch(
        &checkout,
        "classifier:\n  provider: system1\n  model: open\n  credential: sk-live-abc123\n",
    );
    projector.refresh_all().await.unwrap();
    let detail = failure();
    assert!(
        detail.contains("dispatch.yaml: classifier: credential must be a keychain reference"),
        "{detail}"
    );
    assert!(!detail.contains("sk-live"), "{detail}");
    assert_eq!(configs(&engine).len(), applied);

    // A Project can turn the classifier off whatever the default says.
    std::fs::write(
        &user_config,
        "classifier:\n  provider: system1\n  model: open\n",
    )
    .unwrap();
    push_dispatch(&checkout, "classifier:\n  provider: none\n");
    projector.refresh_all().await.unwrap();
    assert_eq!(classifiers().pop().unwrap(), json!({"provider": "none"}));
}

#[tokio::test]
async fn the_api_answers_the_classifier_in_effect_and_a_fallback_to_the_default_rule() {
    use quark_systems::{DispatchChoice, DispatchStatus};
    use quarkd::engine::EngineResolution;

    let dir = tempfile::tempdir().unwrap();
    let Served {
        app,
        engine,
        project,
        checkout,
        ..
    } = served(dir.path());
    let rules = format!("/v1/projects/{}/dispatch", project.id);

    // The user-level default shows on a Project that names no classifier,
    // and the Project's block overrides it.
    std::fs::write(
        dir.path().join("config.yaml"),
        "classifier:\n  provider: system1\n  model: jev-1\n  credential: keychain:quark/classifier\n",
    )
    .unwrap();
    let (status, got) = call(&app, "GET", &rules, None).await;
    assert_eq!(status, 200, "{got}");
    assert_eq!(got["classifier"]["provider"], "system1");
    assert_eq!(got["classifier"]["credential"], "keychain:quark/classifier");
    assert_eq!(got["classifier"]["on_failure"], "coordinator");
    push_dispatch(
        &checkout,
        "classifier:\n  on_failure: default\n  timeout_ms: 800\n\
         default:\n  - { harness: \"claude-code\", model: \"claude-sonnet-5\" }\n",
    );
    let (_, got) = call(&app, "GET", &rules, None).await;
    assert_eq!(got["classifier"]["model"], "jev-1");
    assert_eq!(got["classifier"]["on_failure"], "default");
    assert_eq!(got["classifier"]["timeout_ms"], 800);

    // The classifier timed out and on_failure default resolved the default
    // rule: the default rule decides, and the outcome says why.
    engine.set_description_resolution(Ok(EngineResolution {
        status: DispatchStatus::Clear,
        rule: None,
        reason: None,
        notes: vec![],
        candidates: vec![],
        profile: Some(DispatchChoice {
            harness: "claude".into(),
            model: Some("claude-sonnet-5".into()),
            effort: None,
            account: None,
        }),
        fallback: Some("classifier request timed out after 800ms".into()),
        classifier_consulted: false,
        classifier_model: None,
        confidence: None,
        output: None,
    }));
    let uri = format!("/v1/projects/{}/dispatch:test", project.id);
    let (status, t) = call(
        &app,
        "POST",
        &uri,
        Some(json!({"description": "Fix a typo."})),
    )
    .await;
    assert_eq!(status, 200, "{t}");
    assert_eq!(t["decided_by"], "default_rule");
    assert_eq!(t["rule"]["id"], "default");
    assert_eq!(t["classifier"]["provider"], "system1");
    assert_eq!(t["classifier"]["model"], Value::Null);
    assert_eq!(t["chosen"]["harness"], "claude-code");
    let summary = t["summary"].as_str().unwrap();
    assert!(
        summary.starts_with(
            "The classifier's answer was not used (classifier request timed out after 800ms), \
             so the default rule applied (on_failure: default)."
        ),
        "{summary}"
    );

    // An inline key in the user-level file is refused, not echoed.
    std::fs::write(
        dir.path().join("config.yaml"),
        "classifier:\n  credential: sk-live-abc123\n",
    )
    .unwrap();
    let (status, err) = call(&app, "GET", &rules, None).await;
    assert_eq!(status, 409, "{err}");
    assert_eq!(err["error"]["code"], "dispatch_invalid");
    assert!(!err.to_string().contains("sk-live"), "{err}");
}
