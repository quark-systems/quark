//! Built-in harnesses. Each entry records what firstmate's verified adapter
//! for that CLI does (`.agents/skills/harness-adapters/references/harness/`
//! in the engine), so launches delegate to the engine's spawn unchanged.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use quark_systems::{
    AgentConfig, AgentRole, AuthState, Effort, HarnessAuth, HarnessModels, ModelSelection,
};

use super::{
    is_executable, probe_version, Account, Detection, Harness, HostEnv, Keys, LaunchError,
    LaunchPlan, SignalSource, Signals, TranscriptFormat, TranscriptSource,
};

use AgentRole::{Coordinator, Worker};
use Effort::{High, Low, Max, Medium, Xhigh};
use SignalSource::{Extension, Hooks, Screen, SessionLog};

const ANY_ROLE: &[AgentRole] = &[Coordinator, Worker];
const WORKER: &[AgentRole] = &[Worker];
const ALL_EFFORTS: &[Effort] = &[Low, Medium, High, Xhigh, Max];
const LMH: &[Effort] = &[Low, Medium, High];
const NO_EFFORT: &[Effort] = &[];
const ESC: &[&str] = &["Escape"];

/// Where a harness keeps credentials.
#[derive(Debug)]
pub struct AuthSpec {
    /// Environment variables that carry an API key.
    pub env: &'static [&'static str],
    /// Credential files relative to the account's config directory.
    pub files: &'static [&'static str],
    /// True when `env` and `files` are the harness's only credential
    /// sources, so finding none of them means it is not logged in.
    pub exhaustive: bool,
}

const AUTH_UNKNOWN: AuthSpec = AuthSpec {
    env: &[],
    files: &[],
    exhaustive: false,
};

/// Static description of one CLI harness.
#[derive(Debug)]
pub struct Spec {
    pub id: &'static str,
    pub name: &'static str,
    /// firstmate's adapter name for `fm-spawn.sh --harness`.
    pub engine: &'static str,
    /// Executable names looked up on `PATH`, in order.
    pub bins: &'static [&'static str],
    /// Home-relative executables tried when none is on `PATH`.
    pub fallback_bins: &'static [&'static str],
    pub roles: &'static [AgentRole],
    pub install_hint: &'static str,
    pub selection: ModelSelection,
    pub discovery: Option<&'static str>,
    pub efforts: &'static [Effort],
    /// Variable that points the harness at an account's config directory.
    pub account_env: Option<&'static str>,
    /// Home-relative config directory of the default account.
    pub config_dir: Option<&'static str>,
    pub auth: AuthSpec,
    pub signals: Signals,
    pub keys: Keys,
    /// Session-log format and its directory inside the config directory.
    pub transcript: Option<(TranscriptFormat, &'static str)>,
}

pub static SPECS: &[Spec] = &[
    Spec {
        id: "claude-code",
        name: "Claude Code",
        engine: "claude",
        bins: &["claude"],
        fallback_bins: &[".claude/local/claude"],
        roles: ANY_ROLE,
        install_hint: "npm install -g @anthropic-ai/claude-code",
        selection: ModelSelection::FreeForm,
        discovery: Some("the /model picker in Claude Code"),
        efforts: ALL_EFFORTS,
        account_env: Some("CLAUDE_CONFIG_DIR"),
        config_dir: Some(".claude"),
        // macOS keeps the login in the keychain, so a missing file proves nothing.
        auth: AuthSpec {
            env: &["ANTHROPIC_API_KEY"],
            files: &[".credentials.json"],
            exhaustive: false,
        },
        signals: Signals {
            busy: Hooks,
            turn_end: Hooks,
            summary: "Claude Code hooks (UserPromptSubmit, Stop)",
        },
        keys: Keys {
            interrupt: ESC,
            exit_command: "/exit",
        },
        transcript: Some((TranscriptFormat::ClaudeJsonl, "projects")),
    },
    Spec {
        id: "codex",
        name: "Codex",
        engine: "codex",
        bins: &["codex"],
        fallback_bins: &[],
        roles: ANY_ROLE,
        install_hint: "npm install -g @openai/codex",
        selection: ModelSelection::FreeForm,
        discovery: Some("the /model picker in Codex"),
        efforts: ALL_EFFORTS,
        account_env: Some("CODEX_HOME"),
        config_dir: Some(".codex"),
        auth: AuthSpec {
            env: &["OPENAI_API_KEY"],
            files: &["auth.json"],
            exhaustive: true,
        },
        signals: Signals {
            busy: Screen,
            turn_end: Hooks,
            summary: "Codex notify hook for turn end; busy state from the screen",
        },
        keys: Keys {
            interrupt: ESC,
            exit_command: "/quit",
        },
        transcript: Some((TranscriptFormat::CodexRollout, "sessions")),
    },
    Spec {
        id: "pi",
        name: "Pi",
        engine: "pi",
        bins: &["pi"],
        fallback_bins: &[],
        roles: ANY_ROLE,
        install_hint: "npm install -g @mariozechner/pi-coding-agent",
        selection: ModelSelection::FreeForm,
        discovery: Some("pi --list-models"),
        efforts: ALL_EFFORTS,
        account_env: Some("PI_CODING_AGENT_DIR"),
        config_dir: Some(".pi/agent"),
        // Pi also reads provider API keys from many variables.
        auth: AuthSpec {
            env: &[],
            files: &["auth.json"],
            exhaustive: false,
        },
        signals: Signals {
            busy: Extension,
            turn_end: Extension,
            summary: "engine extension (agent_start, agent_settled)",
        },
        keys: Keys {
            interrupt: ESC,
            exit_command: "/quit",
        },
        transcript: Some((TranscriptFormat::PiSession, "sessions")),
    },
    Spec {
        id: "opencode",
        name: "OpenCode",
        engine: "opencode",
        bins: &["opencode"],
        fallback_bins: &[],
        roles: WORKER,
        install_hint: "npm install -g opencode-ai",
        selection: ModelSelection::ProviderQualified,
        discovery: Some("opencode models"),
        efforts: NO_EFFORT,
        account_env: None,
        config_dir: None,
        auth: AUTH_UNKNOWN,
        signals: Signals {
            busy: Extension,
            turn_end: Extension,
            summary: "engine plugin (session.status)",
        },
        keys: Keys {
            interrupt: &["Escape", "Escape"],
            exit_command: "/exit",
        },
        transcript: None,
    },
    Spec {
        id: "cursor-agent",
        name: "Cursor Agent",
        engine: "cursor",
        bins: &["cursor-agent", "agent"],
        fallback_bins: &[],
        roles: WORKER,
        install_hint: "curl https://cursor.com/install -fsS | bash",
        selection: ModelSelection::FreeForm,
        discovery: Some("cursor-agent --list-models (effort is part of the model id)"),
        efforts: NO_EFFORT,
        account_env: None,
        config_dir: None,
        auth: AUTH_UNKNOWN,
        signals: Signals {
            busy: SessionLog,
            turn_end: SessionLog,
            summary: "Cursor conversation transcript",
        },
        keys: Keys {
            interrupt: ESC,
            exit_command: "/exit",
        },
        transcript: None,
    },
    Spec {
        id: "bob",
        name: "IBM Bob",
        engine: "bob",
        bins: &["bob"],
        fallback_bins: &[],
        roles: WORKER,
        install_hint: "Install IBM Bob and put `bob` on PATH",
        selection: ModelSelection::Automatic,
        discovery: None,
        efforts: NO_EFFORT,
        account_env: None,
        config_dir: Some(".bob"),
        auth: AuthSpec {
            env: &["BOB_API_KEY"],
            files: &["settings/auth-secrets.json"],
            exhaustive: true,
        },
        signals: Signals {
            busy: Hooks,
            turn_end: Hooks,
            summary: "Bob hooks (UserPromptSubmit, Stop)",
        },
        keys: Keys {
            interrupt: ESC,
            exit_command: "/exit",
        },
        transcript: None,
    },
    Spec {
        id: "omp",
        name: "Oh My Pi",
        engine: "omp",
        bins: &["omp"],
        fallback_bins: &[],
        roles: WORKER,
        install_hint: "Install Oh My Pi and put `omp` on PATH",
        selection: ModelSelection::ProviderQualified,
        discovery: Some("omp models"),
        efforts: ALL_EFFORTS,
        account_env: None,
        config_dir: None,
        auth: AUTH_UNKNOWN,
        signals: Signals {
            busy: Extension,
            turn_end: Extension,
            summary: "engine extension (agent_start, agent_end)",
        },
        keys: Keys {
            interrupt: ESC,
            exit_command: "/quit",
        },
        transcript: None,
    },
    Spec {
        id: "gemini",
        name: "Gemini CLI",
        engine: "gemini",
        bins: &["gemini"],
        fallback_bins: &[],
        roles: WORKER,
        install_hint: "npm install -g @google/gemini-cli",
        selection: ModelSelection::FreeForm,
        discovery: Some("the /model dialog in Gemini CLI"),
        efforts: NO_EFFORT,
        account_env: None,
        config_dir: None,
        auth: AuthSpec {
            env: &["GEMINI_API_KEY"],
            files: &[],
            exhaustive: false,
        },
        signals: Signals {
            busy: Hooks,
            turn_end: Hooks,
            summary: "Gemini CLI hooks (BeforeAgent, AfterAgent)",
        },
        keys: Keys {
            interrupt: ESC,
            exit_command: "/quit",
        },
        transcript: None,
    },
    Spec {
        id: "grok",
        name: "Grok Build",
        engine: "grok",
        bins: &["grok"],
        fallback_bins: &[],
        roles: WORKER,
        install_hint: "Install Grok Build and put `grok` on PATH",
        selection: ModelSelection::FreeForm,
        discovery: Some("grok models"),
        efforts: LMH,
        account_env: Some("GROK_HOME"),
        config_dir: Some(".grok"),
        auth: AUTH_UNKNOWN,
        signals: Signals {
            busy: Screen,
            turn_end: Hooks,
            summary: "Grok turn-end hook; busy state from the screen",
        },
        keys: Keys {
            interrupt: &["C-c"],
            exit_command: "/exit",
        },
        transcript: None,
    },
    Spec {
        id: "kimi",
        name: "Kimi Code",
        engine: "kimi",
        bins: &["kimi"],
        fallback_bins: &[".kimi-code/bin/kimi"],
        roles: WORKER,
        install_hint: "Install Kimi Code and put `kimi` on PATH",
        selection: ModelSelection::FreeForm,
        discovery: Some("kimi provider list --json"),
        efforts: NO_EFFORT,
        account_env: None,
        config_dir: None,
        auth: AUTH_UNKNOWN,
        signals: Signals {
            busy: Screen,
            turn_end: Hooks,
            summary: "Kimi turn-end hook; busy state from the screen",
        },
        keys: Keys {
            interrupt: ESC,
            exit_command: "/exit",
        },
        transcript: None,
    },
    Spec {
        id: "rovo",
        name: "Rovo Dev",
        engine: "rovo",
        bins: &["rovo"],
        fallback_bins: &[".local/bin/rovo"],
        roles: WORKER,
        install_hint: "Install the Rovo CLI and put `rovo` on PATH",
        selection: ModelSelection::FreeForm,
        discovery: Some("/models in a Rovo session"),
        efforts: &[Low, Medium, High, Max],
        account_env: None,
        config_dir: None,
        auth: AUTH_UNKNOWN,
        signals: Signals {
            busy: Screen,
            turn_end: Screen,
            summary: "terminal screen",
        },
        keys: Keys {
            interrupt: ESC,
            exit_command: "/exit",
        },
        transcript: None,
    },
    Spec {
        id: "antigravity",
        name: "Antigravity CLI",
        engine: "agy",
        bins: &["agy"],
        fallback_bins: &[],
        roles: WORKER,
        install_hint: "Install the Antigravity CLI and put `agy` on PATH",
        selection: ModelSelection::FreeForm,
        discovery: Some("agy models"),
        efforts: LMH,
        account_env: None,
        config_dir: None,
        auth: AUTH_UNKNOWN,
        signals: Signals {
            busy: Screen,
            turn_end: Screen,
            summary: "terminal screen",
        },
        keys: Keys {
            interrupt: ESC,
            exit_command: "/quit",
        },
        transcript: None,
    },
    Spec {
        id: "muse",
        name: "Muse Code",
        engine: "muse",
        bins: &["muse"],
        fallback_bins: &[],
        roles: WORKER,
        install_hint: "Install Muse Code and put `muse` on PATH",
        selection: ModelSelection::FreeForm,
        discovery: None,
        efforts: ALL_EFFORTS,
        account_env: None,
        config_dir: Some(".config/muse"),
        auth: AuthSpec {
            env: &["META_API_KEY"],
            files: &["auth.json"],
            exhaustive: true,
        },
        signals: Signals {
            busy: SessionLog,
            turn_end: SessionLog,
            summary: "Muse session event log",
        },
        keys: Keys {
            interrupt: &["Escape", "C-u"],
            exit_command: "/exit",
        },
        transcript: None,
    },
];

/// Every built-in harness, in display order.
pub fn all() -> Vec<Arc<dyn Harness>> {
    SPECS
        .iter()
        .map(|s| Arc::new(CliHarness(s)) as Arc<dyn Harness>)
        .collect()
}

/// A harness described by a [`Spec`].
#[derive(Debug, Clone, Copy)]
pub struct CliHarness(pub &'static Spec);

impl CliHarness {
    fn config_dir(&self, env: &HostEnv, account: &Account) -> Option<PathBuf> {
        if let Some(dir) = &account.config_dir {
            return Some(dir.clone());
        }
        if let Some(var) = self.0.account_env.and_then(|v| env.var(v)) {
            return Some(PathBuf::from(var));
        }
        self.0.config_dir.and_then(|d| env.home_join(d))
    }

    fn resolve(&self, env: &HostEnv) -> Option<PathBuf> {
        self.0.bins.iter().find_map(|b| env.which(b)).or_else(|| {
            self.0
                .fallback_bins
                .iter()
                .filter_map(|rel| env.home_join(rel))
                .find(|p| is_executable(p))
        })
    }
}

#[async_trait]
impl Harness for CliHarness {
    fn id(&self) -> &'static str {
        self.0.id
    }

    fn name(&self) -> &'static str {
        self.0.name
    }

    fn roles(&self) -> &'static [AgentRole] {
        self.0.roles
    }

    async fn detect(&self, env: &HostEnv) -> Detection {
        let Some(path) = self.resolve(env) else {
            return Detection::default();
        };
        let version = probe_version(&path).await;
        Detection {
            path: Some(path),
            version,
        }
    }

    fn install_hint(&self) -> &'static str {
        self.0.install_hint
    }

    fn models(&self) -> HarnessModels {
        HarnessModels {
            selection: self.0.selection,
            discovery: self.0.discovery.map(Into::into),
        }
    }

    fn efforts(&self) -> &'static [Effort] {
        self.0.efforts
    }

    fn auth_status(&self, env: &HostEnv, account: &Account) -> HarnessAuth {
        let auth = &self.0.auth;
        // An explicit account is checked by its directory alone; an ambient
        // API key belongs to the default account.
        if account.config_dir.is_none() {
            if let Some(var) = auth.env.iter().find(|v| env.var(v).is_some()) {
                return HarnessAuth {
                    state: AuthState::Configured,
                    detail: format!("{var} is set"),
                };
            }
        }
        let dir = self.config_dir(env, account);
        if let Some(dir) = &dir {
            if let Some(file) = auth.files.iter().map(|f| dir.join(f)).find(|p| p.is_file()) {
                return HarnessAuth {
                    state: AuthState::Configured,
                    detail: format!("found {}", file.display()),
                };
            }
        }
        let mut checked: Vec<String> = auth.env.iter().map(|v| v.to_string()).collect();
        if let Some(dir) = &dir {
            checked.extend(auth.files.iter().map(|f| dir.join(f).display().to_string()));
        }
        if checked.is_empty() {
            return HarnessAuth {
                state: AuthState::Unknown,
                detail: format!("{} has no credential Quark can check", self.0.name),
            };
        }
        HarnessAuth {
            state: if auth.exhaustive {
                AuthState::NotConfigured
            } else {
                AuthState::Unknown
            },
            detail: format!("none of {} found", checked.join(", ")),
        }
    }

    fn account_env(&self) -> Option<&'static str> {
        self.0.account_env
    }

    fn launch(
        &self,
        config: &AgentConfig,
        account: &Account,
        worktree: &Path,
    ) -> Result<LaunchPlan, LaunchError> {
        if config.harness != self.0.id {
            return Err(LaunchError::Invalid(format!(
                "config names `{}`, not `{}`",
                config.harness, self.0.id
            )));
        }
        let (errors, _) = self.validate(config);
        if let Some(e) = errors.first() {
            return Err(LaunchError::Invalid(e.message.clone()));
        }
        let mut env = Vec::new();
        if let Some(dir) = &account.config_dir {
            let var = self
                .0
                .account_env
                .ok_or_else(|| LaunchError::SingleAccount(self.0.name.into()))?;
            env.push((var.to_string(), dir.display().to_string()));
        }
        let model = match self.0.selection {
            ModelSelection::Automatic => None,
            _ => config.model.clone(),
        };
        Ok(LaunchPlan {
            engine_harness: self.0.engine,
            model,
            effort: config.effort.filter(|e| self.0.efforts.contains(e)),
            env,
            worktree: worktree.to_path_buf(),
        })
    }

    fn signals(&self) -> Signals {
        self.0.signals
    }

    fn transcript(&self, env: &HostEnv, account: &Account) -> Option<TranscriptSource> {
        let (format, sub) = self.0.transcript?;
        Some(TranscriptSource {
            format,
            root: self.config_dir(env, account)?.join(sub),
        })
    }

    fn keys(&self) -> Keys {
        self.0.keys
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_and_engine_names_are_unique() {
        let ids: HashSet<_> = SPECS.iter().map(|s| s.id).collect();
        let engines: HashSet<_> = SPECS.iter().map(|s| s.engine).collect();
        assert_eq!(ids.len(), SPECS.len());
        assert_eq!(engines.len(), SPECS.len());
    }

    #[test]
    fn coordinators_are_the_mvp_three() {
        let coordinators: Vec<_> = SPECS
            .iter()
            .filter(|s| s.roles.contains(&Coordinator))
            .map(|s| s.id)
            .collect();
        assert_eq!(coordinators, ["claude-code", "codex", "pi"]);
    }

    #[test]
    fn every_harness_can_interrupt_and_exit() {
        for s in SPECS {
            assert!(!s.keys.interrupt.is_empty(), "{}", s.id);
            assert!(s.keys.exit_command.starts_with('/'), "{}", s.id);
        }
    }
}
