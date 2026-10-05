//! Verification gates (ADR-15): compile the Project repo's `project.yaml` into
//! the engine's gate config.
//!
//! Each workspace source can declare gates under `verification`:
//!
//! ```yaml
//! workspace:
//!   sources:
//!     - name: "app"
//!       url: "https://github.com/acme/app"
//!       verification:
//!         checks:
//!           - { name: "test", run: "npm test" }
//!         journeys:
//!           dir: "web"
//!           start: "npm run dev -- --port 5173"
//!           url: "http://127.0.0.1:5173/"
//!         holdout: { timeout_s: 600 }   # or false to turn it off
//! ```
//!
//! Holdout tests live in the Project repo under `holdout/<source>/`, one
//! directory per category, each with an executable `run`. They are on for a
//! source whenever that directory exists on `main`, unless `holdout: false`.
//! The gate runner clones them from the bare Project repo into a checkout no
//! worker's worktree contains; workers learn only category and pass/fail.
//!
//! The engine runs the gates and writes the evidence; this module only turns
//! the declaration into `fm.gates.v1` JSON for `fm-gates.sh config-set`.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

pub const CONFIG_SCHEMA: &str = "fm.gates.v1";

/// Where holdout tests live in the Project repo.
pub const HOLDOUT_DIR: &str = "holdout";

#[derive(Debug, Deserialize)]
struct ProjectYaml {
    #[serde(default)]
    workspace: Option<WorkspaceYaml>,
}

#[derive(Debug, Deserialize)]
struct WorkspaceYaml {
    #[serde(default)]
    sources: Vec<SourceYaml>,
}

#[derive(Debug, Deserialize)]
struct SourceYaml {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    verification: Option<Verification>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Verification {
    #[serde(default)]
    checks: Vec<Check>,
    #[serde(default)]
    journeys: Option<Journeys>,
    #[serde(default)]
    holdout: Option<HoldoutSetting>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub name: String,
    pub run: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Journeys {
    pub start: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub specs: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_timeout_s: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum HoldoutSetting {
    Enabled(bool),
    Settings(HoldoutYaml),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HoldoutYaml {
    #[serde(default)]
    timeout_s: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Holdout {
    pub repo: String,
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RepoGates {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<Check>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub journeys: Option<Journeys>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holdout: Option<Holdout>,
}

/// The engine's gate config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GatesConfig {
    pub schema: &'static str,
    pub repos: BTreeMap<String, RepoGates>,
}

/// Compile `project_yaml`. `holdout_repo` is the bare Project repo the gate
/// runner clones holdout tests from, and `holdout_sources` names the sources
/// with a `holdout/<source>/` directory on `main`.
pub fn compile(
    project_yaml: &str,
    holdout_repo: &Path,
    holdout_sources: &[String],
) -> Result<GatesConfig, String> {
    let doc: ProjectYaml =
        serde_yaml_ng::from_str(project_yaml).map_err(|e| format!("project.yaml: {e}"))?;
    let mut repos = BTreeMap::new();
    for source in doc.workspace.map(|w| w.sources).unwrap_or_default() {
        let Some(name) = source.name.filter(|n| !n.is_empty()) else {
            continue;
        };
        let v = source.verification.unwrap_or_default();
        let holdout = match v.holdout {
            Some(HoldoutSetting::Enabled(false)) => None,
            Some(HoldoutSetting::Settings(HoldoutYaml { timeout_s })) => Some(timeout_s),
            Some(HoldoutSetting::Enabled(true)) | None => {
                holdout_sources.contains(&name).then_some(None)
            }
        }
        .map(|timeout_s| Holdout {
            repo: holdout_repo.to_string_lossy().into_owned(),
            git_ref: "main".into(),
            path: format!("{HOLDOUT_DIR}/{name}"),
            timeout_s,
        });
        let gates = RepoGates {
            checks: v.checks,
            journeys: v.journeys,
            holdout,
        };
        if gates != RepoGates::default() {
            repos.insert(name, gates);
        }
    }
    Ok(GatesConfig {
        schema: CONFIG_SCHEMA,
        repos,
    })
}

/// What `main` of a bare Project repo declares: its `project.yaml` blob id
/// and text, and the sources with holdout tests. `None` before the first
/// commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    pub blob: String,
    pub project_yaml: String,
    pub holdout_sources: Vec<String>,
}

/// Read [`Declared`] from the bare repo at `bare`.
pub fn read_declared(bare: &Path) -> Result<Option<Declared>, String> {
    let Ok(blob) = git(
        bare,
        &["rev-parse", "--verify", "--quiet", "main:project.yaml"],
    ) else {
        return Ok(None);
    };
    let project_yaml = git(bare, &["show", "main:project.yaml"])?;
    // A tree id for holdout/ changes whenever a holdout test does, so the
    // config is recompiled when a category is added or removed.
    let holdout_tree = git(bare, &["rev-parse", "--verify", "--quiet", "main:holdout"]).ok();
    let holdout_sources = match &holdout_tree {
        Some(_) => git(bare, &["ls-tree", "-d", "--name-only", "main", "holdout/"])?
            .lines()
            .filter_map(|l| l.strip_prefix("holdout/"))
            .map(str::to_string)
            .collect(),
        None => Vec::new(),
    };
    Ok(Some(Declared {
        blob: format!("{blob}:{}", holdout_tree.unwrap_or_default()),
        project_yaml,
        holdout_sources,
    }))
}

/// Run git in `dir` and return its trimmed stdout.
pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const YAML: &str = r#"
schema: "quark.project.v1"
id: "prj_1"
workspace:
  sources:
    - name: "app"
      url: "https://github.com/acme/app"
      verification:
        checks:
          - { name: "test", run: "npm test", timeout_s: 600 }
        journeys:
          dir: "web"
          start: "npm run dev"
          url: "http://127.0.0.1:5173/"
          specs: ["e2e"]
    - name: "lib"
      url: "https://github.com/acme/lib"
    - name: "off"
      url: "https://github.com/acme/off"
      verification:
        holdout: false
delivery:
  policy: "gated"
"#;

    #[test]
    fn compiles_declared_gates_and_found_holdout_tests() {
        let sources = vec!["app".to_string(), "lib".to_string(), "off".to_string()];
        let c = compile(YAML, Path::new("/q/projects/prj_1.git"), &sources).unwrap();
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(json["schema"], "fm.gates.v1");
        let app = &json["repos"]["app"];
        assert_eq!(app["checks"][0]["run"], "npm test");
        assert_eq!(app["checks"][0]["timeout_s"], 600);
        assert_eq!(app["journeys"]["dir"], "web");
        assert_eq!(app["journeys"]["specs"][0], "e2e");
        assert!(app["journeys"].get("setup").is_none());
        assert_eq!(app["holdout"]["repo"], "/q/projects/prj_1.git");
        assert_eq!(app["holdout"]["ref"], "main");
        assert_eq!(app["holdout"]["path"], "holdout/app");
        assert_eq!(json["repos"]["lib"]["holdout"]["path"], "holdout/lib");
        assert!(json["repos"]["lib"].get("checks").is_none());
        assert!(
            json["repos"].get("off").is_none(),
            "holdout: false and nothing else"
        );
    }

    #[test]
    fn sources_without_gates_are_left_out() {
        let c = compile(YAML, Path::new("/r.git"), &[]).unwrap();
        assert_eq!(c.repos.keys().collect::<Vec<_>>(), vec!["app"]);
        assert!(c.repos["app"].holdout.is_none());
        let empty = compile("schema: \"x\"\n", Path::new("/r.git"), &[]).unwrap();
        assert!(empty.repos.is_empty());
    }

    #[test]
    fn refuses_unknown_verification_fields() {
        let bad =
            "workspace:\n  sources:\n    - name: \"a\"\n      verification:\n        chekcs: []\n";
        assert!(compile(bad, Path::new("/r.git"), &[]).is_err());
    }

    #[test]
    fn reads_project_yaml_and_holdout_sources_from_main() {
        let dir = tempfile::tempdir().unwrap();
        let (bare, work) = (dir.path().join("p.git"), dir.path().join("w"));
        let run = |cwd: &Path, args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(cwd)
                .args(args)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        run(dir.path(), &["init", "-q", "--bare", "-b", "main", "p.git"]);
        assert_eq!(read_declared(&bare).unwrap(), None);
        run(dir.path(), &["clone", "-q", "p.git", "w"]);
        std::fs::write(work.join("project.yaml"), YAML).unwrap();
        std::fs::create_dir_all(work.join("holdout/app/smoke")).unwrap();
        std::fs::write(work.join("holdout/app/smoke/run"), "#!/bin/sh\n").unwrap();
        run(&work, &["add", "-A"]);
        run(
            &work,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@x",
                "commit",
                "-qm",
                "p",
            ],
        );
        run(&work, &["push", "-q", "origin", "HEAD:main"]);
        let d = read_declared(&bare).unwrap().unwrap();
        assert_eq!(d.holdout_sources, vec!["app".to_string()]);
        assert!(d.project_yaml.contains("verification"));
        assert!(!d.blob.is_empty());
    }
}
