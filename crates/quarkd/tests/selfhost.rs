//! The self-hosting Project under `selfhost/`: its create request must pass
//! the daemon's own validation, and its `project.yaml` must agree with that
//! request and compile into the engine's gate config.

use std::path::{Path, PathBuf};

use quark_systems::CreateProject;

fn selfhost() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../selfhost")
}

fn read(name: &str) -> String {
    std::fs::read_to_string(selfhost().join(name)).unwrap()
}

#[test]
fn create_request_is_valid() {
    let req: CreateProject = serde_json::from_str(&read("project.json")).unwrap();
    let req = quarkd::provision::normalize(req).unwrap();
    let names: Vec<_> = req.repos.iter().filter_map(|r| r.name.as_deref()).collect();
    assert_eq!(names, ["quark", "firstmate"]);
    assert!(req.agent_config.is_some());
}

#[test]
fn project_yaml_matches_the_request_and_compiles() {
    let req: CreateProject = serde_json::from_str(&read("project.json")).unwrap();
    let yaml = read("project.yaml")
        .replace("@ID@", "prj_selfhost")
        .replace("@CREATED_AT@", "2026-10-02T00:00:00Z");
    let doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(&yaml).unwrap();

    assert_eq!(doc["id"].as_str(), Some("prj_selfhost"));
    assert_eq!(doc["name"].as_str(), Some(req.name.as_str()));
    assert_eq!(doc["goal"].as_str(), req.goal.as_deref());
    let sources = doc["workspace"]["sources"].as_sequence().unwrap();
    let declared: Vec<_> = sources
        .iter()
        .map(|s| (s["name"].as_str().unwrap(), s["url"].as_str().unwrap()))
        .collect();
    let requested: Vec<_> = req
        .repos
        .iter()
        .map(|r| (r.name.as_deref().unwrap(), r.url.as_str()))
        .collect();
    assert_eq!(declared, requested);
    let agent = req.agent_config.as_ref().unwrap();
    assert_eq!(
        doc["agent_config"]["harness"].as_str(),
        Some(agent.harness.as_str())
    );
    assert_eq!(
        doc["agent_config"]["effort"].as_str(),
        agent.effort.as_deref()
    );
    let policy = serde_json::to_value(req.delivery.unwrap()).unwrap();
    assert_eq!(doc["delivery"]["policy"].as_str(), policy.as_str());

    let holdout = ["quark".to_string()];
    let gates = quarkd::gates::compile(&yaml, Path::new("/q/projects/p.git"), &holdout).unwrap();
    let json = serde_json::to_value(&gates).unwrap();
    let quark = &json["repos"]["quark"];
    let checks: Vec<_> = quark["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        checks,
        [
            "rustfmt",
            "clippy",
            "cargo test",
            "app build and unit tests"
        ]
    );
    assert_eq!(quark["journeys"]["start"], "npm run e2e:serve");
    assert_eq!(quark["holdout"]["path"], "holdout/quark");
    let firstmate = &json["repos"]["firstmate"];
    assert_eq!(firstmate["checks"].as_array().unwrap().len(), 2);
    assert!(
        firstmate.get("holdout").is_none(),
        "holdout only where tests exist"
    );
}

#[test]
fn the_journeys_start_script_exists() {
    let app = selfhost().join("../app");
    let pkg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(app.join("package.json")).unwrap()).unwrap();
    assert_eq!(pkg["scripts"]["e2e:serve"], "node scripts/e2e-serve.mjs");
    assert!(app.join("scripts/e2e-serve.mjs").is_file());
}
