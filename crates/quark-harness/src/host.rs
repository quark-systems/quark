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
    pub keychain: Keychain,
}

/// Where credential checks look for Keychain items.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Keychain {
    /// Nowhere: off macOS, and in tests that build a [`HostEnv`] by hand.
    #[default]
    Off,
    /// The user's login Keychain, through `security`.
    System,
    /// Exactly these generic-password services.
    Items(Vec<String>),
}

impl Keychain {
    /// Whether a generic password for `service` exists. Only the item's
    /// attributes are read, never its secret, so the Keychain never asks.
    pub fn has(&self, service: &str) -> bool {
        match self {
            Keychain::Off => false,
            Keychain::Items(items) => items.iter().any(|s| s == service),
            Keychain::System => std::process::Command::new("/usr/bin/security")
                .args(["find-generic-password", "-s", service])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success()),
        }
    }
}

impl HostEnv {
    /// Snapshot of this process's environment.
    pub fn current() -> Self {
        Self {
            path: std::env::var_os("PATH"),
            home: std::env::var_os("HOME").map(PathBuf::from),
            vars: std::env::vars().collect(),
            keychain: if cfg!(target_os = "macos") {
                Keychain::System
            } else {
                Keychain::Off
            },
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

/// Bin directories of Node version managers and per-user package
/// installs under `home`, for tools a login shell only finds through its
/// interactive setup. nvm puts just its default version on a login PATH, so
/// a CLI installed under another version (IBM Bob under an older Node, say)
/// is missing from a launchd-started quarkd. Newest Node version first;
/// only directories that exist.
pub fn tool_dirs(home: &Path, nvm_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let nvm = nvm_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home.join(".nvm"));
    out.extend(node_versions(&nvm.join("versions/node"), "bin"));
    for fnm in [
        home.join(".local/share/fnm/node-versions"),
        home.join("Library/Application Support/fnm/node-versions"),
    ] {
        out.extend(node_versions(&fnm, "installation/bin"));
    }
    for rel in [
        ".volta/bin",
        ".local/share/mise/shims",
        ".asdf/shims",
        ".npm-global/bin",
        "Library/pnpm",
        ".bun/bin",
        ".local/bin",
    ] {
        out.push(home.join(rel));
    }
    out.retain(|d| d.is_dir());
    out
}

/// `<root>/<version>/<bin>` for every version directory, newest first.
fn node_versions(root: &Path, bin: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut versions: Vec<(Vec<u64>, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let key = name
                .trim_start_matches('v')
                .split('.')
                .map(|p| p.parse().unwrap_or(0))
                .collect();
            (key, e.path().join(bin))
        })
        .collect();
    versions.sort_by(|a, b| b.0.cmp(&a.0));
    versions.into_iter().map(|(_, p)| p).collect()
}

/// `path` with each of `extra` not already on it appended, so what the
/// user's PATH picks first still wins. `None` when nothing was added.
pub fn widen_path(path: Option<&std::ffi::OsStr>, extra: &[PathBuf]) -> Option<OsString> {
    let mut dirs: Vec<PathBuf> = path
        .map(|p| std::env::split_paths(p).collect())
        .unwrap_or_default();
    let before = dirs.len();
    for d in extra {
        if !dirs.contains(d) {
            dirs.push(d.clone());
        }
    }
    if dirs.len() == before {
        return None;
    }
    std::env::join_paths(dirs).ok()
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
            ..HostEnv::default()
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

    #[test]
    fn tool_dirs_add_every_nvm_version_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        for v in ["v22.19.0", "v26.13.1", "v24.14.1", "v9.0.0"] {
            std::fs::create_dir_all(home.join(".nvm/versions/node").join(v).join("bin")).unwrap();
        }
        std::fs::create_dir_all(home.join(".volta/bin")).unwrap();
        let node = |v: &str| home.join(".nvm/versions/node").join(v).join("bin");
        let dirs = tool_dirs(home, None);
        assert_eq!(
            dirs,
            [
                node("v26.13.1"),
                node("v24.14.1"),
                node("v22.19.0"),
                node("v9.0.0"),
                home.join(".volta/bin"),
            ]
        );

        // Already-listed directories keep their place; the rest go last.
        let path = std::env::join_paths(["/usr/bin".into(), node("v26.13.1")]).unwrap();
        let wide = widen_path(Some(&path), &dirs).unwrap();
        let got: Vec<PathBuf> = std::env::split_paths(&wide).collect();
        assert_eq!(
            got[..3],
            [
                PathBuf::from("/usr/bin"),
                node("v26.13.1"),
                node("v24.14.1")
            ]
        );
        assert_eq!(got.len(), 6);
        let all = std::env::join_paths(&got).unwrap();
        assert_eq!(widen_path(Some(&all), &dirs), None);

        // Bob installed only under an older Node is found once widened.
        exe(&node("v22.19.0").join("bob"), "echo bob");
        let reg = crate::ManifestRegistry::builtin();
        let env = HostEnv {
            path: Some(path.clone()),
            ..HostEnv::default()
        };
        assert!(resolve_bin(reg.get("bob").unwrap(), &env).is_none());
        let env = HostEnv {
            path: widen_path(Some(&path), &dirs),
            ..HostEnv::default()
        };
        assert_eq!(
            resolve_bin(reg.get("bob").unwrap(), &env),
            Some(node("v22.19.0").join("bob"))
        );
    }
}
