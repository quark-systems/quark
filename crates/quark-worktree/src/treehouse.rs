//! The worktree pool, and treehouse as its first implementation.
//!
//! [`Pool`] is the narrow surface [`crate::TreehouseProvider`] needs from a
//! pooled-worktree tool. [`TreehouseCli`] drives treehouse 3.1.2 through its
//! non-interactive commands only:
//!
//! | Pool call | treehouse command |
//! |---|---|
//! | [`Pool::acquire`] | `treehouse get --lease --json --lease-holder <h> [-b <branch>] [--base <base>]` |
//! | [`Pool::release`] | `treehouse return --if-lease-id <id> <path>` (never `--force`) |
//! | [`Pool::list`] | `treehouse status --json` |
//!
//! Every command runs in the repo's primary checkout with stdin closed, so a
//! prompt can never block it; treehouse's "not returned" exit (3) comes back
//! as [`Release::NotReturned`]. The native pool of slice 9 implements
//! [`Pool`] too and is shadowed against this one.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use quark_core::{CoreError, Result};
use serde::Deserialize;
use tokio::process::Command;

/// The treehouse release this adapter is written against.
pub const TREEHOUSE_VERSION: &str = "3.1.2";

/// treehouse's exit status for "left exactly as it was found".
const EXIT_NOT_RETURNED: i32 = 3;

/// One acquisition, as `treehouse get --lease --json` prints it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeaseInfo {
    pub path: PathBuf,
    /// Fresh per acquisition; used as the [`quark_core::worktree::Worktree`] id.
    pub lease_id: String,
    #[serde(default)]
    pub lease_holder: String,
    #[serde(default)]
    pub base_branch: String,
}

/// One pool slot, as `treehouse status --json` prints it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PoolEntry {
    #[serde(default)]
    pub name: String,
    pub path: PathBuf,
    /// `available`, `in-use`, `dirty`, `leased`, `you're here`, `damaged` or
    /// `unverified`.
    pub status: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub detached: bool,
    /// Set when treehouse rebuilt the entry after losing its state and holds
    /// it until someone inspects it.
    #[serde(default)]
    pub recovery_reason: String,
    #[serde(default)]
    pub lease_id: String,
    #[serde(default)]
    pub lease_holder: String,
}

/// How a release went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Release {
    Returned,
    /// The pool left the worktree and its lease as they were, with its reason.
    NotReturned(String),
}

/// A pool of reusable worktrees with durable leases.
#[async_trait]
pub trait Pool: Send + Sync {
    /// Lease a worktree of the primary checkout `repo` to `holder`, on a new
    /// `branch` when given (detached otherwise), cut from `base` when given.
    async fn acquire(
        &self,
        repo: &Path,
        holder: &str,
        branch: Option<&str>,
        base: Option<&str>,
    ) -> Result<LeaseInfo>;

    /// Release the worktree at `path` only while it still carries `lease_id`.
    /// Must never discard uncommitted work.
    async fn release(&self, repo: &Path, path: &Path, lease_id: &str) -> Result<Release>;

    /// Every slot in `repo`'s pool.
    async fn list(&self, repo: &Path) -> Result<Vec<PoolEntry>>;
}

/// [`Pool`] backed by the `treehouse` command.
#[derive(Debug, Clone)]
pub struct TreehouseCli {
    bin: PathBuf,
    root: Option<PathBuf>,
}

impl Default for TreehouseCli {
    fn default() -> Self {
        Self::new()
    }
}

impl TreehouseCli {
    /// `treehouse` from `PATH`, with its configured pool root.
    pub fn new() -> Self {
        Self {
            bin: PathBuf::from("treehouse"),
            root: None,
        }
    }

    /// Use this treehouse binary.
    pub fn with_bin(mut self, bin: impl Into<PathBuf>) -> Self {
        self.bin = bin.into();
        self
    }

    /// Keep pools under `root` (treehouse's `--root`) instead of its default.
    pub fn with_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.root = Some(root.into());
        self
    }

    /// The installed version, refused unless it is 3.x at or above
    /// [`TREEHOUSE_VERSION`].
    pub async fn check_version(&self) -> Result<String> {
        let out = self.command(Path::new("."), ["--version"]).output().await;
        let out = out.map_err(|e| self.spawn_error(e))?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let found = parse_version(&text).ok_or_else(|| {
            CoreError::Unsupported(format!("cannot read treehouse version from {text:?}"))
        })?;
        let want = parse_version(TREEHOUSE_VERSION).expect("constant parses");
        if found.0 != want.0 || found < want {
            return Err(CoreError::Unsupported(format!(
                "treehouse {}.{}.{} is installed; {TREEHOUSE_VERSION} or a later 3.x is required",
                found.0, found.1, found.2
            )));
        }
        Ok(format!("{}.{}.{}", found.0, found.1, found.2))
    }

    fn command<I, S>(&self, dir: &Path, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let mut cmd = Command::new(&self.bin);
        cmd.current_dir(dir)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("NO_COLOR", "1")
            // A lease holder from the environment would override ours.
            .env_remove("TREEHOUSE_LEASE_HOLDER")
            .env_remove("TREEHOUSE_DIR");
        if let Some(root) = &self.root {
            cmd.arg("--root").arg(root);
        }
        cmd
    }

    fn spawn_error(&self, e: std::io::Error) -> CoreError {
        CoreError::Backend(format!("running {}: {e}", self.bin.display()))
    }

    async fn run(&self, dir: &Path, args: Vec<String>) -> Result<std::process::Output> {
        self.command(dir, &args)
            .output()
            .await
            .map_err(|e| self.spawn_error(e))
    }
}

/// argv after `treehouse` for an acquisition.
pub fn acquire_args(holder: &str, branch: Option<&str>, base: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "get".to_string(),
        "--lease".into(),
        "--json".into(),
        "--lease-holder".into(),
        holder.into(),
    ];
    if let Some(b) = branch {
        args.extend(["--branch".into(), b.into()]);
    }
    if let Some(b) = base {
        args.extend(["--base".into(), b.into()]);
    }
    args
}

/// argv after `treehouse` for a release.
pub fn release_args(path: &Path, lease_id: &str) -> Vec<String> {
    vec![
        "return".into(),
        "--if-lease-id".into(),
        lease_id.into(),
        path.to_string_lossy().into_owned(),
    ]
}

fn failure(what: &str, out: &std::process::Output) -> CoreError {
    CoreError::Backend(format!(
        "treehouse {what} failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

/// Parse `treehouse get --lease --json` stdout.
pub fn parse_lease(stdout: &str) -> Result<LeaseInfo> {
    let info: LeaseInfo = serde_json::from_str(stdout.trim())
        .map_err(|e| CoreError::Backend(format!("treehouse get --json output: {e}")))?;
    if info.lease_id.is_empty() || info.path.as_os_str().is_empty() {
        return Err(CoreError::Backend(
            "treehouse get --json reported no path or lease id".into(),
        ));
    }
    Ok(info)
}

/// Parse `treehouse status --json` stdout.
pub fn parse_status(stdout: &str) -> Result<Vec<PoolEntry>> {
    serde_json::from_str(stdout.trim())
        .map_err(|e| CoreError::Backend(format!("treehouse status --json output: {e}")))
}

/// The first `x.y.z` in `text`.
fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .find_map(|word| {
            let mut parts = word.split('.').map(|p| p.parse::<u32>().ok());
            match (parts.next(), parts.next(), parts.next()) {
                (Some(Some(a)), Some(Some(b)), Some(Some(c))) => Some((a, b, c)),
                _ => None,
            }
        })
}

#[async_trait]
impl Pool for TreehouseCli {
    async fn acquire(
        &self,
        repo: &Path,
        holder: &str,
        branch: Option<&str>,
        base: Option<&str>,
    ) -> Result<LeaseInfo> {
        let out = self.run(repo, acquire_args(holder, branch, base)).await?;
        if !out.status.success() {
            return Err(failure("get", &out));
        }
        parse_lease(&String::from_utf8_lossy(&out.stdout))
    }

    async fn release(&self, repo: &Path, path: &Path, lease_id: &str) -> Result<Release> {
        let out = self.run(repo, release_args(path, lease_id)).await?;
        match out.status.code() {
            Some(0) => Ok(Release::Returned),
            Some(EXIT_NOT_RETURNED) => Ok(Release::NotReturned(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            )),
            _ => Err(failure("return", &out)),
        }
    }

    async fn list(&self, repo: &Path) -> Result<Vec<PoolEntry>> {
        let out = self
            .run(repo, vec!["status".into(), "--json".into()])
            .await?;
        if !out.status.success() {
            return Err(failure("status", &out));
        }
        parse_status(&String::from_utf8_lossy(&out.stdout))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_argv() {
        assert_eq!(
            acquire_args("quark:task:p:t", Some("feat"), Some("dev")),
            [
                "get",
                "--lease",
                "--json",
                "--lease-holder",
                "quark:task:p:t",
                "--branch",
                "feat",
                "--base",
                "dev"
            ]
        );
        assert_eq!(acquire_args("h", None, None).len(), 5);
    }

    #[test]
    fn release_argv_never_forces() {
        let args = release_args(Path::new("/pool/1/repo"), "L1");
        assert_eq!(args, ["return", "--if-lease-id", "L1", "/pool/1/repo"]);
        assert!(!args.iter().any(|a| a == "--force"));
    }

    #[test]
    fn parses_lease_json() {
        let info = parse_lease(
            r#"{"path":"/h/.treehouse/repo-ab12/1/repo","lease_id":"8f2c","lease_holder":"quark:task:p:t","leased_at":"2026-10-07T01:00:00Z","base_branch":"main"}
"#,
        )
        .unwrap();
        assert_eq!(info.path, Path::new("/h/.treehouse/repo-ab12/1/repo"));
        assert_eq!(info.lease_id, "8f2c");
        assert_eq!(info.base_branch, "main");
        assert!(parse_lease(r#"{"path":"/x","lease_id":""}"#).is_err());
    }

    #[test]
    fn parses_status_json() {
        let entries = parse_status(
            r#"[{"name":"1","path":"/p/1/repo","status":"leased","branch":"feat","lease_id":"L1","lease_holder":"quark:task:p:t","leased_at":"2026-10-07T01:00:00Z","processes":[]},
               {"name":"2","path":"/p/2/repo","status":"available","branch":"","detached":true,"lease_id":"","lease_holder":"","leased_at":null,"processes":[{"pid":7,"name":"zsh"}]},
               {"name":"3","path":"/p/3/repo","status":"leased","branch":"","recovery_reason":"state file lost","lease_id":"L3","lease_holder":"treehouse:recovered","leased_at":null,"processes":[]}]"#,
        )
        .unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].lease_id, "L1");
        assert!(entries[1].detached);
        assert_eq!(entries[2].recovery_reason, "state file lost");
        assert!(parse_status("[]").unwrap().is_empty());
    }

    #[test]
    fn versions() {
        assert_eq!(parse_version("treehouse version 3.1.2\n"), Some((3, 1, 2)));
        assert_eq!(parse_version("treehouse version v3.10.0"), Some((3, 10, 0)));
        assert_eq!(parse_version("dev"), None);
    }

    #[tokio::test]
    async fn missing_binary_is_a_backend_error() {
        let cli = TreehouseCli::new().with_bin("/nonexistent/treehouse");
        assert!(matches!(
            cli.list(Path::new(".")).await,
            Err(CoreError::Backend(_))
        ));
    }
}
