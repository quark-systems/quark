//! Declarative harness manifests.
//!
//! Each agent CLI is described by one TOML manifest loaded at runtime, so a
//! new harness needs no quarkd release. This module defines the schema as
//! serde types; `quark-harness` owns parsing (TOML), the built-in manifests
//! and the plugin host. Field names are the TOML keys.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::Result;

/// Current manifest schema version, the `schema` key.
pub const MANIFEST_SCHEMA: u32 = 1;

/// Values of `turn_signals.busy` and `turn_signals.turn_end`.
pub const SIGNAL_SOURCES: &[&str] = &["hooks", "extension", "transcript", "pane", "none"];

/// One harness, as written in `<id>.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessManifest {
    pub schema: u32,
    /// Stable id such as `claude-code`.
    pub id: String,
    pub name: String,
    /// Other names an engine reports for this harness, such as firstmate's
    /// adapter name `claude` for `claude-code`.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Which neutral roles may run on it (`coordinator`, `worker`, ...).
    #[serde(default)]
    pub roles: Vec<String>,
    pub detect: Detect,
    #[serde(default)]
    pub models: Models,
    /// Effort levels the harness accepts, lowest first; empty for none.
    #[serde(default)]
    pub efforts: Vec<String>,
    pub launch: Launch,
    #[serde(default)]
    pub account: Option<Account>,
    #[serde(default)]
    pub hooks: Option<Hooks>,
    pub turn_signals: TurnSignals,
    #[serde(default)]
    pub transcript: Option<TranscriptLocator>,
    pub keys: Keys,
    #[serde(default)]
    pub quota: Option<QuotaSource>,
    /// Escape hatch for behavior a manifest cannot express.
    #[serde(default)]
    pub plugin: Option<Plugin>,
}

/// How to find the executable and tell it is installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Detect {
    /// Executable names looked up on `PATH`, in order.
    pub bins: Vec<String>,
    /// Home-relative paths tried when none is on `PATH`.
    #[serde(default)]
    pub fallback_bins: Vec<String>,
    /// Arguments that print a version, such as `["--version"]`.
    #[serde(default)]
    pub version_args: Vec<String>,
    #[serde(default)]
    pub install_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Models {
    /// `free_form` (any id), `provider_qualified` (a `provider/model` id),
    /// `listed` (only `known`), `automatic` (the harness picks; a configured
    /// model is ignored) or `none`.
    #[serde(default = "default_selection")]
    pub selection: String,
    #[serde(default)]
    pub known: Vec<String>,
    #[serde(default)]
    pub default: Option<String>,
    /// Where a person can see the list, shown in the app.
    #[serde(default)]
    pub discovery: Option<String>,
}

fn default_selection() -> String {
    "free_form".into()
}

impl Default for Models {
    fn default() -> Self {
        Self {
            selection: default_selection(),
            known: Vec::new(),
            default: None,
            discovery: None,
        }
    }
}

/// Launch command. `{model}`, `{effort}`, `{prompt}`, `{prompt_file}` and
/// `{cwd}` are substituted anywhere inside an argument; an argument whose
/// placeholder has no value is dropped. `optional` flags go just before the
/// first argument carrying `{prompt}` or `{prompt_file}`, else at the end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Launch {
    pub argv: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Flag/placeholder pairs such as `["--model", "{model}"]` added only
    /// when the placeholder has a value.
    #[serde(default)]
    pub optional: Vec<Vec<String>>,
    /// How the brief reaches the agent: `argv` (through `{prompt}` or
    /// `{prompt_file}`), `stdin`, or `paste` (typed into the composer once
    /// the agent is ready).
    #[serde(default = "default_prompt_via")]
    pub prompt_via: String,
}

fn default_prompt_via() -> String {
    "paste".into()
}

/// Per-account configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Account {
    /// Variable pointing the harness at an account's config directory;
    /// absent when the harness supports only its default account.
    #[serde(default)]
    pub env: Option<String>,
    /// Home-relative config directory of the default account.
    pub config_dir: String,
    /// API-key variables.
    #[serde(default)]
    pub auth_env: Vec<String>,
    /// Credential files relative to the config directory.
    #[serde(default)]
    pub auth_files: Vec<String>,
    /// `auth_env` and `auth_files` are the only credential sources.
    #[serde(default)]
    pub auth_exhaustive: bool,
}

/// Harness hooks that post events to quarkd.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hooks {
    /// `claude-settings`, `codex-config`, `gemini-settings`, ...: how
    /// `quark-harness` installs them for one task.
    pub install: String,
    /// Harness hook names mapped to neutral worker signals (`busy`,
    /// `turn_end`, `report`).
    #[serde(default)]
    pub events: BTreeMap<String, String>,
}

/// How quarkd tells the agent is busy and when its turn ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnSignals {
    /// `hooks`, `extension` (an engine-owned plugin loaded into the
    /// harness), `transcript`, `pane` or `none`.
    pub busy: String,
    pub turn_end: String,
    /// One line for people, such as `Claude Code hooks (UserPromptSubmit,
    /// Stop)`.
    #[serde(default)]
    pub summary: Option<String>,
    /// Pane text that means the agent is waiting, for `pane` signals.
    #[serde(default)]
    pub idle_patterns: Vec<String>,
}

/// Where the session log lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptLocator {
    /// A format `quark-transcript` reads, such as `claude-jsonl`.
    pub format: String,
    /// Directory inside the account's config directory.
    pub dir: String,
    /// How a session maps to a file: `cwd-slug`, `newest` or `session-id`.
    #[serde(default = "default_key")]
    pub key: String,
}

fn default_key() -> String {
    "cwd-slug".into()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keys {
    /// Key names sent to interrupt a turn, such as `["Escape"]`.
    pub interrupt: Vec<String>,
    /// Text typed to exit cleanly, such as `/exit`.
    pub exit: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaSource {
    /// `quota-axi` provider name, or `none`.
    pub provider: String,
}

/// Behavior implemented outside the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Plugin {
    /// A WASM module path relative to the manifest.
    Wasm { module: String },
    /// A subprocess speaking JSON lines on stdin and stdout.
    Subprocess { argv: Vec<String> },
}

impl HarnessManifest {
    /// Checks a loader runs after parsing, before registering the manifest.
    pub fn validate(&self) -> Result<()> {
        let bad = |m: String| {
            Err(crate::CoreError::Invalid(format!(
                "harness {}: {m}",
                self.id
            )))
        };
        if self.schema != MANIFEST_SCHEMA {
            return bad(format!("schema {} (want {MANIFEST_SCHEMA})", self.schema));
        }
        if self.id.is_empty()
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return bad("id must be lowercase letters, digits and dashes".into());
        }
        if self.detect.bins.is_empty() {
            return bad("detect.bins is empty".into());
        }
        if self.launch.argv.is_empty() {
            return bad("launch.argv is empty".into());
        }
        if !matches!(self.launch.prompt_via.as_str(), "argv" | "stdin" | "paste") {
            return bad(format!("launch.prompt_via {:?}", self.launch.prompt_via));
        }
        if self.launch.prompt_via == "argv"
            && !self
                .launch
                .argv
                .iter()
                .any(|a| a.contains("{prompt}") || a.contains("{prompt_file}"))
        {
            return bad("launch.prompt_via is argv but no argument has {prompt}".into());
        }
        if !matches!(
            self.models.selection.as_str(),
            "free_form" | "provider_qualified" | "listed" | "automatic" | "none"
        ) {
            return bad(format!("models.selection {:?}", self.models.selection));
        }
        for (key, v) in [
            ("busy", &self.turn_signals.busy),
            ("turn_end", &self.turn_signals.turn_end),
        ] {
            if !SIGNAL_SOURCES.contains(&v.as_str()) {
                return bad(format!("turn_signals.{key} {v:?}"));
            }
        }
        Ok(())
    }
}

/// The set of manifests quarkd knows, built-in and user-supplied.
#[async_trait]
pub trait HarnessRegistry: Send + Sync {
    async fn list(&self) -> Result<Vec<HarnessManifest>>;

    async fn get(&self, id: &str) -> Result<HarnessManifest> {
        self.list()
            .await?
            .into_iter()
            .find(|m| m.id == id)
            .ok_or_else(|| crate::CoreError::NotFound(format!("harness {id}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude() -> HarnessManifest {
        serde_json::from_value(serde_json::json!({
            "schema": 1,
            "id": "claude-code",
            "name": "Claude Code",
            "roles": ["coordinator", "worker"],
            "detect": { "bins": ["claude"], "version_args": ["--version"] },
            "efforts": ["low", "medium", "high", "xhigh", "max"],
            "launch": {
                "argv": ["claude"],
                "optional": [["--model", "{model}"], ["--effort", "{effort}"]]
            },
            "account": { "env": "CLAUDE_CONFIG_DIR", "config_dir": ".claude" },
            "turn_signals": { "busy": "hooks", "turn_end": "hooks" },
            "transcript": { "format": "claude-jsonl", "dir": "projects" },
            "keys": { "interrupt": ["Escape"], "exit": "/exit" }
        }))
        .unwrap()
    }

    #[test]
    fn parses_with_defaults_and_validates() {
        let m = claude();
        assert_eq!(m.models.selection, "free_form");
        assert_eq!(m.launch.prompt_via, "paste");
        assert_eq!(m.transcript.as_ref().unwrap().key, "cwd-slug");
        m.validate().unwrap();
    }

    #[test]
    fn rejects_bad_manifests() {
        let mut m = claude();
        m.id = "Claude Code".into();
        assert!(m.validate().is_err());
        let mut m = claude();
        m.turn_signals.busy = "telepathy".into();
        assert!(m.validate().is_err());
        let mut m = claude();
        m.schema = 2;
        assert!(m.validate().is_err());
        let unknown = serde_json::json!({ "schema": 1, "bogus": true });
        assert!(serde_json::from_value::<HarnessManifest>(unknown).is_err());
    }
}
