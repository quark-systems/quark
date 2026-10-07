//! Where a repo's pool lives and how it is configured, resolved as
//! treehouse 3.1.2 resolves them so both find the same pool.
//!
//! Configuration is `treehouse.toml` in the primary checkout, else
//! `~/.config/treehouse/config.toml`. The pool is
//! `<root>/.treehouse/<repo dir name>-<first 3 bytes of sha256(origin url)>`
//! (the repo path stands in when there is no origin); `<root>` is `--root`,
//! else `TREEHOUSE_ROOT`, else `root` from the config, else the home
//! directory.

use std::path::{Path, PathBuf};

use quark_core::{CoreError, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::git;

/// treehouse's `max_trees` default.
pub const DEFAULT_MAX_TREES: usize = 16;

/// The settings the native pool reads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Config {
    pub max_trees: Option<usize>,
    pub root: String,
    pub base_branch: String,
    pub worktree_path: String,
    pub apfs_sharing: String,
    pub vcs: String,
    pub unique_leaf: bool,
    pub hooks: Hooks,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Hooks {
    pub post_create: Vec<String>,
    pub pre_destroy: Vec<String>,
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

fn parse(path: &Path) -> Result<Option<Config>> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text)
            .map(Some)
            .map_err(|e| CoreError::Invalid(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(CoreError::Backend(format!(
            "reading {}: {e}",
            path.display()
        ))),
    }
}

impl Config {
    /// The repo's config, the user's when the repo has none. (Repo files
    /// never carry hooks; the user's hooks apply either way.)
    pub fn load(repo: &Path) -> Result<Self> {
        let user = match home() {
            Some(h) => parse(&h.join(".config/treehouse/config.toml"))?,
            None => None,
        };
        let mut cfg = match parse(&repo.join("treehouse.toml"))? {
            Some(mut c) => {
                c.hooks = user.as_ref().map(|u| u.hooks.clone()).unwrap_or_default();
                c
            }
            None => user.unwrap_or_default(),
        };
        if let Ok(v) = std::env::var("TREEHOUSE_WORKTREE_PATH") {
            if !v.is_empty() {
                cfg.worktree_path = v;
            }
        }
        if let Ok(v) = std::env::var("TREEHOUSE_APFS_SHARING") {
            if !v.is_empty() {
                cfg.apfs_sharing = v;
            }
        }
        if let Ok(v) = std::env::var("TREEHOUSE_UNIQUE_LEAF") {
            if let Some(b) = parse_go_bool(&v) {
                cfg.unique_leaf = b;
            }
        }
        if let Ok(v) = std::env::var("TREEHOUSE_VCS") {
            if !v.is_empty() {
                cfg.vcs = v;
            }
        }
        Ok(cfg)
    }

    pub fn max_trees(&self) -> usize {
        match self.max_trees {
            Some(n) => n,
            None => DEFAULT_MAX_TREES,
        }
    }

    /// What this config asks for that the native pool does not do yet; the
    /// native pool refuses such a repo rather than act differently from
    /// treehouse.
    pub fn unsupported(&self, repo: &Path) -> Option<String> {
        let selects_jj = self.vcs == "jj" && repo.join(".jj").is_dir();
        let reason = if selects_jj {
            "jj workspaces"
        } else if !self.worktree_path.is_empty() {
            "worktree_path templates"
        } else if self.unique_leaf {
            "unique_leaf"
        } else if !matches!(self.apfs_sharing.as_str(), "" | "off") {
            "apfs_sharing"
        } else if !self.hooks.post_create.is_empty() {
            "post_create hooks"
        } else {
            return None;
        };
        Some(format!(
            "the native worktree pool does not support {reason} yet"
        ))
    }
}

fn parse_go_bool(v: &str) -> Option<bool> {
    match v {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Some(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Some(false),
        _ => None,
    }
}

/// `<root>/.treehouse` for an explicit or configured `root`, else
/// `~/.treehouse`.
pub fn pool_root(repo: &Path, root: Option<&Path>, cfg: &Config) -> Result<PathBuf> {
    let root = match root {
        Some(r) => r.to_string_lossy().into_owned(),
        None => match std::env::var("TREEHOUSE_ROOT") {
            Ok(v) if !v.is_empty() => v,
            _ => cfg.root.clone(),
        },
    };
    if root.is_empty() {
        return home()
            .map(|h| h.join(".treehouse"))
            .ok_or_else(|| CoreError::Backend("no home directory for the pool".into()));
    }
    let expanded = PathBuf::from(expand_env(&root));
    let base = if expanded.is_absolute() {
        expanded
    } else {
        repo.join(expanded)
    };
    Ok(base.join(".treehouse"))
}

/// The pool directory of the primary checkout `repo`.
pub fn pool_dir(repo: &Path, root: Option<&Path>, cfg: &Config) -> Result<PathBuf> {
    let hash_input = git::remote_url(repo).unwrap_or_else(|_| repo.to_string_lossy().into_owned());
    let name = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(pool_root(repo, root, cfg)?.join(format!("{name}-{}", short_hash(&hash_input))))
}

/// treehouse's `ShortHash`: the first 3 bytes of SHA-256, hex.
pub fn short_hash(s: &str) -> String {
    Sha256::digest(s.as_bytes())[..3]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `$VAR` and `${VAR}` expansion, as Go's `os.ExpandEnv`.
fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        let name: String = if chars.peek() == Some(&'{') {
            chars.next();
            chars.by_ref().take_while(|c| *c != '}').collect()
        } else {
            let mut n = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_alphanumeric() || c == '_' {
                    n.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            n
        };
        out.push_str(&std::env::var(&name).unwrap_or_default());
    }
    out
}

/// Keep the pool out of `git status`: a self-ignoring `.gitignore` in the
/// pool root, and an `info/exclude` line when the root sits inside a repo.
/// Best effort, as in treehouse.
pub fn ensure_excluded(treehouse_dir: &Path) {
    let self_ignored = std::fs::create_dir_all(treehouse_dir).is_ok() && {
        let gi = treehouse_dir.join(".gitignore");
        gi.exists() || std::fs::write(&gi, "*\n").is_ok()
    };
    let mut check = treehouse_dir.to_path_buf();
    while !check.is_dir() {
        match check.parent() {
            Some(p) => check = p.to_path_buf(),
            None => return,
        }
    }
    let Ok(top) = git::run(&check, &["rev-parse", "--show-toplevel"]) else {
        return;
    };
    let Ok(rel) = treehouse_dir.strip_prefix(&top) else {
        return;
    };
    let entry = format!("/{}", rel.to_string_lossy());
    let has =
        |p: &Path| std::fs::read_to_string(p).is_ok_and(|t| t.lines().any(|l| l.trim() == entry));
    if has(&Path::new(&top).join(".gitignore")) {
        return;
    }
    let Ok(common) = git::common_git_dir(Path::new(&top)) else {
        let _ = self_ignored;
        return;
    };
    let exclude = common.join("info/exclude");
    if has(&exclude) {
        return;
    }
    let existing = std::fs::read_to_string(&exclude).unwrap_or_default();
    let prefix = if !existing.is_empty() && !existing.ends_with('\n') {
        "\n"
    } else {
        ""
    };
    let _ = std::fs::create_dir_all(common.join("info"));
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&exclude)
        .and_then(|mut f| {
            std::io::Write::write_all(&mut f, format!("{prefix}{entry}\n").as_bytes())
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_hash_matches_treehouse() {
        // sha256("git@github.com:quark-systems/quark.git")[..3]
        let h = short_hash("git@github.com:quark-systems/quark.git");
        assert_eq!(h.len(), 6);
        assert_eq!(short_hash("abc"), "ba7816");
    }

    #[test]
    fn config_subset() {
        let c: Config = toml::from_str("max_trees = 4\nbase_branch = \"dev\"\n").unwrap();
        assert_eq!(c.max_trees(), 4);
        assert_eq!(c.base_branch, "dev");
        assert_eq!(Config::default().max_trees(), DEFAULT_MAX_TREES);
        let c: Config = toml::from_str("worktree_path = \"{pool}/{slot}/x\"").unwrap();
        assert!(c.unsupported(Path::new("/r")).is_some());
        assert!(Config::default().unsupported(Path::new("/r")).is_none());
    }

    #[test]
    fn env_expansion() {
        std::env::set_var("QUARK_WT_TEST_ROOT", "/x");
        assert_eq!(
            expand_env("$QUARK_WT_TEST_ROOT/a/${QUARK_WT_TEST_ROOT}"),
            "/x/a//x"
        );
    }
}
