//! The harness registry: one [`Harness`] per agent CLI the daemon can run.
//!
//! A harness covers detection, models and effort, credential health per
//! account, launch, turn signals, the session-log transcript and the
//! interrupt and exit keys (ADR-9). The MVP implementations in [`builtin`]
//! describe firstmate's verified adapters, so a launch delegates to the
//! engine's own spawn; a harness that needs custom logic implements the
//! trait directly and registers next to them.

mod builtin;

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use quark_systems::{
    AgentConfig, AgentConfigValidation, AgentRole, ConfigIssue, Effort, HarnessAuth, HarnessInfo,
    HarnessInstall, HarnessModels, HarnessSupervision, ModelSelection,
};

pub use builtin::{all as builtin, CliHarness, Spec, SPECS};

/// How long a detection result is reused before `GET /v1/harnesses` probes
/// the executables again.
pub const DETECT_TTL: Duration = Duration::from_secs(30);

/// Longest a `--version` probe may run.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Longest model id accepted in an agent config.
const MAX_MODEL_LEN: usize = 128;

/// The parts of the host environment harness checks read. Tests build one
/// by hand so detection and credential checks never touch the real machine.
#[derive(Debug, Clone, Default)]
pub struct HostEnv {
    pub path: Option<OsString>,
    pub home: Option<PathBuf>,
    pub vars: HashMap<String, String>,
}

impl HostEnv {
    /// Snapshot of this process's environment.
    pub fn current() -> Self {
        Self {
            path: std::env::var_os("PATH"),
            home: std::env::var_os("HOME").map(PathBuf::from),
            vars: std::env::vars().collect(),
        }
    }

    /// A non-empty variable.
    pub fn var(&self, name: &str) -> Option<&str> {
        self.vars
            .get(name)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    /// `~/rel`, when a home directory is known.
    pub fn home_join(&self, rel: &str) -> Option<PathBuf> {
        self.home.as_ref().map(|h| h.join(rel))
    }

    /// The first executable named `name` on `PATH`.
    pub fn which(&self, name: &str) -> Option<PathBuf> {
        let path = self.path.as_ref()?;
        std::env::split_paths(path)
            .map(|dir| dir.join(name))
            .find(|p| is_executable(p))
    }
}

pub(crate) fn is_executable(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// An account a harness runs under: its own config directory, or the
/// harness's default location when `config_dir` is absent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Account {
    pub config_dir: Option<PathBuf>,
}

/// Where busy and turn-end state come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalSource {
    /// The harness's own lifecycle hooks.
    Hooks,
    /// An engine-owned extension or plugin loaded into the harness.
    Extension,
    /// The harness's durable session or event log.
    SessionLog,
    /// The terminal screen. Lower confidence.
    Screen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signals {
    pub busy: SignalSource,
    pub turn_end: SignalSource,
    /// Human-readable summary for the API.
    pub summary: &'static str,
}

impl Signals {
    pub fn confidence(&self) -> quark_systems::SupervisionConfidence {
        if self.busy == SignalSource::Screen || self.turn_end == SignalSource::Screen {
            quark_systems::SupervisionConfidence::Low
        } else {
            quark_systems::SupervisionConfidence::High
        }
    }
}

/// A terminal key, in tmux `send-keys` notation.
pub type Key = &'static str;

/// Interrupt and exit sequences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keys {
    /// Keys that cancel the current turn without exiting.
    pub interrupt: &'static [Key],
    /// Slash command typed and submitted to exit.
    pub exit_command: &'static str,
}

/// Session-log formats the transcript tap can parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptFormat {
    ClaudeJsonl,
    CodexRollout,
    PiSession,
}

/// Where a harness writes its session log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptSource {
    pub format: TranscriptFormat,
    /// Root directory holding the session logs for the account.
    pub root: PathBuf,
}

/// What the engine needs to start an agent: the engine's own adapter name,
/// the validated model and effort, and environment for the account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    /// The engine adapter name, e.g. firstmate's `claude`. Internal only.
    pub engine_harness: &'static str,
    pub model: Option<String>,
    /// Absent when unset or when the harness has no control for it.
    pub effort: Option<Effort>,
    pub env: Vec<(String, String)>,
    pub worktree: PathBuf,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LaunchError {
    #[error("invalid agent config: {0}")]
    Invalid(String),
    #[error("{0} supports only its default account")]
    SingleAccount(String),
}

/// One agent CLI.
#[async_trait]
pub trait Harness: Send + Sync {
    /// Stable id used in agent configs and dispatch rules.
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    fn roles(&self) -> &'static [AgentRole];

    /// Installed executable and version.
    async fn detect(&self, env: &HostEnv) -> Detection;
    fn install_hint(&self) -> &'static str;

    fn models(&self) -> HarnessModels;
    /// Accepted effort levels; empty when there is no effort control.
    fn efforts(&self) -> &'static [Effort];

    /// Errors and warnings for `config`, which names this harness.
    fn validate(&self, config: &AgentConfig) -> (Vec<ConfigIssue>, Vec<ConfigIssue>) {
        default_validate(self, config)
    }

    /// Credential health for `account`.
    fn auth_status(&self, env: &HostEnv, account: &Account) -> HarnessAuth;

    /// The environment variable that points the harness at an account's
    /// config directory, when it supports more than one account.
    fn account_env(&self) -> Option<&'static str>;

    fn launch(
        &self,
        config: &AgentConfig,
        account: &Account,
        worktree: &Path,
    ) -> Result<LaunchPlan, LaunchError>;

    fn signals(&self) -> Signals;
    /// The session log the transcript tap reads, when the format is known.
    fn transcript(&self, env: &HostEnv, account: &Account) -> Option<TranscriptSource>;
    fn keys(&self) -> Keys;
}

/// Result of [`Harness::detect`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Detection {
    pub path: Option<PathBuf>,
    pub version: Option<String>,
}

/// Runs `<exe> --version` with a timeout and returns its first non-empty
/// output line.
pub(crate) async fn probe_version(exe: &Path) -> Option<String> {
    let run = tokio::process::Command::new(exe)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(PROBE_TIMEOUT, run).await.ok()?.ok()?;
    let text = if out.stdout.is_empty() {
        out.stderr
    } else {
        out.stdout
    };
    String::from_utf8_lossy(&text)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(200).collect())
}

fn issue(field: &str, code: &str, message: impl Into<String>) -> ConfigIssue {
    ConfigIssue {
        field: field.into(),
        code: code.into(),
        message: message.into(),
    }
}

/// Model ids are passed to the harness as a single argument, so they must
/// not be empty, start with `-` or carry whitespace or shell metacharacters.
fn model_id_ok(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= MAX_MODEL_LEN
        && !model.starts_with('-')
        && model.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/' | '@' | '[' | ']')
        })
}

fn default_validate<H: Harness + ?Sized>(
    h: &H,
    config: &AgentConfig,
) -> (Vec<ConfigIssue>, Vec<ConfigIssue>) {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let models = h.models();
    if let Some(model) = config.model.as_deref() {
        if models.selection == ModelSelection::Automatic {
            warnings.push(issue(
                "model",
                "model_ignored",
                format!(
                    "{} picks its model automatically; `{model}` is ignored",
                    h.name()
                ),
            ));
        } else if !model_id_ok(model) {
            errors.push(issue(
                "model",
                "invalid_model",
                format!("`{model}` is not a valid model id"),
            ));
        } else if models.selection == ModelSelection::ProviderQualified
            && !model
                .split_once('/')
                .is_some_and(|(p, m)| !p.is_empty() && !m.is_empty())
        {
            errors.push(issue(
                "model",
                "model_needs_provider",
                format!("{} needs a `provider/model` id, got `{model}`", h.name()),
            ));
        }
    }
    if let Some(raw) = config.effort.as_deref() {
        let accepted = h.efforts();
        let Some(effort) = Effort::parse(raw) else {
            let all: Vec<_> = Effort::ALL.iter().map(|e| e.as_str()).collect();
            errors.push(issue(
                "effort",
                "invalid_effort",
                format!("effort `{raw}` must be one of {}", all.join(", ")),
            ));
            return (errors, warnings);
        };
        if accepted.is_empty() {
            warnings.push(issue(
                "effort",
                "effort_ignored",
                format!(
                    "{} has no effort control; `{}` is ignored",
                    h.name(),
                    effort.as_str()
                ),
            ));
        } else if !accepted.contains(&effort) {
            let list: Vec<_> = accepted.iter().map(|e| e.as_str()).collect();
            errors.push(issue(
                "effort",
                "unsupported_effort",
                format!(
                    "{} accepts effort {}; `{}` is not one of them",
                    h.name(),
                    list.join(", "),
                    effort.as_str()
                ),
            ));
        }
    }
    (errors, warnings)
}

/// Every harness the daemon knows, in display order.
pub struct HarnessRegistry {
    harnesses: Vec<Arc<dyn Harness>>,
    env: HostEnv,
    cache: Mutex<Option<(Instant, Vec<HarnessInfo>)>>,
}

impl HarnessRegistry {
    /// The built-in harnesses against this process's environment.
    pub fn builtin() -> Self {
        Self::new(builtin(), HostEnv::current())
    }

    pub fn new(harnesses: Vec<Arc<dyn Harness>>, env: HostEnv) -> Self {
        Self {
            harnesses,
            env,
            cache: Mutex::new(None),
        }
    }

    pub fn get(&self, id: &str) -> Option<&Arc<dyn Harness>> {
        self.harnesses.iter().find(|h| h.id() == id)
    }

    pub fn ids(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.harnesses.iter().map(|h| h.id())
    }

    /// Checks `config` for `role`. Unknown harnesses, unsupported roles,
    /// malformed models and unsupported efforts are errors; settings the
    /// harness would ignore are warnings.
    pub fn validate(&self, config: &AgentConfig, role: AgentRole) -> AgentConfigValidation {
        let Some(h) = self.get(&config.harness) else {
            let known: Vec<_> = self.ids().collect();
            return AgentConfigValidation {
                valid: false,
                errors: vec![issue(
                    "harness",
                    "unknown_harness",
                    format!(
                        "unknown harness `{}`; known: {}",
                        config.harness,
                        known.join(", ")
                    ),
                )],
                warnings: Vec::new(),
            };
        };
        let (mut errors, warnings) = h.validate(config);
        if !h.roles().contains(&role) {
            let role = match role {
                AgentRole::Coordinator => "coordinator",
                AgentRole::Worker => "worker",
            };
            errors.insert(
                0,
                issue(
                    "role",
                    "unsupported_role",
                    format!("{} cannot run as a {role}", h.name()),
                ),
            );
        }
        AgentConfigValidation {
            valid: errors.is_empty(),
            errors,
            warnings,
        }
    }

    /// Detection, models, efforts and default-account health for every
    /// harness. Probes run concurrently and are cached for [`DETECT_TTL`].
    pub async fn list(&self, refresh: bool) -> Vec<HarnessInfo> {
        if !refresh {
            if let Some((at, infos)) = self.cache.lock().unwrap().as_ref() {
                if at.elapsed() < DETECT_TTL {
                    return infos.clone();
                }
            }
        }
        let detections =
            futures_util::future::join_all(self.harnesses.iter().map(|h| h.detect(&self.env)))
                .await;
        let account = Account::default();
        let infos: Vec<HarnessInfo> = self
            .harnesses
            .iter()
            .zip(detections)
            .map(|(h, d)| {
                let signals = h.signals();
                HarnessInfo {
                    id: h.id().into(),
                    name: h.name().into(),
                    roles: h.roles().to_vec(),
                    install: HarnessInstall {
                        installed: d.path.is_some(),
                        version: d.version,
                        path: d.path.map(|p| p.display().to_string()),
                        install_hint: h.install_hint().into(),
                    },
                    models: h.models(),
                    efforts: h.efforts().to_vec(),
                    auth: h.auth_status(&self.env, &account),
                    supervision: HarnessSupervision {
                        confidence: signals.confidence(),
                        source: signals.summary.into(),
                    },
                    transcript: h.transcript(&self.env, &account).is_some(),
                    account_env: h.account_env().map(Into::into),
                }
            })
            .collect();
        *self.cache.lock().unwrap() = Some((Instant::now(), infos.clone()));
        infos
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_ids() {
        for ok in ["claude-sonnet-5", "openai/gpt-5.6", "sonnet[1m]", "x@y:1"] {
            assert!(model_id_ok(ok), "{ok}");
        }
        for bad in ["", "-rf", "a b", "a;b", "$(x)", &"m".repeat(200)] {
            assert!(!model_id_ok(bad), "{bad}");
        }
    }
}
