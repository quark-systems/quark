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
