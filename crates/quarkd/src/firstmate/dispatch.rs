//! `config/crew-dispatch.json` to a Project's `dispatch.yaml`.
//!
//! The two describe the same rules (ADR-11); the differences are the harness
//! ids (firstmate's adapter names, `claude`, against Quark's, `claude-code`)
//! and the `classifier` block, which is not carried over: its provider and
//! credential are the user's to choose again (`docs/configuration/dispatch-rules.md`).
//! The variant files (`crew-dispatch-*.json`) and the `.bak-*` copies are
//! alternatives and history firstmate swapped between, not the rule set in use.

use serde_json::{Map, Value};

use crate::crew_dispatch;

/// The profile fields Quark's `dispatch.yaml` takes.
const PROFILE_KEYS: [&str; 6] = ["harness", "model", "effort", "provider", "floor", "pricing"];
const RULE_KEYS: [&str; 6] = ["when", "use", "select", "why", "approval", "floor"];

#[derive(Debug, Clone)]
pub struct Converted {
    pub yaml: String,
    pub rules: usize,
    pub notes: Vec<String>,
}

/// Quark's id for a firstmate adapter name: a built-in harness's id, engine
/// name or alias. `None` for a harness Quark has no manifest for.
pub fn quark_harness(name: &str) -> Option<String> {
    quark_harness::builtin()
        .iter()
        .find(|m| {
            m.id == name
                || quark_harness::engine_name(m) == name
                || m.aliases.iter().any(|a| a == name)
        })
        .map(|m| m.id.clone())
}

pub fn convert(json: &str) -> Result<Converted, String> {
    let doc: Value = serde_json::from_str(json).map_err(|e| format!("crew-dispatch.json: {e}"))?;
    let doc = doc
        .as_object()
        .ok_or("crew-dispatch.json: not a JSON object")?;
    let mut notes = Vec::new();
    let mut out = Map::new();
    if let Some(c) = doc.get("classifier") {
        let provider = c.get("provider").and_then(Value::as_str).unwrap_or("none");
        if provider != "none" {
            notes.push(format!(
                "the classifier (provider {provider}) was not carried over: choose it again in Settings > Dispatch and store its key with `security add-generic-password`"
            ));
        }
    }
    for key in doc.keys() {
        if !["classifier", "rules", "default", "default_select"].contains(&key.as_str()) {
            notes.push(format!(
                "crew-dispatch.json field `{key}` has no equivalent and was dropped"
            ));
        }
    }
    if let Some(s) = doc.get("default_select") {
        out.insert("default_select".into(), s.clone());
    }
    let mut rules = Vec::new();
    for (i, rule) in doc
        .get("rules")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let label = format!("rule {}", i + 1);
        let obj = rule
            .as_object()
            .ok_or_else(|| format!("crew-dispatch.json: {label} is not an object"))?;
        let mut r = Map::new();
        for (k, v) in obj {
            match k.as_str() {
                "use" => {
                    r.insert("use".into(), profiles(v, &label, &mut notes)?);
                }
                k if RULE_KEYS.contains(&k) => {
                    r.insert(k.into(), v.clone());
                }
                k => notes.push(format!(
                    "{label}: field `{k}` has no equivalent and was dropped"
                )),
            }
        }
        rules.push(Value::Object(r));
    }
    let count = rules.len();
    out.insert("rules".into(), Value::Array(rules));
    if let Some(d) = doc.get("default") {
        out.insert("default".into(), profiles(d, "default", &mut notes)?);
    }
    let yaml = format!(
        "# Imported from a firstmate home's config/crew-dispatch.json.\n{}",
        serde_yaml_ng::to_string(&Value::Object(out)).map_err(|e| e.to_string())?
    );
    crew_dispatch::compile(&yaml, None)?;
    Ok(Converted {
        yaml,
        rules: count,
        notes,
    })
}

fn profiles(v: &Value, label: &str, notes: &mut Vec<String>) -> Result<Value, String> {
    match v {
        Value::Array(items) => Ok(Value::Array(
            items
                .iter()
                .map(|p| profile(p, label, notes))
                .collect::<Result<_, _>>()?,
        )),
        p => profile(p, label, notes),
    }
}

fn profile(v: &Value, label: &str, notes: &mut Vec<String>) -> Result<Value, String> {
    let obj = v
        .as_object()
        .ok_or_else(|| format!("crew-dispatch.json: {label}: a profile is not an object"))?;
    let mut p = Map::new();
    for (k, val) in obj {
        if !PROFILE_KEYS.contains(&k.as_str()) {
            notes.push(format!(
                "{label}: profile field `{k}` has no equivalent and was dropped"
            ));
            continue;
        }
        if k == "harness" {
            let name = val.as_str().unwrap_or_default();
            match quark_harness(name) {
                Some(id) => {
                    p.insert(k.clone(), Value::String(id));
                }
                None => {
                    notes.push(format!(
                        "{label}: harness `{name}` is not a Quark harness; kept as written, so no worker can start with it until one is added"
                    ));
                    p.insert(k.clone(), val.clone());
                }
            }
        } else {
            p.insert(k.clone(), val.clone());
        }
    }
    Ok(Value::Object(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "classifier": {"provider": "system1", "model": "m"},
      "rules": [
        {"when": "a trivial edit", "select": "ordered", "why": "cheap",
         "use": [
           {"harness": "bob", "provider": "bob", "floor": {"scope": "all_models", "min_percent": 15}},
           {"harness": "pi", "model": "x/y", "effort": "medium", "pricing": "budget"}]},
        {"when": "a big feature", "use": {"harness": "claude", "model": "opus", "effort": "max"}}
      ],
      "default": {"harness": "claude", "effort": "high"},
      "default_select": "ordered",
      "extra": 1
    }"#;

    #[test]
    fn maps_harness_ids_and_keeps_typed_fields() {
        let c = convert(SAMPLE).unwrap();
        assert_eq!(c.rules, 2);
        let parsed = crew_dispatch::as_written(&c.yaml).unwrap();
        assert_eq!(
            parsed.rules[1].profiles.as_slice()[0].harness,
            "claude-code"
        );
        let first = parsed.rules[0].profiles.as_slice();
        assert_eq!(first[0].harness, "bob");
        assert_eq!(first[0].floor.as_ref().unwrap().scope, "all_models");
        assert_eq!(first[1].pricing.as_deref(), Some("budget"));
        assert_eq!(parsed.rules[0].why.as_deref(), Some("cheap"));
        assert!(c.notes.iter().any(|n| n.contains("classifier")));
        assert!(c.notes.iter().any(|n| n.contains("`extra`")));
    }

    #[test]
    fn unknown_harness_is_kept_and_noted() {
        let c = convert(r#"{"rules":[{"when":"x","use":{"harness":"zzz"}}]}"#).unwrap();
        assert!(c.notes.iter().any(|n| n.contains("zzz")));
    }

    #[test]
    fn refuses_what_does_not_compile() {
        assert!(convert(r#"{"rules":[{"when":"","use":{"harness":"pi"}}]}"#).is_err());
        assert!(convert("[]").is_err());
    }
}
