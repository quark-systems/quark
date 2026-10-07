//! Harnesses described by `quark-harness` manifests. The built-in manifests
//! record what firstmate's verified adapters do, so a launch delegates to
//! the engine's spawn under the manifest's engine name.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use quark_core::harness::HarnessManifest;
use quark_harness::ManifestRegistry;
use quark_systems::{AgentConfig, AgentRole, Effort, HarnessAuth, HarnessModels, ModelSelection};

use super::{
    Account, Detection, Harness, HostEnv, Keys, LaunchError, LaunchPlan, SignalSource, Signals,
    TranscriptFormat, TranscriptSource,
};

/// Every built-in harness, in display order.
pub fn all() -> Vec<Arc<dyn Harness>> {
    from_manifests(&ManifestRegistry::builtin())
}

/// One harness per manifest in `reg`, in its order.
pub fn from_manifests(reg: &ManifestRegistry) -> Vec<Arc<dyn Harness>> {
    reg.all()
        .iter()
        .map(|m| Arc::new(ManifestHarness::new(m.clone())) as Arc<dyn Harness>)
        .collect()
}

/// A harness read from its manifest. The enum views quarkd's API uses are
/// computed once.
#[derive(Debug, Clone)]
pub struct ManifestHarness {
    m: Arc<HarnessManifest>,
    roles: Vec<AgentRole>,
    efforts: Vec<Effort>,
    selection: ModelSelection,
    signals: Signals,
    transcript: Option<TranscriptFormat>,
}

fn signal_source(s: &str) -> SignalSource {
    match s {
        "hooks" => SignalSource::Hooks,
        "extension" => SignalSource::Extension,
        "transcript" => SignalSource::SessionLog,
        _ => SignalSource::Screen,
    }
}

impl ManifestHarness {
    pub fn new(m: Arc<HarnessManifest>) -> Self {
        let roles = m
            .roles
            .iter()
            .filter_map(|r| match r.as_str() {
                "coordinator" => Some(AgentRole::Coordinator),
                "worker" => Some(AgentRole::Worker),
                _ => None,
            })
            .collect();
        let efforts = m.efforts.iter().filter_map(|e| Effort::parse(e)).collect();
        let selection = match m.models.selection.as_str() {
            "provider_qualified" => ModelSelection::ProviderQualified,
            "automatic" | "none" => ModelSelection::Automatic,
            _ => ModelSelection::FreeForm,
        };
        let busy = signal_source(&m.turn_signals.busy);
        let turn_end = signal_source(&m.turn_signals.turn_end);
        let signals = Signals {
            busy,
            turn_end,
            summary: m.turn_signals.summary.clone().unwrap_or_else(|| {
                format!("{} / {}", m.turn_signals.busy, m.turn_signals.turn_end)
            }),
        };
        let transcript = m.transcript.as_ref().and_then(|t| match t.format.as_str() {
            "claude-jsonl" => Some(TranscriptFormat::ClaudeJsonl),
            "codex-rollout" => Some(TranscriptFormat::CodexRollout),
            "pi-session" => Some(TranscriptFormat::PiSession),
            _ => None,
        });
        Self {
            m,
            roles,
            efforts,
            selection,
            signals,
            transcript,
        }
    }

    pub fn manifest(&self) -> &HarnessManifest {
        &self.m
    }

    fn config_dir(&self, env: &HostEnv, account: &Account) -> Option<PathBuf> {
        quark_harness::config_dir(&self.m, env, account.config_dir.as_deref())
    }
}

#[async_trait]
impl Harness for ManifestHarness {
    fn id(&self) -> &str {
        &self.m.id
    }

    fn name(&self) -> &str {
        &self.m.name
    }

    fn engine_name(&self) -> &str {
        quark_harness::engine_name(&self.m)
    }

    fn roles(&self) -> &[AgentRole] {
        &self.roles
    }

    async fn detect(&self, env: &HostEnv) -> Detection {
        let Some(path) = quark_harness::resolve_bin(&self.m, env) else {
            return Detection::default();
        };
        let version = quark_harness::probe_version(&path, &self.m.detect.version_args).await;
        Detection {
            path: Some(path),
            version,
        }
    }

    fn install_hint(&self) -> &str {
        self.m.detect.install_hint.as_deref().unwrap_or("")
    }

    fn models(&self) -> HarnessModels {
        HarnessModels {
            selection: self.selection,
            discovery: self.m.models.discovery.clone(),
        }
    }

    fn efforts(&self) -> &[Effort] {
        &self.efforts
    }

    fn auth_status(&self, env: &HostEnv, account: &Account) -> HarnessAuth {
        quark_harness::auth_status(&self.m, env, account.config_dir.as_deref())
    }

    fn account_env(&self) -> Option<&str> {
        self.m.account.as_ref()?.env.as_deref()
    }

    fn default_config_dir(&self, env: &HostEnv) -> Option<PathBuf> {
        self.config_dir(env, &Account::default())
    }

    fn quota_provider(&self) -> Option<&str> {
        self.m
            .quota
            .as_ref()
            .map(|q| q.provider.as_str())
            .filter(|p| *p != "none")
    }

    fn launch(
        &self,
        config: &AgentConfig,
        account: &Account,
        worktree: &Path,
    ) -> Result<LaunchPlan, LaunchError> {
        if config.harness != self.m.id {
            return Err(LaunchError::Invalid(format!(
                "config names `{}`, not `{}`",
                config.harness, self.m.id
            )));
        }
        let (errors, _) = self.validate(config);
        if let Some(e) = errors.first() {
            return Err(LaunchError::Invalid(e.message.clone()));
        }
        let mut env = Vec::new();
        if let Some(dir) = &account.config_dir {
            let var = self
                .account_env()
                .ok_or_else(|| LaunchError::SingleAccount(self.m.name.clone()))?;
            env.push((var.to_string(), dir.display().to_string()));
        }
        let model = match self.selection {
            ModelSelection::Automatic => None,
            _ => config.model.clone(),
        };
        Ok(LaunchPlan {
            engine_harness: self.engine_name().to_string(),
            model,
            effort: config
                .effort
                .as_deref()
                .and_then(Effort::parse)
                .filter(|e| self.efforts.contains(e)),
            env,
            worktree: worktree.to_path_buf(),
        })
    }

    fn signals(&self) -> Signals {
        self.signals.clone()
    }

    fn transcript(&self, env: &HostEnv, account: &Account) -> Option<TranscriptSource> {
        let format = self.transcript?;
        let dir = &self.m.transcript.as_ref()?.dir;
        Some(TranscriptSource {
            format,
            root: self.config_dir(env, account)?.join(dir),
        })
    }

    fn keys(&self) -> Keys {
        Keys {
            interrupt: self.m.keys.interrupt.clone(),
            exit_command: self.m.keys.exit.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_maps_cleanly() {
        for m in quark_harness::builtin() {
            let h = ManifestHarness::new(m.clone());
            assert_eq!(h.roles.len(), m.roles.len(), "{}", m.id);
            assert_eq!(h.efforts.len(), m.efforts.len(), "{}", m.id);
            assert_eq!(h.transcript.is_some(), m.transcript.is_some(), "{}", m.id);
            assert!(!h.install_hint().is_empty(), "{}", m.id);
        }
    }

    #[test]
    fn engine_names_are_firstmate_adapters() {
        let names: Vec<_> = all().iter().map(|h| h.engine_name().to_string()).collect();
        assert_eq!(
            names,
            [
                "claude",
                "codex",
                "pi",
                "pi-signed",
                "opencode",
                "cursor",
                "bob",
                "omp",
                "gemini",
                "grok",
                "kimi",
                "rovo",
                "agy",
                "muse",
                "kiro"
            ]
        );
    }
}
