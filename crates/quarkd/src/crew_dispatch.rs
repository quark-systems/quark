//! Dispatch profiles (ADR-11): compile the Project repo's `dispatch.yaml` into
//! the engine's `config/crew-dispatch.json`.
//!
//! ```yaml
//! classifier:
//!   provider: "none"           # kept for the classifier; the engine ignores it
//! default_select: "ordered"    # or "quota-balanced"
//! rules:
//!   - name: "trivial-edit"
//!     when: "A trivial mechanical edit such as a rename, typo or one-line fix."
//!     select: "ordered"
//!     use:
//!       - { harness: "claude-code", model: "claude-sonnet-5", effort: "low" }
//!       - { harness: "codex", model: "gpt-5.5", effort: "low" }
//! default:
//!   - { harness: "claude-code", model: "claude-sonnet-5", effort: "medium" }
//! ```
//!
//! `use` and `default` take one profile or a list of them, and keep that shape.
//! Harness ids are Quark's (`claude-code`) and compile to the engine's adapter
//! names (`claude`). A profile's account `pool` is Quark's own and is left
//! out. The engine's optional typed-resolution fields (rule `why`, `approval`
//! and `floor`, profile `provider`, `floor` and `pricing`) pass through.
//!
//! The engine validates the result again before writing it
//! (`fm-crew-dispatch.sh config-set`, docs/configuration.md "Crew dispatch
//! profiles" in the engine); this module refuses what it can tell is wrong
//! from the file alone.

use std::collections::HashSet;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::harness::SPECS;

/// The Project repo file this module compiles.
pub const FILE: &str = "dispatch.yaml";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DispatchYaml {
    #[serde(default)]
    classifier: Option<serde_json::Value>,
    #[serde(default)]
    default_select: Option<Select>,
    #[serde(default)]
    rules: Option<Vec<RuleYaml>>,
    #[serde(default)]
    default: Option<serde_yaml_ng::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleYaml {
    #[serde(default)]
    name: Option<String>,
    when: String,
    #[serde(rename = "use")]
    profiles: serde_yaml_ng::Value,
    #[serde(default)]
    select: Option<Select>,
    #[serde(default)]
    why: Option<String>,
    #[serde(default)]
    approval: Option<String>,
    #[serde(default)]
    floor: Option<Floor>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileYaml {
    harness: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    floor: Option<Floor>,
    #[serde(default)]
    pricing: Option<String>,
    /// The Quark account pool the profile runs on; not the engine's concern.
    #[serde(default, rename = "pool")]
    _pool: Option<String>,
}

/// How a profile list is resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Select {
    QuotaBalanced,
    Ordered,
}

/// A quota floor; the engine applies it only under typed resolution.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Floor {
    pub scope: String,
    pub min_percent: serde_json::Number,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Profile {
    pub harness: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub floor: Option<Floor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing: Option<String>,
}

/// One profile, or candidates to choose among.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Profiles {
    One(Profile),
    Many(Vec<Profile>),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Rule {
    /// Quark's name for the rule; the engine ignores it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub when: String,
    #[serde(rename = "use")]
    pub profiles: Profiles,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select: Option<Select>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub floor: Option<Floor>,
}

/// The engine's `crew-dispatch.json`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CrewDispatchConfig {
    /// The classifier block, kept verbatim for Quark's classifier. The engine
    /// keeps unknown top-level keys and ignores them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classifier: Option<serde_json::Value>,
    pub rules: Vec<Rule>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Profiles>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_select: Option<Select>,
}

/// Compile `dispatch.yaml`.
pub fn compile(dispatch_yaml: &str) -> Result<CrewDispatchConfig, String> {
    let fail = |e: String| format!("{FILE}: {e}");
    let doc: Option<DispatchYaml> =
        serde_yaml_ng::from_str(dispatch_yaml).map_err(|e| fail(e.to_string()))?;
    let Some(doc) = doc else {
        return Ok(CrewDispatchConfig {
            classifier: None,
            rules: Vec::new(),
            default: None,
            default_select: None,
        });
    };
    if doc.classifier.as_ref().is_some_and(|c| !c.is_object()) {
        return Err(fail("classifier must be a mapping".into()));
    }
    let mut names = HashSet::new();
    let mut rules = Vec::new();
    for (i, r) in doc.rules.unwrap_or_default().into_iter().enumerate() {
        let label = match &r.name {
            Some(n) => format!("rule {} ({n})", i + 1),
            None => format!("rule {}", i + 1),
        };
        let fail = |e: String| fail(format!("{label}: {e}"));
        if let Some(n) = &r.name {
            if n.trim().is_empty() {
                return Err(fail("name is empty".into()));
            }
            if !names.insert(n.clone()) {
                return Err(fail("another rule has this name".into()));
            }
        }
        if r.when.trim().is_empty() {
            return Err(fail("when is empty".into()));
        }
        rules.push(Rule {
            name: r.name,
            when: r.when,
            profiles: profiles(r.profiles).map_err(|e| fail(format!("use: {e}")))?,
            select: r.select,
            why: r.why,
            approval: r.approval,
            floor: r.floor,
        });
    }
    let default = doc
        .default
        .map(profiles)
        .transpose()
        .map_err(|e| fail(format!("default: {e}")))?;
    Ok(CrewDispatchConfig {
        classifier: doc.classifier,
        rules,
        default,
        default_select: doc.default_select,
    })
}

/// One profile mapping or a non-empty list of them.
fn profiles(v: serde_yaml_ng::Value) -> Result<Profiles, String> {
    match v {
        serde_yaml_ng::Value::Sequence(items) => {
            if items.is_empty() {
                return Err("needs at least one profile".into());
            }
            items
                .into_iter()
                .enumerate()
                .map(|(i, p)| profile(p).map_err(|e| format!("profile {}: {e}", i + 1)))
                .collect::<Result<_, _>>()
                .map(Profiles::Many)
        }
        serde_yaml_ng::Value::Mapping(_) => profile(v).map(Profiles::One),
        _ => Err("must be a profile or a list of profiles".into()),
    }
}

fn profile(v: serde_yaml_ng::Value) -> Result<Profile, String> {
    let p: ProfileYaml = from_value(v)?;
    for (field, value) in [
        ("harness", Some(&p.harness)),
        ("model", p.model.as_ref()),
        ("effort", p.effort.as_ref()),
    ] {
        if value.is_some_and(|v| v.trim().is_empty()) {
            return Err(format!("{field} is empty"));
        }
    }
    Ok(Profile {
        harness: engine_harness(&p.harness).to_string(),
        model: p.model,
        effort: p.effort,
        provider: p.provider,
        floor: p.floor,
        pricing: p.pricing,
    })
}

fn from_value<T: DeserializeOwned>(v: serde_yaml_ng::Value) -> Result<T, String> {
    serde_yaml_ng::from_value(v).map_err(|e| e.to_string())
}

/// The engine's adapter name for a Quark harness id. Names Quark has no
/// built-in harness for pass through, for the engine to accept or refuse.
fn engine_harness(id: &str) -> &str {
    SPECS.iter().find(|s| s.id == id).map_or(id, |s| s.engine)
}

/// What `main` of a bare Project repo declares: the `dispatch.yaml` blob id
/// and text. `None` when `main` has no such file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    pub blob: String,
    pub dispatch_yaml: String,
}

/// Read [`Declared`] from the bare repo at `bare`.
pub fn read_declared(bare: &Path) -> Result<Option<Declared>, String> {
    let spec = format!("main:{FILE}");
    let Ok(blob) = crate::gates::git(bare, &["rev-parse", "--verify", "--quiet", &spec]) else {
        return Ok(None);
    };
    let dispatch_yaml = crate::gates::git(bare, &["show", &spec])?;
    Ok(Some(Declared {
        blob,
        dispatch_yaml,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::process::Command;

    const YAML: &str = r#"
# Dispatch rules for this Project.
classifier:
  provider: "none"
  confidence_floor: 0.6
default_select: "ordered"
rules:
  - name: "trivial-edit"
    when: "A trivial mechanical edit such as a rename, typo or one-line fix."
    use:
      - { harness: "claude-code", model: "claude-sonnet-5", effort: "low", pool: "max" }
      - { harness: "codex", model: "gpt-5.5", effort: "low" }
  - name: "news"
    when: "The task depends on fresh news."
    select: "quota-balanced"
    why: "Grok has live web context."
    use: { harness: "grok" }
  - when: "A Bedrock-only task."
    approval: "captain"
    floor: { scope: "all_models", min_percent: 20, provider: "claude" }
    use:
      - { harness: "pi", model: "amazon-bedrock/sonnet", provider: "amazon-bedrock", pricing: "budget" }
      - { harness: "bob", provider: "bob", floor: { scope: "all_models", min_percent: 15 } }
default:
  - { harness: "claude-code", model: "claude-sonnet-5", effort: "medium" }
"#;

    #[test]
    fn compiles_rules_default_and_candidate_lists() {
        let c = compile(YAML).unwrap();
        assert_eq!(
            serde_json::to_value(&c).unwrap(),
            json!({
                "classifier": {"provider": "none", "confidence_floor": 0.6},
                "rules": [
                    {
                        "name": "trivial-edit",
                        "when": "A trivial mechanical edit such as a rename, typo or one-line fix.",
                        "use": [
                            {"harness": "claude", "model": "claude-sonnet-5", "effort": "low"},
                            {"harness": "codex", "model": "gpt-5.5", "effort": "low"}
                        ]
                    },
                    {
                        "name": "news",
                        "when": "The task depends on fresh news.",
                        "use": {"harness": "grok"},
                        "select": "quota-balanced",
                        "why": "Grok has live web context."
                    },
                    {
                        "when": "A Bedrock-only task.",
                        "use": [
                            {"harness": "pi", "model": "amazon-bedrock/sonnet", "provider": "amazon-bedrock", "pricing": "budget"},
                            {"harness": "bob", "provider": "bob", "floor": {"scope": "all_models", "min_percent": 15}}
                        ],
                        "approval": "captain",
                        "floor": {"scope": "all_models", "min_percent": 20, "provider": "claude"}
                    }
                ],
                "default": [{"harness": "claude", "model": "claude-sonnet-5", "effort": "medium"}],
                "default_select": "ordered"
            })
        );
    }

    #[test]
    fn compiles_what_project_creation_writes() {
        let y = "classifier:\n  provider: \"none\"\ndefault_select: \"ordered\"\nrules: []\n\
                 default:\n  - { harness: \"cursor-agent\" }\n";
        let c = serde_json::to_value(compile(y).unwrap()).unwrap();
        assert_eq!(c["rules"], json!([]));
        assert_eq!(c["default"], json!([{"harness": "cursor"}]));
        let empty = serde_json::to_value(compile("# nothing yet\n").unwrap()).unwrap();
        assert_eq!(empty, json!({"rules": []}));
    }

    #[test]
    fn refuses_what_the_engine_could_not_use() {
        for (bad, why) in [
            ("rules: [{ when: \"x\" }]", "missing field `use`"),
            ("rules: [{ when: \" \", use: { harness: \"codex\" } }]", "when is empty"),
            ("rules: [{ when: \"x\", use: [] }]", "at least one profile"),
            ("rules: [{ when: \"x\", use: \"codex\" }]", "a profile or a list"),
            ("rules: [{ when: \"x\", use: [{ harness: \"codex\", efort: \"low\" }] }]", "rule 1: use: profile 1: unknown field `efort`"),
            ("rules: [{ when: \"x\", use: { model: \"m\" } }]", "missing field `harness`"),
            ("rules: [{ when: \"x\", use: { harness: \"codex\", effort: \"\" } }]", "effort is empty"),
            ("rules: [{ name: a, when: x, use: { harness: codex } }, { name: a, when: y, use: { harness: codex } }]", "rule 2 (a): another rule has this name"),
            ("default_select: \"cheapest\"", "unknown variant"),
            ("default: []", "default: needs at least one profile"),
            ("classifier: \"none\"", "classifier must be a mapping"),
            ("defualt: []", "unknown field `defualt`"),
            ("rules: [", "dispatch.yaml"),
        ] {
            let err = compile(bad).unwrap_err();
            assert!(err.contains(why), "{bad}: {err}");
            assert!(err.starts_with("dispatch.yaml: "), "{err}");
        }
    }

    #[test]
    fn reads_dispatch_yaml_from_main() {
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
        std::fs::write(work.join(FILE), YAML).unwrap();
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
                "d",
            ],
        );
        run(&work, &["push", "-q", "origin", "HEAD:main"]);
        let d = read_declared(&bare).unwrap().unwrap();
        assert!(d.dispatch_yaml.contains("trivial-edit"));
        assert_eq!(d.blob.len(), 40);
    }
}
