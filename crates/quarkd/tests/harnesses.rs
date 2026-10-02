use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use quark_systems::{AgentConfig, AgentRole, AuthState, Effort, HarnessInfo, ModelSelection};
use quarkd::api::{self, AppState};
use quarkd::engine::StubEngine;
use quarkd::harness::{Account, HarnessRegistry, HostEnv, LaunchError};
use quarkd::store::Store;
use serde_json::{json, Value};
use tower::ServiceExt;

fn exe(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// A fake machine: `claude` on PATH with a login, `codex` installed under
/// its fallback-free name with no login, kimi only in its home fallback.
fn fake_host(root: &Path) -> HostEnv {
    let bin = root.join("bin");
    let home = root.join("home");
    exe(&bin.join("claude"), "echo '2.1.200 (Claude Code)'");
    exe(&bin.join("codex"), "echo 'codex-cli 0.99.0' >&2");
    exe(&home.join(".kimi-code/bin/kimi"), "echo 'kimi 2.0.0'");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::write(home.join(".claude/.credentials.json"), "{}").unwrap();
    HostEnv {
        path: Some(bin.into_os_string()),
        home: Some(home),
        vars: HashMap::new(),
    }
}

fn config(harness: &str, model: Option<&str>, effort: Option<Effort>) -> AgentConfig {
    AgentConfig {
        harness: harness.into(),
        model: model.map(Into::into),
        effort: effort.map(|e| e.as_str().to_string()),
    }
}

#[tokio::test]
async fn detects_installed_harnesses_and_logins() {
    let dir = tempfile::tempdir().unwrap();
    let reg = HarnessRegistry::new(quarkd::harness::builtin(), fake_host(dir.path()));
    let infos = reg.list(true).await;
    let by_id = |id: &str| infos.iter().find(|h| h.id == id).unwrap().clone();

    let claude = by_id("claude-code");
    assert!(claude.install.installed);
    assert_eq!(
        claude.install.version.as_deref(),
        Some("2.1.200 (Claude Code)")
    );
    assert_eq!(claude.auth.state, AuthState::Configured);
    assert!(claude.transcript);
    assert_eq!(claude.account_env.as_deref(), Some("CLAUDE_CONFIG_DIR"));
    assert_eq!(claude.roles, [AgentRole::Coordinator, AgentRole::Worker]);

    let codex = by_id("codex");
    assert_eq!(codex.install.version.as_deref(), Some("codex-cli 0.99.0"));
    assert_eq!(codex.auth.state, AuthState::NotConfigured);

    assert!(by_id("kimi").install.installed);

    let bob = by_id("bob");
    assert!(!bob.install.installed);
    assert!(!bob.install.install_hint.is_empty());
    assert_eq!(bob.models.selection, ModelSelection::Automatic);
    assert!(bob.efforts.is_empty());
    assert_eq!(
        by_id("rovo").supervision.confidence,
        quark_systems::SupervisionConfidence::Low
    );
}

#[test]
fn validates_agent_configs() {
    let reg = HarnessRegistry::new(quarkd::harness::builtin(), HostEnv::default());
    let ok = reg.validate(
        &config("claude-code", Some("claude-sonnet-5"), Some(Effort::Medium)),
        AgentRole::Coordinator,
    );
    assert!(
        ok.valid && ok.errors.is_empty() && ok.warnings.is_empty(),
        "{ok:?}"
    );

    let codes = |c: AgentConfig, role| {
        let v = reg.validate(&c, role);
        (
            v.valid,
            v.errors.into_iter().map(|i| i.code).collect::<Vec<_>>(),
            v.warnings.into_iter().map(|i| i.code).collect::<Vec<_>>(),
        )
    };
    assert_eq!(
        codes(config("nope", None, None), AgentRole::Worker),
        (false, vec!["unknown_harness".into()], vec![])
    );
    assert_eq!(
        codes(config("gemini", None, None), AgentRole::Coordinator),
        (false, vec!["unsupported_role".into()], vec![])
    );
    assert_eq!(
        codes(config("grok", None, Some(Effort::Max)), AgentRole::Worker),
        (false, vec!["unsupported_effort".into()], vec![])
    );
    let mut bad = config("codex", None, None);
    bad.effort = Some("turbo".into());
    assert_eq!(
        codes(bad, AgentRole::Worker),
        (false, vec!["invalid_effort".into()], vec![])
    );
    assert_eq!(
        codes(config("opencode", Some("gpt-5"), None), AgentRole::Worker),
        (false, vec!["model_needs_provider".into()], vec![])
    );
    assert_eq!(
        codes(config("codex", Some("--yolo"), None), AgentRole::Worker),
        (false, vec!["invalid_model".into()], vec![])
    );
    assert_eq!(
        codes(
            config("bob", Some("x"), Some(Effort::High)),
            AgentRole::Worker
        ),
        (
            true,
            vec![],
            vec!["model_ignored".into(), "effort_ignored".into()]
        )
    );
}

#[test]
fn launch_plans_delegate_to_engine_adapters() {
    let reg = HarnessRegistry::new(quarkd::harness::builtin(), HostEnv::default());
    let wt = Path::new("/w/task-1");
    let acct = Account {
        config_dir: Some("/accounts/work/claude".into()),
    };
    let plan = reg
        .get("claude-code")
        .unwrap()
        .launch(
            &config("claude-code", Some("opus"), Some(Effort::High)),
            &acct,
            wt,
        )
        .unwrap();
    assert_eq!(plan.engine_harness, "claude");
    assert_eq!(plan.model.as_deref(), Some("opus"));
    assert_eq!(plan.effort, Some(Effort::High));
    assert_eq!(
        plan.env,
        [(
            "CLAUDE_CONFIG_DIR".to_string(),
            "/accounts/work/claude".to_string()
        )]
    );

    // Bob ignores model and effort, and has a single account.
    let bob = reg.get("bob").unwrap();
    let plan = bob
        .launch(
            &config("bob", Some("x"), Some(Effort::Low)),
            &Account::default(),
            wt,
        )
        .unwrap();
    assert_eq!((plan.model, plan.effort), (None, None));
    assert_eq!(
        bob.launch(&config("bob", None, None), &acct, wt),
        Err(LaunchError::SingleAccount("IBM Bob".into()))
    );
    assert!(matches!(
        reg.get("grok").unwrap().launch(
            &config("grok", None, Some(Effort::Xhigh)),
            &Account::default(),
            wt
        ),
        Err(LaunchError::Invalid(_))
    ));
}

#[test]
fn transcripts_follow_the_account_dir() {
    let env = HostEnv {
        home: Some("/home/u".into()),
        ..HostEnv::default()
    };
    let reg = HarnessRegistry::new(quarkd::harness::builtin(), env.clone());
    let codex = reg.get("codex").unwrap();
    assert_eq!(
        codex.transcript(&env, &Account::default()).unwrap().root,
        Path::new("/home/u/.codex/sessions")
    );
    let acct = Account {
        config_dir: Some("/accounts/b".into()),
    };
    assert_eq!(
        codex.transcript(&env, &acct).unwrap().root,
        Path::new("/accounts/b/sessions")
    );
    assert!(reg.get("bob").unwrap().transcript(&env, &acct).is_none());
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn harness_routes() {
    let dir = tempfile::tempdir().unwrap();
    let app = api::router(AppState {
        store: Arc::new(Store::open_in_memory().unwrap()),
        engine: Arc::new(StubEngine::new()),
        chat: Arc::new(quarkd::chat::RecordingInput::new()),
        forge: Arc::new(quarkd::forge::StubForge::new()),
        sessions: quarkd::sessions::Sessions::disabled("not used in this test"),
        layout: quarkd::provision::Layout::new(dir.path().join("quark-home")),
        harnesses: Arc::new(HarnessRegistry::new(
            quarkd::harness::builtin(),
            fake_host(dir.path()),
        )),
    });

    let (status, body) = call(&app, "GET", "/v1/harnesses?refresh=true", None).await;
    assert_eq!(status, StatusCode::OK);
    let infos: Vec<HarnessInfo> = serde_json::from_value(body).unwrap();
    assert_eq!(infos[0].id, "claude-code");
    assert!(infos[0].install.installed);
    assert_eq!(infos[0].efforts.len(), 5);

    let (status, body) = call(
        &app,
        "POST",
        "/v1/harnesses:validate",
        Some(json!({"config": {"harness": "pi", "effort": "xhigh"}, "role": "coordinator"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"valid": true, "errors": [], "warnings": []}));

    let (_, body) = call(
        &app,
        "POST",
        "/v1/harnesses:validate",
        Some(json!({"config": {"harness": "antigravity", "effort": "max"}})),
    )
    .await;
    assert_eq!(body["valid"], json!(false));
    assert_eq!(body["errors"][0]["field"], json!("effort"));
}
