//! The optional dispatch classifier (ADR-11): the `classifier` block of a
//! Project's `dispatch.yaml` and its user-level default in
//! `~/.quark/config.yaml`.
//!
//! ```yaml
//! classifier:
//!   provider: system1            # system1 | none
//!   model: <model-id>            # Quark ships no default model
//!   credential: keychain:quark/classifier   # a keychain reference, never an inline key
//!   confidence_floor: 0.6        # below this, on_failure decides
//!   timeout_ms: 5000
//!   on_failure: coordinator      # coordinator | default (fall back to the default rule)
//! ```
//!
//! Quark ships no classifier: the System-1 API is the contract, and the
//! provider behind it is the user's choice. Every field is optional in both
//! files. A field the Project sets overrides the user default, and what
//! neither sets takes the default here, so with no block anywhere the
//! provider is `none` and the coordinator picks every rule itself.
//!
//! Quark only reads, checks and merges the block. The engine's dispatch
//! resolution (`fm-dispatch-resolve.sh`) is what honors it: it reads the
//! block in effect from `config/crew-dispatch.json`, where
//! [`crate::crew_dispatch`] compiles it, looks the credential up in the
//! keychain by service name, asks the classifier, and applies the floor,
//! the timeout and `on_failure`. The block has exactly the fields the engine
//! accepts; the engine refuses any other.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The user-level file under the Quark home that carries the default block.
pub const USER_FILE: &str = "config.yaml";

pub const DEFAULT_CONFIDENCE_FLOOR: f64 = 0.6;
pub const DEFAULT_TIMEOUT_MS: u64 = 5000;
/// The longest a classifier may be given to answer.
pub const MAX_TIMEOUT_MS: u64 = 60_000;

const KEYCHAIN_PREFIX: &str = "keychain:";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    None,
    System1,
}

/// Where a task goes when the classifier answers below the floor, times out
/// or fails.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OnFailure {
    /// The coordinator picks the rule.
    #[default]
    Coordinator,
    /// The default rule is used.
    Default,
}

/// A `classifier` block as written in either file.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Block {
    #[serde(default)]
    provider: Option<Provider>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    credential: Option<String>,
    #[serde(default)]
    confidence_floor: Option<f64>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    on_failure: Option<OnFailure>,
}

/// A reference to a generic password in the OS keychain, by its service
/// name: `keychain:<service>`. The engine reads the key when it asks the
/// classifier; Quark never holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeychainRef {
    pub service: String,
}

impl KeychainRef {
    /// A service starts with a letter or digit and continues with letters,
    /// digits, `.`, `_`, `/`, `@` or `-`, as the engine accepts it. Never
    /// echoes `s`, which may be a key someone pasted inline.
    pub fn parse(s: &str) -> Result<Self, String> {
        let service = s.strip_prefix(KEYCHAIN_PREFIX).unwrap_or("");
        let named = service.starts_with(|c: char| c.is_ascii_alphanumeric())
            && service
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._/@-".contains(c));
        if !named {
            return Err(
                "credential must be a keychain reference such as keychain:quark/classifier, \
                 never an inline key"
                    .into(),
            );
        }
        Ok(Self {
            service: service.into(),
        })
    }
}

impl std::fmt::Display for KeychainRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{KEYCHAIN_PREFIX}{}", self.service)
    }
}

impl Serialize for KeychainRef {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

/// A classifier behind the System-1 API.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct System1 {
    pub model: String,
    /// `None` leaves the key to the engine's own environment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<KeychainRef>,
    pub confidence_floor: f64,
    pub timeout_ms: u64,
    pub on_failure: OnFailure,
}

/// The classifier in effect for a Project.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(tag = "provider", rename_all = "lowercase")]
pub enum Classifier {
    /// Every task goes to the coordinator.
    #[default]
    None,
    System1(System1),
}

/// Read one `classifier` block. `None` is an absent block.
pub fn block(v: Option<&serde_json::Value>) -> Result<Option<Block>, String> {
    let Some(v) = v else {
        return Ok(None);
    };
    if !v.is_object() {
        return Err("classifier must be a mapping".into());
    }
    let b = Block::deserialize(v).map_err(|e| format!("classifier: {e}"))?;
    check(&b).map_err(|e| format!("classifier: {e}"))?;
    Ok(Some(b))
}

/// What one block can be refused for on its own.
fn check(b: &Block) -> Result<(), String> {
    if let Some(c) = &b.credential {
        KeychainRef::parse(c)?;
    }
    if b.model.as_ref().is_some_and(|m| m.trim().is_empty()) {
        return Err("model is empty".into());
    }
    if b.confidence_floor
        .is_some_and(|f| !(0.0..=1.0).contains(&f))
    {
        return Err("confidence_floor must be between 0 and 1".into());
    }
    if b.timeout_ms
        .is_some_and(|t| !(1..=MAX_TIMEOUT_MS).contains(&t))
    {
        return Err(format!("timeout_ms must be between 1 and {MAX_TIMEOUT_MS}"));
    }
    Ok(())
}

/// The classifier a Project's block and the user default add up to. Each
/// field comes from the Project when it sets it, else from the user default.
pub fn effective(project: Option<&Block>, user: Option<&Block>) -> Result<Classifier, String> {
    let none = Block::default();
    let (p, u) = (project.unwrap_or(&none), user.unwrap_or(&none));
    if p.provider.or(u.provider).unwrap_or(Provider::None) == Provider::None {
        return Ok(Classifier::None);
    }
    let model = p
        .model
        .clone()
        .or_else(|| u.model.clone())
        .ok_or("classifier: provider system1 needs a model")?;
    let credential = p
        .credential
        .as_deref()
        .or(u.credential.as_deref())
        .map(KeychainRef::parse)
        .transpose()?;
    Ok(Classifier::System1(System1 {
        model,
        credential,
        confidence_floor: p
            .confidence_floor
            .or(u.confidence_floor)
            .unwrap_or(DEFAULT_CONFIDENCE_FLOOR),
        timeout_ms: p.timeout_ms.or(u.timeout_ms).unwrap_or(DEFAULT_TIMEOUT_MS),
        on_failure: p.on_failure.or(u.on_failure).unwrap_or_default(),
    }))
}

/// The user-level settings file. Keys other than `classifier` are not this
/// module's and are left alone.
#[derive(Debug, Default, Deserialize)]
struct UserFile {
    #[serde(default)]
    classifier: Option<serde_json::Value>,
}

/// The user default block in the text of `config.yaml`.
pub fn user_default(config_yaml: &str) -> Result<Option<Block>, String> {
    let fail = |e: String| format!("{USER_FILE}: {e}");
    let file: Option<UserFile> =
        serde_yaml_ng::from_str(config_yaml).map_err(|e| fail(e.to_string()))?;
    block(file.unwrap_or_default().classifier.as_ref()).map_err(fail)
}

/// The text of the user-level file at `path`, empty when there is none.
pub fn read_user_file(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("{USER_FILE}: {e}")),
    }
}

/// The user default block in the settings file at `path`.
pub fn user_default_at(path: &Path) -> Result<Option<Block>, String> {
    user_default(&read_user_file(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(yaml: &str) -> Result<Option<Block>, String> {
        let v: serde_json::Value = serde_yaml_ng::from_str(yaml).unwrap();
        block(Some(&v))
    }

    const JEV: &str = "provider: system1\nmodel: jev-1\ncredential: keychain:quark/classifier\n";

    #[test]
    fn provider_none_is_the_default() {
        assert_eq!(effective(None, None).unwrap(), Classifier::None);
        let b = parse("confidence_floor: 0.9").unwrap();
        assert_eq!(effective(b.as_ref(), None).unwrap(), Classifier::None);
        assert_eq!(
            serde_json::to_value(Classifier::None).unwrap(),
            json!({"provider": "none"})
        );
    }

    #[test]
    fn a_full_block_is_read_with_its_defaults() {
        let b = parse(JEV).unwrap();
        let c = effective(b.as_ref(), None).unwrap();
        assert_eq!(
            serde_json::to_value(&c).unwrap(),
            json!({
                "provider": "system1",
                "model": "jev-1",
                "credential": "keychain:quark/classifier",
                "confidence_floor": 0.6,
                "timeout_ms": 5000,
                "on_failure": "coordinator"
            })
        );
        let keyless = parse(
            "provider: system1\nmodel: open\nconfidence_floor: 0.75\ntimeout_ms: 500\n\
             on_failure: default\n",
        )
        .unwrap();
        let c = effective(keyless.as_ref(), None).unwrap();
        assert_eq!(
            serde_json::to_value(&c).unwrap(),
            json!({
                "provider": "system1",
                "model": "open",
                "confidence_floor": 0.75,
                "timeout_ms": 500,
                "on_failure": "default"
            })
        );
    }

    #[test]
    fn the_project_overrides_the_user_default_field_by_field() {
        let user = user_default(&format!(
            "theme: dark\nclassifier:\n  {}",
            JEV.replace('\n', "\n  ")
        ))
        .unwrap();
        // No Project block: the user default applies whole.
        let Classifier::System1(s) = effective(None, user.as_ref()).unwrap() else {
            panic!("system1");
        };
        assert_eq!(s.model, "jev-1");

        let project = parse("model: other\nconfidence_floor: 0.8\non_failure: default").unwrap();
        let Classifier::System1(s) = effective(project.as_ref(), user.as_ref()).unwrap() else {
            panic!("system1");
        };
        assert_eq!(s.model, "other");
        assert_eq!(s.confidence_floor, 0.8);
        assert_eq!(s.timeout_ms, DEFAULT_TIMEOUT_MS);
        assert_eq!(s.on_failure, OnFailure::Default);
        assert_eq!(
            s.credential.unwrap().to_string(),
            "keychain:quark/classifier"
        );

        let off = parse("provider: none").unwrap();
        assert_eq!(
            effective(off.as_ref(), user.as_ref()).unwrap(),
            Classifier::None
        );
    }

    #[test]
    fn credentials_are_keychain_references_only() {
        for (reference, service) in [
            ("keychain:quark/classifier", "quark/classifier"),
            ("keychain:quark", "quark"),
            ("keychain:ai.typesafe_key@work-1", "ai.typesafe_key@work-1"),
        ] {
            let r = KeychainRef::parse(reference).unwrap();
            assert_eq!(r.service, service);
            assert_eq!(r.to_string(), reference);
        }
        for inline in [
            "sk-live-abc123",
            "keychain:",
            "keychain:/x",
            "keychain:-x",
            "keychain:quark/a b",
            "keychain:quark:classifier",
            "env:TYPESAFE_API_KEY",
        ] {
            let err = parse(&format!("credential: \"{inline}\"")).unwrap_err();
            assert!(err.contains("never an inline key"), "{inline}: {err}");
            assert!(!err.contains("sk-live"), "the key is not echoed: {err}");
        }
        let err = parse("api_key: sk-live-abc123").unwrap_err();
        assert!(err.contains("unknown field `api_key`"), "{err}");
        assert!(!err.contains("sk-live"), "{err}");
    }

    #[test]
    fn refuses_what_the_engine_would_not_accept() {
        for (bad, why) in [
            ("provider: jev", "unknown variant `jev`"),
            ("provider: system1", "provider system1 needs a model"),
            ("endpoint: https://x.test", "unknown field `endpoint`"),
            ("model: \" \"", "model is empty"),
            (
                "confidence_floor: 1.5",
                "confidence_floor must be between 0 and 1",
            ),
            (
                "confidence_floor: -0.1",
                "confidence_floor must be between 0 and 1",
            ),
            ("timeout_ms: 0", "timeout_ms must be between 1 and 60000"),
            (
                "timeout_ms: 600000",
                "timeout_ms must be between 1 and 60000",
            ),
            ("timeout_ms: 1.5", "invalid type"),
            ("on_failure: retry", "unknown variant `retry`"),
            ("floor: 0.6", "unknown field `floor`"),
        ] {
            let err = parse(bad)
                .and_then(|b| effective(b.as_ref(), None))
                .unwrap_err();
            assert!(err.contains(why), "{bad}: {err}");
            assert!(err.starts_with("classifier"), "{err}");
        }
    }

    #[test]
    fn reads_the_user_file() {
        assert_eq!(user_default("").unwrap(), None);
        assert_eq!(user_default("# nothing\n").unwrap(), None);
        assert_eq!(user_default("theme: dark\n").unwrap(), None);
        let err = user_default("classifier:\n  credential: sk-live-abc123\n").unwrap_err();
        assert!(
            err.starts_with("config.yaml: classifier: credential must be"),
            "{err}"
        );
        assert!(user_default("classifier: [")
            .unwrap_err()
            .starts_with("config.yaml: "));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(USER_FILE);
        assert_eq!(user_default_at(&path).unwrap(), None);
        std::fs::write(&path, "classifier:\n  provider: none\n").unwrap();
        assert!(user_default_at(&path).unwrap().is_some());
    }
}
