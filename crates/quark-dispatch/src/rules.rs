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
/// The provider for the hosted System One API.
pub const SYSTEM1: &str = "system1";
/// The provider for an Ollama server's System One endpoint.
pub const OLLAMA: &str = "ollama";
/// The hosted System One server.
pub const SYSTEM1_URL: &str = "https://api.typesafe.ai";
/// A local Ollama server.
pub const OLLAMA_URL: &str = "http://localhost:11434";
/// The System One endpoint's path on either server.
pub const SYSTEM_ONE_PATH: &str = "/v1/systemone";

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
    /// The server, e.g. `http://mac.lan:11434`; the provider's own when
    /// absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
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
            if let Some(u) = &c.url {
                if !(u.starts_with("http://") || u.starts_with("https://")) {
                    return Err(format!("classifier url {u:?} is not http(s)"));
                }
            }
            match c.provider.as_deref() {
                None | Some("none") | Some(SYSTEM1) | Some(OLLAMA) => {}
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

    /// The resolution's id for a rule the coordinator names: `rule_N`, a
    /// rule's `name` (ignoring case), or `default`.
    pub fn find_rule(&self, name: &str) -> Option<String> {
        let name = name.trim();
        if name.eq_ignore_ascii_case(crate::classify::DEFAULT_CHOICE) {
            return Some(crate::classify::DEFAULT_CHOICE.into());
        }
        if self.rule(name).is_some() {
            return Some(name.to_string());
        }
        self.rules
            .iter()
            .position(|r| {
                r.name
                    .as_deref()
                    .is_some_and(|n| n.trim().eq_ignore_ascii_case(name))
            })
            .map(Self::rule_id)
    }

    /// The rules as the coordinator matches them when no classifier is
    /// configured: each rule's id, name and condition, then the default.
    pub fn menu(&self) -> String {
        let mut out: Vec<String> = self
            .rules
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let name = r
                    .name
                    .as_deref()
                    .map(|n| format!(" ({n})"))
                    .unwrap_or_default();
                format!("{}{name}: {}", Self::rule_id(i), r.when.trim())
            })
            .collect();
        out.push(format!(
            "{}: {}",
            crate::classify::DEFAULT_CHOICE,
            crate::classify::DEFAULT_WHEN
        ));
        out.join("; ")
    }
}

/// The classifier settings in effect.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassifierSettings {
    /// Whether the classifier is asked at all.
    pub on: bool,
    /// `system1` or `ollama`.
    pub provider: String,
    /// The System One endpoint.
    pub url: String,
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
            (Some(_), Some(SYSTEM1 | OLLAMA)) => true,
            (Some(_), _) => false,
        };
        let provider = match block.provider.as_deref() {
            Some(OLLAMA) => OLLAMA,
            _ => SYSTEM1,
        };
        let server = block.url.unwrap_or_else(|| {
            if provider == OLLAMA {
                OLLAMA_URL
            } else {
                SYSTEM1_URL
            }
            .into()
        });
        Self {
            on,
            provider: provider.into(),
            url: endpoint(&server),
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

/// The System One endpoint on `server`, which may already name it.
pub fn endpoint(server: &str) -> String {
    let server = server.trim_end_matches('/');
    if server.ends_with(SYSTEM_ONE_PATH) {
        server.to_string()
    } else {
        format!("{server}{SYSTEM_ONE_PATH}")
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
        assert_eq!(s.provider, SYSTEM1);
        assert_eq!(s.url, "https://api.typesafe.ai/v1/systemone");
    }

    #[test]
    fn ollama_classifier() {
        let local =
            DispatchConfig::parse(r#"{"classifier":{"provider":"ollama","model":"d1"}}"#).unwrap();
        let s = ClassifierSettings::from_config(&local, false);
        assert!(s.on);
        assert_eq!(s.provider, OLLAMA);
        assert_eq!(s.model, "d1");
        assert_eq!(s.url, "http://localhost:11434/v1/systemone");
        for url in [
            "http://mac.lan:11434",
            "http://mac.lan:11434/",
            "http://mac.lan:11434/v1/systemone",
        ] {
            let c = DispatchConfig::parse(&format!(
                r#"{{"classifier":{{"provider":"ollama","model":"d1","url":"{url}"}}}}"#
            ))
            .unwrap();
            let s = ClassifierSettings::from_config(&c, false);
            assert_eq!(s.url, "http://mac.lan:11434/v1/systemone", "{url}");
        }
        assert!(
            DispatchConfig::parse(r#"{"classifier":{"provider":"ollama","url":"mac.lan"}}"#)
                .is_err()
        );
        assert!(DispatchConfig::parse(r#"{"classifier":{"provider":"llama"}}"#).is_err());
    }
}
