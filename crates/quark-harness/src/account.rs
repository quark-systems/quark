//! Account config directories and credential health.

use std::path::{Path, PathBuf};

use quark_core::harness::HarnessManifest;
use quark_systems::{AuthState, HarnessAuth};

use crate::HostEnv;

/// The config directory for an account: `explicit` when given, else the
/// account variable when it is set in `env`, else the manifest's home
/// relative default. `None` for a harness without an `[account]` section.
pub fn config_dir(m: &HarnessManifest, env: &HostEnv, explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = explicit {
        return Some(dir.to_path_buf());
    }
    let account = m.account.as_ref()?;
    if let Some(dir) = account.env.as_deref().and_then(|v| env.var(v)) {
        return Some(PathBuf::from(dir));
    }
    env.home_join(&account.config_dir)
}

/// Whether the account has credentials. An explicit account is checked by
/// its directory alone; an ambient API key belongs to the default account.
/// Finding nothing is `NotConfigured` only when the manifest says its
/// sources are exhaustive, otherwise `Unknown`.
pub fn auth_status(m: &HarnessManifest, env: &HostEnv, explicit: Option<&Path>) -> HarnessAuth {
    let (auth_env, auth_files, exhaustive) = match &m.account {
        Some(a) => (&a.auth_env[..], &a.auth_files[..], a.auth_exhaustive),
        None => (&[][..], &[][..], false),
    };
    if explicit.is_none() {
        if let Some(var) = auth_env.iter().find(|v| env.var(v).is_some()) {
            return HarnessAuth {
                state: AuthState::Configured,
                detail: format!("{var} is set"),
            };
        }
    }
    let dir = config_dir(m, env, explicit);
    if let Some(dir) = &dir {
        if let Some(file) = auth_files.iter().map(|f| dir.join(f)).find(|p| p.is_file()) {
            return HarnessAuth {
                state: AuthState::Configured,
                detail: format!("found {}", file.display()),
            };
        }
    }
    let mut checked: Vec<String> = auth_env.to_vec();
    if let Some(dir) = &dir {
        checked.extend(auth_files.iter().map(|f| dir.join(f).display().to_string()));
    }
    if checked.is_empty() {
        return HarnessAuth {
            state: AuthState::Unknown,
            detail: format!("{} has no credential Quark can check", m.name),
        };
    }
    HarnessAuth {
        state: if exhaustive {
            AuthState::NotConfigured
        } else {
            AuthState::Unknown
        },
        detail: format!("none of {} found", checked.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ManifestRegistry;

    #[test]
    fn config_dirs_and_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let mut env = HostEnv {
            home: Some(home.clone()),
            ..HostEnv::default()
        };
        let reg = ManifestRegistry::builtin();
        let codex = reg.get("codex").unwrap();

        assert_eq!(config_dir(codex, &env, None), Some(home.join(".codex")));
        assert_eq!(
            auth_status(codex, &env, None).state,
            AuthState::NotConfigured
        );
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(home.join(".codex/auth.json"), "{}").unwrap();
        assert_eq!(auth_status(codex, &env, None).state, AuthState::Configured);

        let other = home.join("work");
        assert_eq!(
            auth_status(codex, &env, Some(&other)).state,
            AuthState::NotConfigured
        );
        env.vars.insert("OPENAI_API_KEY".into(), "k".into());
        assert_eq!(
            auth_status(codex, &env, None).detail,
            "OPENAI_API_KEY is set"
        );
        // An ambient key does not log in an explicit account.
        assert_eq!(
            auth_status(codex, &env, Some(&other)).state,
            AuthState::NotConfigured
        );
        env.vars.insert("CODEX_HOME".into(), "/elsewhere".into());
        assert_eq!(config_dir(codex, &env, None), Some("/elsewhere".into()));

        let claude = reg.get("claude-code").unwrap();
        assert_eq!(auth_status(claude, &env, None).state, AuthState::Unknown);
        let rovo = reg.get("rovo").unwrap();
        assert_eq!(config_dir(rovo, &env, None), None);
        assert_eq!(auth_status(rovo, &env, None).state, AuthState::Unknown);
    }
}
