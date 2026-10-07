//! Harness manifests for the native Quark engine.
//!
//! Every agent CLI Quark can run is one TOML file in the
//! [`quark_core::harness`] schema. The fifteen built-in manifests live in
//! `manifests/` and are compiled in; a person can add or replace one by
//! dropping `<id>.toml` into a manifest directory, with no quarkd release.
//!
//! This crate owns parsing and validation ([`parse`], [`load_dir`]), the
//! registry ([`ManifestRegistry`]), and the logic that reads a manifest
//! against a machine: executable detection ([`host`]), account directories
//! and credential health ([`account`]), and the launch command ([`launch`]).

pub mod account;
pub mod host;
pub mod launch;

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use quark_core::harness::HarnessManifest;
use quark_core::{CoreError, HarnessRegistry, Result};

pub use account::{auth_status, config_dir};
pub use host::{probe_version, resolve_bin, HostEnv};
pub use launch::{argv, LaunchVars};

/// The built-in manifests as `(file name, TOML)`, in display order.
pub const BUILTIN_SOURCES: &[(&str, &str)] = &[
    (
        "claude-code.toml",
        include_str!("../manifests/claude-code.toml"),
    ),
    ("codex.toml", include_str!("../manifests/codex.toml")),
    ("pi.toml", include_str!("../manifests/pi.toml")),
    (
        "pi-signed.toml",
        include_str!("../manifests/pi-signed.toml"),
    ),
    ("opencode.toml", include_str!("../manifests/opencode.toml")),
    (
        "cursor-agent.toml",
        include_str!("../manifests/cursor-agent.toml"),
    ),
    ("bob.toml", include_str!("../manifests/bob.toml")),
    ("omp.toml", include_str!("../manifests/omp.toml")),
    ("gemini.toml", include_str!("../manifests/gemini.toml")),
    ("grok.toml", include_str!("../manifests/grok.toml")),
    ("kimi.toml", include_str!("../manifests/kimi.toml")),
    ("rovo.toml", include_str!("../manifests/rovo.toml")),
    (
        "antigravity.toml",
        include_str!("../manifests/antigravity.toml"),
    ),
    ("muse.toml", include_str!("../manifests/muse.toml")),
    ("kiro.toml", include_str!("../manifests/kiro.toml")),
];

static BUILTIN: LazyLock<Vec<Arc<HarnessManifest>>> = LazyLock::new(|| {
    BUILTIN_SOURCES
        .iter()
        .map(|(file, src)| {
            Arc::new(parse(src).unwrap_or_else(|e| panic!("built-in manifest {file}: {e}")))
        })
        .collect()
});

/// The built-in manifests, parsed and validated once, in display order.
pub fn builtin() -> &'static [Arc<HarnessManifest>] {
    &BUILTIN
}

/// Parses and validates one manifest.
pub fn parse(src: &str) -> Result<HarnessManifest> {
    let m: HarnessManifest =
        toml::from_str(src).map_err(|e| CoreError::Invalid(e.message().to_string()))?;
    m.validate()?;
    Ok(m)
}

/// The name an engine reports for `m`: its first alias, else its id.
/// firstmate's adapter names are the first alias where they differ.
pub fn engine_name(m: &HarnessManifest) -> &str {
    m.aliases.first().map_or(m.id.as_str(), String::as_str)
}

/// A manifest file that did not load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadError {
    pub path: PathBuf,
    pub error: String,
}

/// Every `*.toml` in `dir`, sorted by file name. A missing directory is
/// empty; a file that fails to read, parse or validate is reported and
/// skipped, and so is one whose id does not match its file name.
pub fn load_dir(dir: &Path) -> (Vec<HarnessManifest>, Vec<LoadError>) {
    let mut ok = Vec::new();
    let mut errors = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (ok, errors);
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml") && p.is_file())
        .collect();
    paths.sort();
    for path in paths {
        let loaded = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|src| parse(&src).map_err(|e| e.to_string()))
            .and_then(|m| {
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                if m.id == stem {
                    Ok(m)
                } else {
                    Err(format!("id `{}` does not match the file name", m.id))
                }
            });
        match loaded {
            Ok(m) => ok.push(m),
            Err(error) => errors.push(LoadError { path, error }),
        }
    }
    (ok, errors)
}

/// The manifests an engine runs with: the built-ins, then each extra
/// manifest replacing the built-in with its id or joining at the end.
#[derive(Debug, Clone)]
pub struct ManifestRegistry {
    manifests: Vec<Arc<HarnessManifest>>,
}

impl Default for ManifestRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

impl ManifestRegistry {
    /// The built-in manifests alone.
    pub fn builtin() -> Self {
        Self {
            manifests: builtin().to_vec(),
        }
    }

    /// The built-ins overlaid with `extra`.
    pub fn with(extra: impl IntoIterator<Item = HarnessManifest>) -> Self {
        let mut reg = Self::builtin();
        for m in extra {
            let m = Arc::new(m);
            match reg.manifests.iter_mut().find(|e| e.id == m.id) {
                Some(slot) => *slot = m,
                None => reg.manifests.push(m),
            }
        }
        reg
    }

    /// The built-ins overlaid with the manifests in `dir`, plus the files
    /// that failed to load.
    pub fn load(dir: &Path) -> (Self, Vec<LoadError>) {
        let (extra, errors) = load_dir(dir);
        (Self::with(extra), errors)
    }

    pub fn all(&self) -> &[Arc<HarnessManifest>] {
        &self.manifests
    }

    pub fn get(&self, id: &str) -> Option<&Arc<HarnessManifest>> {
        self.manifests.iter().find(|m| m.id == id)
    }

    /// The manifest an engine reports by id or alias.
    pub fn resolve(&self, name: &str) -> Option<&Arc<HarnessManifest>> {
        self.get(name).or_else(|| {
            self.manifests
                .iter()
                .find(|m| m.aliases.iter().any(|a| a == name))
        })
    }
}

#[async_trait]
impl HarnessRegistry for ManifestRegistry {
    async fn list(&self) -> Result<Vec<HarnessManifest>> {
        Ok(self.manifests.iter().map(|m| (**m).clone()).collect())
    }

    async fn get(&self, id: &str) -> Result<HarnessManifest> {
        ManifestRegistry::get(self, id)
            .map(|m| (**m).clone())
            .ok_or_else(|| CoreError::NotFound(format!("harness {id}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const IDS: [&str; 15] = [
        "claude-code",
        "codex",
        "pi",
        "pi-signed",
        "opencode",
        "cursor-agent",
        "bob",
        "omp",
        "gemini",
        "grok",
        "kimi",
        "rovo",
        "antigravity",
        "muse",
        "kiro",
    ];

    #[test]
    fn builtins_parse_and_cover_every_harness() {
        let ids: Vec<_> = builtin().iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, IDS);
        for ((file, _), m) in BUILTIN_SOURCES.iter().zip(builtin()) {
            assert_eq!(*file, format!("{}.toml", m.id));
        }
    }

    #[test]
    fn names_and_aliases_are_unique() {
        let mut seen = HashSet::new();
        for m in builtin() {
            assert!(seen.insert(m.id.clone()), "{}", m.id);
            for a in &m.aliases {
                assert!(seen.insert(a.clone()), "{a}");
            }
        }
        let engines: HashSet<_> = builtin().iter().map(|m| engine_name(m)).collect();
        assert_eq!(engines.len(), IDS.len());
    }

    #[test]
    fn every_harness_can_interrupt_and_exit() {
        for m in builtin() {
            assert!(!m.keys.interrupt.is_empty(), "{}", m.id);
            assert!(m.keys.exit.starts_with('/'), "{}", m.id);
        }
    }

    #[test]
    fn coordinators_are_the_pi_family_claude_and_codex() {
        let c: Vec<_> = builtin()
            .iter()
            .filter(|m| m.roles.iter().any(|r| r == "coordinator"))
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(c, ["claude-code", "codex", "pi", "pi-signed"]);
    }

    #[test]
    fn resolves_aliases() {
        let reg = ManifestRegistry::builtin();
        assert_eq!(reg.resolve("claude").unwrap().id, "claude-code");
        assert_eq!(reg.resolve("agy").unwrap().id, "antigravity");
        assert_eq!(reg.resolve("codex").unwrap().id, "codex");
        assert!(reg.resolve("nope").is_none());
    }

    #[test]
    fn a_directory_adds_and_replaces_manifests() {
        let dir = tempfile::tempdir().unwrap();
        let claude = BUILTIN_SOURCES[0]
            .1
            .replace("name = \"Claude Code\"", "name = \"Mine\"");
        std::fs::write(dir.path().join("claude-code.toml"), claude).unwrap();
        let extra = BUILTIN_SOURCES[0]
            .1
            .replace("id = \"claude-code\"", "id = \"acme\"")
            .replace("aliases = [\"claude\"]", "aliases = []");
        std::fs::write(dir.path().join("acme.toml"), extra.clone()).unwrap();
        std::fs::write(dir.path().join("wrong.toml"), extra).unwrap();
        std::fs::write(dir.path().join("broken.toml"), "schema = 1\nbogus = 2\n").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "ignored").unwrap();

        let (reg, errors) = ManifestRegistry::load(dir.path());
        assert_eq!(reg.get("claude-code").unwrap().name, "Mine");
        assert_eq!(reg.all().len(), IDS.len() + 1);
        assert_eq!(reg.all().last().unwrap().id, "acme");
        let bad: Vec<_> = errors
            .iter()
            .map(|e| e.path.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(bad, ["broken.toml", "wrong.toml"]);

        let (none, errors) = ManifestRegistry::load(&dir.path().join("missing"));
        assert_eq!(none.all().len(), IDS.len());
        assert!(errors.is_empty());
    }

    #[tokio::test]
    async fn implements_the_core_registry() {
        let reg = ManifestRegistry::builtin();
        let r: &dyn HarnessRegistry = &reg;
        assert_eq!(r.list().await.unwrap().len(), IDS.len());
        assert_eq!(r.get("kiro").await.unwrap().name, "Kiro CLI");
        assert!(matches!(r.get("x").await, Err(CoreError::NotFound(_))));
    }
}
