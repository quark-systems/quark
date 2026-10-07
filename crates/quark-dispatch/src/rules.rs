//! A Project's dispatch rules, in the shape quarkd compiles `dispatch.yaml`
//! to (firstmate's `config/crew-dispatch.json`).
//!
//! ```json
//! {
//!   "classifier": { "provider": "system1", "confidence_floor": 0.6 },
//!   "default_select": "quota-balanced",
//!   "rules": [
//!     { "when": "A trivial edit.", "select": "ordered",
//!       "use": [{ "harness": "claude", "model": "claude-sonnet-5", "effort": "low" }] }
//!   ],
//!   "default": { "harness": "claude", "effort": "medium" }
//! }
//! ```
//!
//! `use` and `default` take one profile or a list. A profile's `pool` is
//! Quark's own (the account pool it runs under); firstmate never sees it.

use serde::{Deserialize, Deserializer, Serialize};

/// The default classifier confidence floor.
pub const DEFAULT_CONFIDENCE_FLOOR: f64 = 0.6;
/// The default classifier timeout.
pub const DEFAULT_TIMEOUT_MS: u64 = 5000;
/// The default System One model.
pub const DEFAULT_CLASSIFIER_MODEL: &str = "jev-latest";

/// How a candidate list is resolved.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Select {
    /// The first eligible candidate in listed order.
    Ordered,
    /// The eligible candidate with the highest spend priority.
    #[default]
    QuotaBalanced,
}

/// A quota floor: `scope` of `provider` must have at least `min_percent`
/// left.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Floor {
    pub scope: String,
    pub min_percent: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

/// One candidate agent: a harness with its model and effort, the account
/// pool it runs under, and its quota declarations.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub harness: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Account pool; absent runs under the harness's default account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    /// Quota provider family, when the harness alone does not name one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub floor: Option<Floor>,
    /// `budget`: priced by budget, with no quota to read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<String>,
}

impl Profile {
    pub fn new(harness: impl Into<String>) -> Self {
        Self {
            harness: harness.into(),
            ..Self::default()
        }
    }

    pub fn is_budget(&self) -> bool {
        self.pricing.as_deref() == Some("budget")
    }

    /// `harness:model`, as a person reads it.
    pub fn label(&self) -> String {
        format!("{}:{}", self.harness, self.model.as_deref().unwrap_or("-"))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The condition a task must meet, in plain words.
    pub when: String,
    #[serde(rename = "use", deserialize_with = "one_or_many")]
    pub profiles: Vec<Profile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select: Option<Select>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// `captain` (any person): the rule needs a person's approval before
    /// dispatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub floor: Option<Floor>,
}

/// What to do when the classifier gives no usable answer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnFailure {
    /// Report it; the coordinator picks.
    #[default]
    Coordinator,
    /// Select from the default profiles.
    Default,
}

/// The `classifier` block.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassifierBlock {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// `keychain:<service>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence_floor: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_failure: Option<OnFailure>,
}

/// A Project's dispatch rules.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DispatchConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classifier: Option<ClassifierBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_select: Option<Select>,
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default, deserialize_with = "one_or_many")]
    pub default: Vec<Profile>,
}

impl DispatchConfig {
    pub fn parse(json: &str) -> Result<Self, String> {
        let c: Self = serde_json::from_str(json).map_err(|e| e.to_string())?;
        c.check()?;
        Ok(c)
    }

    /// What firstmate refuses from the file alone.
    pub fn check(&self) -> Result<(), String> {
        let profiles = self
            .rules
            .iter()
            .flat_map(|r| &r.profiles)
            .chain(&self.default);
        for p in profiles {
            if p.harness.trim().is_empty() {
                return Err("a profile has no harness".into());
            }
        }
        for (i, r) in self.rules.iter().enumerate() {
            if r.profiles.is_empty() {
                return Err(format!("rule {} has no profiles", i + 1));
            }
            if r.when.trim().is_empty() {
                return Err(format!("rule {} has no condition", i + 1));
            }
        }
        if let Some(c) = &self.classifier {
            if let Some(f) = c.confidence_floor {
                if !(0.0..=1.0).contains(&f) {
                    return Err(format!("classifier confidence_floor {f} is not 0 to 1"));
                }
            }
            if let Some(t) = c.timeout_ms {
                if !(1..=60_000).contains(&t) {
                    return Err(format!("classifier timeout_ms {t} is not 1 to 60000"));
                }
            }
            if let Some(cred) = &c.credential {
                if !cred.starts_with("keychain:") || cred.len() == "keychain:".len() {
                    return Err("classifier credential must be keychain:<service>".into());
                }
            }
            match c.provider.as_deref() {
                None | Some("none") | Some("system1") => {}
                Some(p) => return Err(format!("classifier provider {p:?}")),
            }
        }
        Ok(())
    }

    /// The id the classifier and the resolution use for rule `i` (0-based).
    pub fn rule_id(i: usize) -> String {
        format!("rule_{}", i + 1)
    }

    /// The rule named `rule_N`, if there is one.
    pub fn rule(&self, id: &str) -> Option<&Rule> {
        let n: usize = id.strip_prefix("rule_")?.parse().ok()?;
        self.rules.get(n.checked_sub(1)?)
    }
}

/// The classifier settings in effect.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassifierSettings {
    /// Whether the classifier is asked at all.
    pub on: bool,
    pub model: String,
    pub credential: Option<String>,
    pub confidence_floor: f64,
    pub timeout_ms: u64,
    pub on_failure: OnFailure,
}

impl ClassifierSettings {
    /// The settings `config` asks for. Without a `classifier` block the
    /// classifier is on only when an API key is in the environment
    /// (`key_present`), as firstmate's opt-in gate has it.
    pub fn from_config(config: &DispatchConfig, key_present: bool) -> Self {
        let block = config.classifier.clone().unwrap_or_default();
        let on = match (&config.classifier, block.provider.as_deref()) {
            (None, _) => key_present,
            (Some(_), Some("system1")) => true,
            (Some(_), _) => false,
        };
        Self {
            on,
            model: block
                .model
                .unwrap_or_else(|| DEFAULT_CLASSIFIER_MODEL.to_string()),
            credential: block.credential,
            confidence_floor: block.confidence_floor.unwrap_or(DEFAULT_CONFIDENCE_FLOOR),
            timeout_ms: block.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS),
            on_failure: block.on_failure.unwrap_or_default(),
        }
    }
}

fn one_or_many<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Profile>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(Profile),
        Many(Vec<Profile>),
    }
    Ok(match Option::<OneOrMany>::deserialize(d)? {
        None => Vec::new(),
        Some(OneOrMany::One(p)) => vec![p],
        Some(OneOrMany::Many(ps)) => ps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_one_or_many() {
        let c = DispatchConfig::parse(
            r#"{"rules":[{"when":"x","use":{"harness":"claude"}},
                {"when":"y","use":[{"harness":"codex","model":"gpt-5.5"},{"harness":"claude","pricing":"budget"}],"select":"ordered"}],
               "default":{"harness":"claude","effort":"medium"}}"#,
        )
        .unwrap();
        assert_eq!(c.rules[0].profiles.len(), 1);
        assert_eq!(c.rules[1].profiles.len(), 2);
        assert_eq!(c.rules[1].select, Some(Select::Ordered));
        assert!(c.rules[1].profiles[1].is_budget());
        assert_eq!(c.default[0].effort.as_deref(), Some("medium"));
        assert_eq!(c.rule("rule_2").unwrap().when, "y");
        assert!(c.rule("rule_3").is_none());
        assert!(c.rule("rule_0").is_none());
        assert!(c.rule("default").is_none());
    }

    #[test]
    fn refuses_bad_files() {
        assert!(DispatchConfig::parse(r#"{"rules":[{"when":"x","use":[]}]}"#).is_err());
        assert!(DispatchConfig::parse(r#"{"classifier":{"api_key":"k"}}"#).is_err());
        assert!(DispatchConfig::parse(r#"{"classifier":{"credential":"sk-1"}}"#).is_err());
        assert!(DispatchConfig::parse(r#"{"classifier":{"confidence_floor":2}}"#).is_err());
    }

    #[test]
    fn classifier_gate() {
        let none = DispatchConfig::default();
        assert!(!ClassifierSettings::from_config(&none, false).on);
        assert!(ClassifierSettings::from_config(&none, true).on);
        let off = DispatchConfig::parse(r#"{"classifier":{"provider":"none"}}"#).unwrap();
        assert!(!ClassifierSettings::from_config(&off, true).on);
        let on = DispatchConfig::parse(
            r#"{"classifier":{"provider":"system1","confidence_floor":0.8,"on_failure":"default"}}"#,
        )
        .unwrap();
        let s = ClassifierSettings::from_config(&on, false);
        assert!(s.on);
        assert_eq!(s.confidence_floor, 0.8);
        assert_eq!(s.on_failure, OnFailure::Default);
        assert_eq!(s.model, DEFAULT_CLASSIFIER_MODEL);
    }
}
