//! Dispatch profiles (ADR-11): compile the Project repo's `dispatch.yaml` into
//! the engine's `config/crew-dispatch.json`.
//!
//! ```yaml
//! classifier:
//!   provider: "none"           # see crate::classifier
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
//! out. The `classifier` block compiles to the classifier in effect: the
//! Project's block over the user default (`crate::classifier`), `provider:
//! none` when neither names one. The engine's optional typed-resolution fields (rule `why`, `approval`
//! and `floor`, profile `provider`, `floor` and `pricing`) pass through.
//!
//! The engine validates the result again before writing it
//! (`fm-crew-dispatch.sh config-set`, docs/configuration.md "Crew dispatch
//! profiles" in the engine); this module refuses what it can tell is wrong
//! from the file alone.

use std::collections::HashSet;
use std::path::Path;

use quark_systems::{DispatchProfile, DispatchRuleSpec, DispatchRulesDraft};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::classifier;

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
    #[serde(default)]
    pool: Option<String>,
}

/// How a profile list is resolved.
pub use quark_systems::DispatchSelect as Select;

/// A quota floor; the engine applies it only under typed resolution.
pub use quark_systems::DispatchFloor as Floor;

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
    /// The Quark account pool the profile runs on, which the engine is not
    /// told.
    #[serde(skip)]
    pub pool: Option<String>,
}

/// One profile, or candidates to choose among.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Profiles {
    One(Profile),
    Many(Vec<Profile>),
}

impl Profiles {
    pub fn as_slice(&self) -> &[Profile] {
        match self {
            Profiles::One(p) => std::slice::from_ref(p),
            Profiles::Many(ps) => ps,
        }
    }
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
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CrewDispatchConfig {
    /// The classifier block: the one in effect when compiled, the file's own
    /// when read as written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classifier: Option<serde_json::Value>,
    pub rules: Vec<Rule>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Profiles>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_select: Option<Select>,
}

/// Compile `dispatch.yaml`. `user_classifier` is the user-level default
/// classifier block, which the file's own block overrides field by field.
pub fn compile(
    dispatch_yaml: &str,
    user_classifier: Option<&classifier::Block>,
) -> Result<CrewDispatchConfig, String> {
    let fail = |e: String| format!("{FILE}: {e}");
    let mut config = read(dispatch_yaml, engine_harness)?;
    let block = classifier::block(config.classifier.as_ref()).map_err(fail)?;
    let effective = classifier::effective(block.as_ref(), user_classifier).map_err(fail)?;
    config.classifier = Some(serde_json::to_value(effective).map_err(|e| fail(e.to_string()))?);
    Ok(config)
}

/// `dispatch.yaml` as written: checked as [`compile`] checks it, with
/// harness ids left Quark's.
pub fn as_written(dispatch_yaml: &str) -> Result<CrewDispatchConfig, String> {
    read(dispatch_yaml, |id| id)
}

fn read(dispatch_yaml: &str, harness: fn(&str) -> &str) -> Result<CrewDispatchConfig, String> {
    let fail = |e: String| format!("{FILE}: {e}");
    let doc: Option<DispatchYaml> =
        serde_yaml_ng::from_str(dispatch_yaml).map_err(|e| fail(e.to_string()))?;
    let Some(doc) = doc else {
        return Ok(CrewDispatchConfig::default());
    };
    classifier::block(doc.classifier.as_ref()).map_err(fail)?;
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
            profiles: profiles(r.profiles, harness).map_err(|e| fail(format!("use: {e}")))?,
            select: r.select,
            why: r.why,
            approval: r.approval,
            floor: r.floor,
        });
    }
    let default = doc
        .default
        .map(|d| profiles(d, harness))
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
fn profiles(v: serde_yaml_ng::Value, harness: fn(&str) -> &str) -> Result<Profiles, String> {
    match v {
        serde_yaml_ng::Value::Sequence(items) => {
            if items.is_empty() {
                return Err("needs at least one profile".into());
            }
            items
                .into_iter()
                .enumerate()
                .map(|(i, p)| profile(p, harness).map_err(|e| format!("profile {}: {e}", i + 1)))
                .collect::<Result<_, _>>()
                .map(Profiles::Many)
        }
        serde_yaml_ng::Value::Mapping(_) => profile(v, harness).map(Profiles::One),
        _ => Err("must be a profile or a list of profiles".into()),
    }
}

fn profile(v: serde_yaml_ng::Value, harness: fn(&str) -> &str) -> Result<Profile, String> {
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
        harness: harness(&p.harness).to_string(),
        model: p.model,
        effort: p.effort,
        provider: p.provider,
        floor: p.floor,
        pricing: p.pricing,
        pool: p.pool,
    })
}

fn from_value<T: DeserializeOwned>(v: serde_yaml_ng::Value) -> Result<T, String> {
    serde_yaml_ng::from_value(v).map_err(|e| e.to_string())
}

/// The engine's adapter name for a Quark harness id. Names Quark has no
/// built-in harness for pass through, for the engine to accept or refuse.
fn engine_harness(id: &str) -> &str {
    quark_harness::builtin()
        .iter()
        .find(|m| m.id == id)
        .map_or(id, |m| quark_harness::engine_name(m))
}

/// The editable part of a config [`as_written`] read.
pub fn draft(written: &CrewDispatchConfig) -> DispatchRulesDraft {
    let profile = |p: &Profile| DispatchProfile {
        harness: p.harness.clone(),
        model: p.model.clone(),
        effort: p.effort.clone(),
        pool: p.pool.clone(),
        provider: p.provider.clone(),
        floor: p.floor.clone(),
        pricing: p.pricing.clone(),
    };
    let list = |ps: &Profiles| ps.as_slice().iter().map(profile).collect();
    DispatchRulesDraft {
        default_select: written.default_select,
        rules: written
            .rules
            .iter()
            .map(|r| DispatchRuleSpec {
                name: r.name.clone(),
                when: r.when.clone(),
                candidates: list(&r.profiles),
                select: r.select,
                why: r.why.clone(),
                approval: r.approval.clone(),
                floor: r.floor.clone(),
            })
            .collect(),
        default: written.default.as_ref().map(list).unwrap_or_default(),
    }
}

/// Write `draft` as `dispatch.yaml`, in the layout project creation uses.
/// The comment lines that open `previous`, the file being replaced, and its
/// `classifier` block are kept; other comments are not. Blank optional
/// values are left out. The result is not checked: [`compile`] it.
pub fn render(draft: &DispatchRulesDraft, previous: Option<&str>) -> Result<String, String> {
    let mut y = String::new();
    match previous {
        Some(prev) => {
            for line in prev.lines().take_while(|l| l.starts_with('#')) {
                y.push_str(line);
                y.push('\n');
            }
            if let Some(classifier) = as_written(prev)?.classifier {
                y.push_str(&classifier_block(prev, &classifier)?);
            }
        }
        None => y.push_str("# Dispatch rules for this Project.\n"),
    }
    if let Some(select) = draft.default_select {
        y.push_str(&format!("default_select: {}\n", select_yaml(select)));
    }
    if draft.rules.is_empty() {
        y.push_str("rules: []\n");
    } else {
        y.push_str("rules:\n");
    }
    for r in &draft.rules {
        let mut lines = Vec::new();
        if let Some(name) = filled(&r.name) {
            lines.push(format!("name: {}", q(name)));
        }
        lines.push(format!("when: {}", q(&r.when)));
        if let Some(select) = r.select {
            lines.push(format!("select: {}", select_yaml(select)));
        }
        if let Some(why) = filled(&r.why) {
            lines.push(format!("why: {}", q(why)));
        }
        if let Some(approval) = filled(&r.approval) {
            lines.push(format!("approval: {}", q(approval)));
        }
        if let Some(floor) = &r.floor {
            lines.push(format!("floor: {}", floor_yaml(floor)));
        }
        if r.candidates.is_empty() {
            lines.push("use: []".into());
        } else {
            lines.push("use:".into());
        }
        for (i, line) in lines.iter().enumerate() {
            y.push_str(if i == 0 { "  - " } else { "    " });
            y.push_str(line);
            y.push('\n');
        }
        for p in &r.candidates {
            y.push_str(&format!("      - {}\n", profile_yaml(p)));
        }
    }
    if !draft.default.is_empty() {
        y.push_str("default:\n");
        for p in &draft.default {
            y.push_str(&format!("  - {}\n", profile_yaml(p)));
        }
    }
    Ok(y)
}

/// A YAML double-quoted scalar: a JSON string is one.
fn q(s: &str) -> String {
    serde_json::to_string(s).expect("strings serialize")
}

fn filled(s: &Option<String>) -> Option<&str> {
    s.as_deref().filter(|v| !v.trim().is_empty())
}

fn select_yaml(select: Select) -> String {
    serde_json::to_string(&select).expect("a select serializes")
}

fn floor_yaml(f: &Floor) -> String {
    let mut parts = vec![
        format!("scope: {}", q(&f.scope)),
        format!("min_percent: {}", f.min_percent),
    ];
    if let Some(p) = filled(&f.provider) {
        parts.push(format!("provider: {}", q(p)));
    }
    format!("{{ {} }}", parts.join(", "))
}

/// One flow-style profile.
fn profile_yaml(p: &DispatchProfile) -> String {
    let mut parts = vec![format!("harness: {}", q(&p.harness))];
    for (key, value) in [
        ("model", &p.model),
        ("effort", &p.effort),
        ("pool", &p.pool),
        ("provider", &p.provider),
        ("pricing", &p.pricing),
    ] {
        if let Some(v) = filled(value) {
            parts.push(format!("{key}: {}", q(v)));
        }
    }
    if let Some(f) = &p.floor {
        parts.push(format!("floor: {}", floor_yaml(f)));
    }
    format!("{{ {} }}", parts.join(", "))
}

/// The `classifier` block of `file`, which reads as `classifier`: its own
/// lines when they stand apart from the rest, else the value written anew.
fn classifier_block(file: &str, classifier: &serde_json::Value) -> Result<String, String> {
    let lines: Vec<&str> = file.lines().collect();
    if let Some(start) = lines.iter().position(|l| l.starts_with("classifier:")) {
        let inside = |l: &&&str| l.trim().is_empty() || l.starts_with([' ', '\t']);
        let mut end = start + 1 + lines[start + 1..].iter().take_while(inside).count();
        while lines[end - 1].trim().is_empty() {
            end -= 1;
        }
        let block = format!("{}\n", lines[start..end].join("\n"));
        if as_written(&block).is_ok_and(|c| c.classifier.as_ref() == Some(classifier)) {
            return Ok(block);
        }
    }
    serde_yaml_ng::to_string(&serde_json::json!({ "classifier": classifier }))
        .map_err(|e| format!("{FILE}: classifier: {e}"))
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
        let c = compile(YAML, None).unwrap();
        assert_eq!(
            serde_json::to_value(&c).unwrap(),
            json!({
                "classifier": {"provider": "none"},
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
        let c = serde_json::to_value(compile(y, None).unwrap()).unwrap();
        assert_eq!(c["rules"], json!([]));
        assert_eq!(c["default"], json!([{"harness": "cursor"}]));
        let empty = serde_json::to_value(compile("# nothing yet\n", None).unwrap()).unwrap();
        assert_eq!(
            empty,
            json!({"classifier": {"provider": "none"}, "rules": []})
        );
    }

    #[test]
    fn compiles_the_classifier_in_effect() {
        let user = classifier::user_default(
            "classifier:\n  provider: system1\n  model: jev-1\n\
             \x20 credential: keychain:quark/classifier\n  timeout_ms: 1500\n",
        )
        .unwrap();
        let of = |y: &str| {
            serde_json::to_value(compile(y, user.as_ref()).unwrap()).unwrap()["classifier"].clone()
        };
        // A Project that says nothing takes the user default.
        let inherited = json!({
            "provider": "system1",
            "model": "jev-1",
            "credential": "keychain:quark/classifier",
            "confidence_floor": 0.6,
            "timeout_ms": 1500,
            "on_failure": "coordinator"
        });
        assert_eq!(of(""), inherited);
        assert_eq!(of("rules: []\n"), inherited);
        // What the Project sets wins; the rest still comes from the default.
        let c = of("classifier:\n  confidence_floor: 0.8\n  on_failure: default\n");
        assert_eq!(c["confidence_floor"], 0.8);
        assert_eq!(c["on_failure"], "default");
        assert_eq!(c["model"], "jev-1");
        assert_eq!(
            of("classifier:\n  provider: none\n"),
            json!({"provider": "none"})
        );
        // Without a user default the provider is none.
        let alone = serde_json::to_value(compile("rules: []\n", None).unwrap()).unwrap();
        assert_eq!(alone["classifier"], json!({"provider": "none"}));
        // Read as written, the file's own block is kept as it is.
        let written = as_written("classifier:\n  confidence_floor: 0.8\n").unwrap();
        assert_eq!(written.classifier, Some(json!({"confidence_floor": 0.8})));
        assert_eq!(as_written("rules: []\n").unwrap().classifier, None);
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
            ("classifier: { credential: \"sk-live-abc\" }", "classifier: credential must be a keychain reference"),
            ("classifier: { provider: system1 }", "classifier: provider system1 needs a model"),
            ("classifier: { confidence_floor: 2 }", "classifier: confidence_floor must be between 0 and 1"),
            ("classifier: { endpoint: \"https://x.test\" }", "classifier: unknown field `endpoint`"),
            ("defualt: []", "unknown field `defualt`"),
            ("rules: [", "dispatch.yaml"),
        ] {
            let err = compile(bad, None).unwrap_err();
            assert!(err.contains(why), "{bad}: {err}");
            assert!(err.starts_with("dispatch.yaml: "), "{err}");
        }
    }

    #[test]
    fn a_draft_renders_to_a_file_that_reads_back_as_the_draft() {
        let written = as_written(YAML).unwrap();
        let d = draft(&written);
        assert_eq!(d.rules[0].candidates[0].harness, "claude-code");
        assert_eq!(d.rules[0].candidates[0].pool.as_deref(), Some("max"));
        assert_eq!(
            d.rules[1].candidates.len(),
            1,
            "one profile is a list of one"
        );
        assert_eq!(d.default_select, Some(Select::Ordered));

        let y = render(&d, Some(YAML.trim_start())).unwrap();
        assert!(
            y.starts_with(
                "# Dispatch rules for this Project.\nclassifier:\n  provider: \"none\"\n  \
                 confidence_floor: 0.6\ndefault_select: \"ordered\"\nrules:\n  \
                 - name: \"trivial-edit\"\n    when: \"A trivial mechanical edit such as a rename, typo or one-line fix.\"\n    use:\n      \
                 - { harness: \"claude-code\", model: \"claude-sonnet-5\", effort: \"low\", pool: \"max\" }\n"
            ),
            "{y}"
        );
        assert!(y.contains(
            "  - when: \"A Bedrock-only task.\"\n    approval: \"captain\"\n    \
             floor: { scope: \"all_models\", min_percent: 20, provider: \"claude\" }\n    use:\n"
        ));
        assert!(y.ends_with(
            "default:\n  - { harness: \"claude-code\", model: \"claude-sonnet-5\", effort: \"medium\" }\n"
        ));
        let again = as_written(&y).unwrap();
        assert_eq!(draft(&again), d);
        assert_eq!(again.classifier, written.classifier);
        // Rendering what was rendered changes nothing.
        assert_eq!(render(&d, Some(&y)).unwrap(), y);
    }

    #[test]
    fn rendering_keeps_any_classifier_and_drops_blank_values() {
        let d = DispatchRulesDraft {
            default_select: None,
            rules: vec![DispatchRuleSpec {
                name: Some(" ".into()),
                when: "Says \"quoted\": yes".into(),
                candidates: vec![DispatchProfile {
                    harness: "codex".into(),
                    model: Some(String::new()),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            default: vec![],
        };
        // A classifier written in flow style, after the rules.
        let prev = "rules: []\nclassifier: { provider: \"system1\", confidence_floor: 0.5 }\n";
        let y = render(&d, Some(prev)).unwrap();
        assert_eq!(
            y,
            "classifier: { provider: \"system1\", confidence_floor: 0.5 }\nrules:\n  \
             - when: \"Says \\\"quoted\\\": yes\"\n    use:\n      - { harness: \"codex\" }\n"
        );
        assert_eq!(
            as_written(&y).unwrap().rules[0].when,
            "Says \"quoted\": yes"
        );

        // A block that cannot be lifted as lines is written anew.
        let prev = "{ classifier: { provider: \"system1\" }, rules: [] }\n";
        let y = render(&d, Some(prev)).unwrap();
        assert!(
            y.starts_with("classifier:\n  provider: system1\nrules:\n"),
            "{y}"
        );

        // No file yet, and a file that does not compile.
        let y = render(&DispatchRulesDraft::default(), None).unwrap();
        assert_eq!(y, "# Dispatch rules for this Project.\nrules: []\n");
        assert!(render(&d, Some("rules: [")).is_err());
        // An empty candidate list is written so that compiling refuses it.
        let mut empty = d.clone();
        empty.rules[0].candidates.clear();
        let err = compile(&render(&empty, None).unwrap(), None).unwrap_err();
        assert!(err.contains("needs at least one profile"), "{err}");
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
