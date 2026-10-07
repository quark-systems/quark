//! Project settings (the dashboard's Settings tab): read the verification
//! gates `project.yaml` on the Project repo's `main` declares, and turn a
//! source's holdout tests on or off in it.
//!
//! Turning holdout off writes `holdout: false` under the source's
//! `verification`; turning it back on removes that, so the tests run again
//! whenever `holdout/<source>/` exists. A `holdout` with settings (a
//! `timeout_s`) already means on and is kept. The file is rewritten from its
//! parsed form, so comments in it are not kept.

use std::path::Path;

use quark_systems::VerificationSettings;
use quark_systems::{GateCheck, GateJourneys, HoldoutChange, HoldoutSettings, SourceVerification};
use serde::Deserialize;
use serde_yaml_ng::{Mapping, Value};

use crate::gates::{self, git, HOLDOUT_DIR};

/// The file holding a Project's gates.
pub const FILE: &str = "project.yaml";

#[derive(Debug, Default, Deserialize)]
struct Doc {
    #[serde(default)]
    workspace: Option<Workspace>,
}

#[derive(Debug, Default, Deserialize)]
struct Workspace {
    #[serde(default)]
    sources: Vec<Source>,
}

#[derive(Debug, Deserialize)]
struct Source {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    verification: Option<Verification>,
}

#[derive(Debug, Default, Deserialize)]
struct Verification {
    #[serde(default)]
    checks: Vec<gates::Check>,
    #[serde(default)]
    journeys: Option<gates::Journeys>,
    #[serde(default)]
    holdout: Option<Value>,
}

/// What `main` of the bare Project repo at `bare` declares. A repo without
/// `project.yaml` declares nothing; one whose file does not compile says why
/// in `error`.
pub fn verification(bare: &Path) -> VerificationSettings {
    let Ok(revision) = git(
        bare,
        &["rev-parse", "--verify", "--quiet", &format!("main:{FILE}")],
    ) else {
        return VerificationSettings::default();
    };
    let failed = |error: String| VerificationSettings {
        revision: Some(revision.clone()),
        sources: Vec::new(),
        error: Some(error),
    };
    let text = match git(bare, &["show", &format!("main:{FILE}")]) {
        Ok(t) => t,
        Err(e) => return failed(e),
    };
    // The gate compiler is the authority on what the file may say.
    if let Err(e) = gates::compile(&text, bare, &[]) {
        return failed(e);
    }
    let doc: Doc = match serde_yaml_ng::from_str(&text) {
        Ok(d) => d,
        Err(e) => return failed(format!("{FILE}: {e}")),
    };
    let sources = doc
        .workspace
        .unwrap_or_default()
        .sources
        .into_iter()
        .filter_map(|s| {
            let name = s.name.filter(|n| !n.is_empty())?;
            let v = s.verification.unwrap_or_default();
            let (enabled, timeout_s) = match &v.holdout {
                Some(Value::Bool(on)) => (*on, None),
                Some(Value::Mapping(m)) => (true, m.get("timeout_s").and_then(Value::as_u64)),
                _ => (true, None),
            };
            Some(SourceVerification {
                checks: v
                    .checks
                    .into_iter()
                    .map(|c| GateCheck {
                        name: c.name,
                        run: c.run,
                        timeout_s: c.timeout_s,
                    })
                    .collect(),
                journeys: v.journeys.map(|j| GateJourneys {
                    start: j.start,
                    url: j.url,
                    dir: j.dir,
                }),
                holdout: HoldoutSettings {
                    enabled,
                    categories: categories(bare, &name),
                    timeout_s,
                },
                source: name,
            })
        })
        .collect();
    VerificationSettings {
        revision: Some(revision),
        sources,
        error: None,
    }
}

/// Category directories under `holdout/<source>/` on `main`.
fn categories(bare: &Path, source: &str) -> Vec<String> {
    let prefix = format!("{HOLDOUT_DIR}/{source}/");
    git(bare, &["ls-tree", "-d", "--name-only", "main", &prefix])
        .map(|out| {
            out.lines()
                .filter_map(|l| l.strip_prefix(&prefix))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// `project_yaml` with `changes` applied, or the same text when they change
/// nothing. A change naming no source of the file is refused.
pub fn set_holdout(project_yaml: &str, changes: &[HoldoutChange]) -> Result<String, String> {
    let mut doc: Value =
        serde_yaml_ng::from_str(project_yaml).map_err(|e| format!("{FILE}: {e}"))?;
    let mut changed = false;
    for change in changes {
        let source = doc
            .get_mut("workspace")
            .and_then(|w| w.get_mut("sources"))
            .and_then(Value::as_sequence_mut)
            .and_then(|s| {
                s.iter_mut()
                    .find(|s| s.get("name").and_then(Value::as_str) == Some(&change.source))
            })
            .and_then(Value::as_mapping_mut)
            .ok_or_else(|| format!("{FILE} has no source named {:?}", change.source))?;
        changed |= apply(source, change.enabled)?;
    }
    if !changed {
        return Ok(project_yaml.to_string());
    }
    serde_yaml_ng::to_string(&doc).map_err(|e| e.to_string())
}

/// Turn holdout on or off in one source's mapping. Returns whether the
/// mapping changed.
fn apply(source: &mut Mapping, enabled: bool) -> Result<bool, String> {
    let verification = source
        .entry(Value::from("verification"))
        .or_insert_with(|| Value::Mapping(Mapping::new()));
    if verification.is_null() {
        *verification = Value::Mapping(Mapping::new());
    }
    let v = verification
        .as_mapping_mut()
        .ok_or("verification is not a mapping")?;
    let current = v.get("holdout");
    let on = !matches!(current, Some(Value::Bool(false)));
    if on == enabled {
        return Ok(false);
    }
    if enabled {
        v.remove("holdout");
    } else {
        v.insert(Value::from("holdout"), Value::Bool(false));
    }
    if v.is_empty() {
        source.remove("verification");
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const YAML: &str = r#"schema: "quark.project.v1"
workspace:
  sources:
    - name: "app"
      url: "https://github.com/acme/app"
      verification:
        checks:
          - { name: "test", run: "npm test" }
    - name: "lib"
      url: "https://github.com/acme/lib"
      verification:
        holdout: { timeout_s: 60 }
"#;

    fn change(source: &str, enabled: bool) -> HoldoutChange {
        HoldoutChange {
            source: source.into(),
            enabled,
        }
    }

    fn holdout(yaml: &str, source: &str) -> Option<Value> {
        let doc: Value = serde_yaml_ng::from_str(yaml).unwrap();
        doc["workspace"]["sources"]
            .as_sequence()
            .unwrap()
            .iter()
            .find(|s| s["name"].as_str() == Some(source))
            .unwrap()
            .get("verification")
            .and_then(|v| v.get("holdout"))
            .cloned()
    }

    #[test]
    fn turning_holdout_off_and_on_again_restores_the_default() {
        let off = set_holdout(YAML, &[change("app", false)]).unwrap();
        assert_eq!(holdout(&off, "app"), Some(Value::Bool(false)));
        // The rest of the source is kept.
        assert!(off.contains("npm test"));
        let on = set_holdout(&off, &[change("app", true)]).unwrap();
        assert_eq!(holdout(&on, "app"), None);
        assert!(on.contains("npm test"));
    }

    #[test]
    fn a_source_without_verification_gets_one_and_loses_it_again() {
        let yaml = "workspace:\n  sources:\n    - name: \"app\"\n      url: \"u\"\n";
        let off = set_holdout(yaml, &[change("app", false)]).unwrap();
        assert_eq!(holdout(&off, "app"), Some(Value::Bool(false)));
        let on = set_holdout(&off, &[change("app", true)]).unwrap();
        assert!(!on.contains("verification"), "{on}");
    }

    #[test]
    fn holdout_settings_already_mean_on() {
        assert_eq!(set_holdout(YAML, &[change("lib", true)]).unwrap(), YAML);
        let off = set_holdout(YAML, &[change("lib", false)]).unwrap();
        assert_eq!(holdout(&off, "lib"), Some(Value::Bool(false)));
    }

    #[test]
    fn a_change_that_changes_nothing_keeps_the_text() {
        assert_eq!(set_holdout(YAML, &[change("app", true)]).unwrap(), YAML);
    }

    #[test]
    fn an_unknown_source_is_refused() {
        let err = set_holdout(YAML, &[change("web", false)]).unwrap_err();
        assert!(err.contains("\"web\""), "{err}");
    }
}
