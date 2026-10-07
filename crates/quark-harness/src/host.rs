//! Finding a harness's executable on a machine.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use quark_core::harness::HarnessManifest;

/// Longest a version probe may run.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

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

/// A regular file with an execute bit.
pub fn is_executable(p: &Path) -> bool {
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

/// The harness executable: the first of `detect.bins` on `PATH`, else the
/// first executable `detect.fallback_bins` under the home directory.
pub fn resolve_bin(m: &HarnessManifest, env: &HostEnv) -> Option<PathBuf> {
    m.detect.bins.iter().find_map(|b| env.which(b)).or_else(|| {
        m.detect
            .fallback_bins
            .iter()
            .filter_map(|rel| env.home_join(rel))
            .find(|p| is_executable(p))
    })
}

/// Runs `<exe> <args>` with a timeout and returns the first non-empty line
/// of stdout, or of stderr when stdout is empty. `None` when `args` is
/// empty, the probe fails or it runs past [`PROBE_TIMEOUT`].
pub async fn probe_version(exe: &Path, args: &[String]) -> Option<String> {
    if args.is_empty() {
        return None;
    }
    let run = tokio::process::Command::new(exe)
        .args(args)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn exe(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[tokio::test]
    async fn resolves_path_then_home_fallback_and_probes() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        let home = dir.path().join("home");
        exe(&bin.join("agent"), "echo '1.2.3'");
        exe(&home.join(".kimi-code/bin/kimi"), "echo 'kimi 2' >&2");
        let env = HostEnv {
            path: Some(bin.clone().into_os_string()),
            home: Some(home.clone()),
            vars: HashMap::new(),
        };
        let reg = crate::ManifestRegistry::builtin();

        let cursor = reg.get("cursor-agent").unwrap();
        let found = resolve_bin(cursor, &env).unwrap();
        assert_eq!(found, bin.join("agent"));
        let v = probe_version(&found, &cursor.detect.version_args).await;
        assert_eq!(v.as_deref(), Some("1.2.3"));

        let kimi = reg.get("kimi").unwrap();
        let found = resolve_bin(kimi, &env).unwrap();
        assert_eq!(found, home.join(".kimi-code/bin/kimi"));
        assert_eq!(
            probe_version(&found, &kimi.detect.version_args)
                .await
                .as_deref(),
            Some("kimi 2")
        );
        assert_eq!(probe_version(&found, &[]).await, None);

        assert!(resolve_bin(reg.get("bob").unwrap(), &env).is_none());
    }
}
