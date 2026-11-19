//! A Project's repos after creation: add one to `project.yaml`'s
//! `workspace.sources` (with its GitHub issues tracker), or take one out.
//!
//! The file is rewritten from its parsed form, as the Settings tab's holdout
//! switch does, so comments in it are not kept. Everything else a source
//! declares (its `verification`) is kept for the others.

use serde_yaml_ng::{Mapping, Value};

use crate::project_repo::github_repo;
use crate::settings::FILE;

const TRACKER_KIND: &str = "github_issues";

/// `project_yaml` with the source `name` at `url` added, and its GitHub
/// issues tracker when the URL is a GitHub repo. Unchanged when a source of
/// that name and URL is already there; another URL under the name is refused.
pub fn add(project_yaml: &str, name: &str, url: &str) -> Result<String, String> {
    let mut doc = parse(project_yaml)?;
    let root = doc
        .as_mapping_mut()
        .ok_or_else(|| format!("{FILE} is not a mapping"))?;
    let workspace = mapping_at(root, "workspace")?;
    let sources = sequence_at(workspace, "sources")?;
    if let Some(existing) = sources.iter().find(|s| str_at(s, "name") == Some(name)) {
        return if str_at(existing, "url") == Some(url) {
            Ok(project_yaml.to_string())
        } else {
            Err(format!("{FILE} already has a source named {name:?}"))
        };
    }
    let mut source = Mapping::new();
    source.insert("name".into(), name.into());
    source.insert("url".into(), url.into());
    sources.push(Value::Mapping(source));
    if let Some(repo) = github_repo(url) {
        let trackers = sequence_at(root, "trackers")?;
        if !trackers.iter().any(|t| str_at(t, "repo") == Some(&repo)) {
            let mut tracker = Mapping::new();
            tracker.insert("kind".into(), TRACKER_KIND.into());
            tracker.insert("repo".into(), repo.into());
            trackers.push(Value::Mapping(tracker));
        }
    }
    serde_yaml_ng::to_string(&doc).map_err(|e| e.to_string())
}

/// `project_yaml` without the source `name`, and without its GitHub issues
/// tracker unless another source is the same GitHub repo. Unchanged when
/// there is no such source.
pub fn remove(project_yaml: &str, name: &str) -> Result<String, String> {
    let mut doc = parse(project_yaml)?;
    let Some(sources) = doc
        .get_mut("workspace")
        .and_then(|w| w.get_mut("sources"))
        .and_then(Value::as_sequence_mut)
    else {
        return Ok(project_yaml.to_string());
    };
    let Some(at) = sources.iter().position(|s| str_at(s, "name") == Some(name)) else {
        return Ok(project_yaml.to_string());
    };
    let removed = sources.remove(at);
    let still: Vec<String> = sources
        .iter()
        .filter_map(|s| str_at(s, "url").and_then(github_repo))
        .collect();
    if let Some(repo) = str_at(&removed, "url").and_then(github_repo) {
        if !still.contains(&repo) {
            if let Some(trackers) = doc.get_mut("trackers").and_then(Value::as_sequence_mut) {
                trackers.retain(|t| {
                    !(str_at(t, "repo") == Some(&repo) && str_at(t, "kind") == Some(TRACKER_KIND))
                });
            }
        }
    }
    serde_yaml_ng::to_string(&doc).map_err(|e| e.to_string())
}

fn parse(text: &str) -> Result<Value, String> {
    let doc: Value = serde_yaml_ng::from_str(text).map_err(|e| format!("{FILE}: {e}"))?;
    Ok(if doc.is_null() {
        Value::Mapping(Mapping::new())
    } else {
        doc
    })
}

fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// The mapping under `key`, created when absent or null.
fn mapping_at<'a>(m: &'a mut Mapping, key: &str) -> Result<&'a mut Mapping, String> {
    let v = m
        .entry(key.into())
        .or_insert_with(|| Value::Mapping(Mapping::new()));
    if v.is_null() {
        *v = Value::Mapping(Mapping::new());
    }
    v.as_mapping_mut()
        .ok_or_else(|| format!("{FILE}: {key} is not a mapping"))
}

/// The sequence under `key`, created when absent or null.
fn sequence_at<'a>(m: &'a mut Mapping, key: &str) -> Result<&'a mut Vec<Value>, String> {
    let v = m
        .entry(key.into())
        .or_insert_with(|| Value::Sequence(vec![]));
    if v.is_null() {
        *v = Value::Sequence(vec![]);
    }
    v.as_sequence_mut()
        .ok_or_else(|| format!("{FILE}: {key} is not a list"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const YAML: &str = r#"schema: "quark.project.v1"
id: "prj_1"
workspace:
  sources:
    - name: "app"
      url: "git@github.com:acme/app.git"
      verification:
        checks:
          - { name: "test", run: "npm test" }
trackers:
  - kind: "github_issues"
    repo: "acme/app"
delivery:
  policy: "gated"
"#;

    fn doc(text: &str) -> Value {
        serde_yaml_ng::from_str(text).unwrap()
    }

    fn names(text: &str) -> Vec<String> {
        doc(text)["workspace"]["sources"]
            .as_sequence()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap().to_string())
            .collect()
    }

    fn trackers(text: &str) -> Vec<String> {
        doc(text)["trackers"]
            .as_sequence()
            .map(|t| {
                t.iter()
                    .map(|t| t["repo"].as_str().unwrap().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn adds_a_github_source_with_its_tracker_and_keeps_the_rest() {
        let out = add(YAML, "lib", "https://github.com/acme/lib").unwrap();
        assert_eq!(names(&out), ["app", "lib"]);
        assert_eq!(trackers(&out), ["acme/app", "acme/lib"]);
        assert!(out.contains("npm test"));
        assert_eq!(doc(&out)["delivery"]["policy"], "gated");
        crate::gates::compile(&out, std::path::Path::new("/q/p.git"), &[]).unwrap();
    }

    #[test]
    fn a_local_folder_gets_no_tracker() {
        let out = add(YAML, "notes", "/Users/me/notes").unwrap();
        assert_eq!(names(&out), ["app", "notes"]);
        assert_eq!(trackers(&out), ["acme/app"]);
    }

    #[test]
    fn adding_again_changes_nothing_and_another_url_is_refused() {
        let out = add(YAML, "app", "git@github.com:acme/app.git").unwrap();
        assert_eq!(out, YAML);
        assert!(add(YAML, "app", "https://example.com/app.git").is_err());
    }

    #[test]
    fn adds_to_a_file_without_sources_or_trackers() {
        let out = add(
            "schema: \"quark.project.v1\"\n",
            "lib",
            "git@github.com:a/lib.git",
        )
        .unwrap();
        assert_eq!(names(&out), ["lib"]);
        assert_eq!(trackers(&out), ["a/lib"]);
        let out = add("workspace:\n  sources: []\ntrackers: []\n", "x", "/x").unwrap();
        assert_eq!(names(&out), ["x"]);
    }

    #[test]
    fn removes_a_source_and_its_tracker() {
        let two = add(YAML, "lib", "https://github.com/acme/lib").unwrap();
        let out = remove(&two, "app").unwrap();
        assert_eq!(names(&out), ["lib"]);
        assert_eq!(trackers(&out), ["acme/lib"]);
        assert!(!out.contains("npm test"));
    }

    #[test]
    fn keeps_a_tracker_another_source_still_uses() {
        let twice = add(YAML, "app2", "https://github.com/acme/app").unwrap();
        let out = remove(&twice, "app").unwrap();
        assert_eq!(names(&out), ["app2"]);
        assert_eq!(trackers(&out), ["acme/app"]);
    }

    #[test]
    fn removing_an_unknown_source_changes_nothing() {
        assert_eq!(remove(YAML, "nope").unwrap(), YAML);
        assert_eq!(remove("schema: x\n", "nope").unwrap(), "schema: x\n");
    }
}
