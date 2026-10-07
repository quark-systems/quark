//! The forge reads the guard needs: a branch's tip, a commit's checks, and
//! how far a head is behind a base.
//!
//! [`GhForge`] reads GitHub through the `gh` CLI, the same reads firstmate
//! makes, so it uses whatever account `gh` is logged in to. [`FakeForge`]
//! answers from memory for tests.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use quark_core::{CoreError, Result};
use serde::Deserialize;
use tokio::process::Command;

use crate::health::{CheckRun, CommitStatus};

/// A commit's check runs and statuses.
pub type Checks = (Vec<CheckRun>, Vec<CommitStatus>);

/// GitHub reads, by `owner/repo`.
#[async_trait]
pub trait Forge: Send + Sync {
    /// The tip commit of `branch`.
    async fn branch_tip(&self, repo: &str, branch: &str) -> Result<String>;

    /// The check runs and commit statuses on `sha`.
    async fn checks(&self, repo: &str, sha: &str) -> Result<(Vec<CheckRun>, Vec<CommitStatus>)>;

    /// How many commits of `base` the commit `head` lacks; 0 means `head`
    /// contains `base`.
    async fn behind_by(&self, repo: &str, base: &str, head: &str) -> Result<u64>;
}

const GH_TIMEOUT: Duration = Duration::from_secs(30);

/// GitHub through `gh api`.
#[derive(Debug, Clone)]
pub struct GhForge {
    gh: String,
}

impl Default for GhForge {
    fn default() -> Self {
        Self { gh: "gh".into() }
    }
}

impl GhForge {
    pub fn new(gh: impl Into<String>) -> Self {
        Self { gh: gh.into() }
    }

    async fn api<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let mut cmd = Command::new(&self.gh);
        cmd.args(["api", path]).kill_on_drop(true);
        let out = tokio::time::timeout(GH_TIMEOUT, cmd.output())
            .await
            .map_err(|_| CoreError::Backend(format!("gh api {path}: timed out")))?
            .map_err(|e| CoreError::Backend(format!("gh: {e}")))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(CoreError::Backend(format!(
                "gh api {path}: {}",
                err.lines().next().unwrap_or("failed")
            )));
        }
        serde_json::from_slice(&out.stdout)
            .map_err(|e| CoreError::Backend(format!("gh api {path}: {e}")))
    }
}

#[derive(Deserialize)]
struct CommitJson {
    sha: String,
}

#[derive(Deserialize)]
struct RunsJson {
    check_runs: Vec<CheckRun>,
}

#[derive(Deserialize)]
struct StatusJson {
    #[serde(default)]
    statuses: Option<Vec<CommitStatus>>,
}

#[derive(Deserialize)]
struct CompareJson {
    behind_by: u64,
}

#[async_trait]
impl Forge for GhForge {
    async fn branch_tip(&self, repo: &str, branch: &str) -> Result<String> {
        check_repo(repo)?;
        let c: CommitJson = self
            .api(&format!("repos/{repo}/commits/{}", urlencode(branch)))
            .await?;
        check_sha(&c.sha)?;
        Ok(c.sha)
    }

    async fn checks(&self, repo: &str, sha: &str) -> Result<(Vec<CheckRun>, Vec<CommitStatus>)> {
        check_repo(repo)?;
        check_sha(sha)?;
        let runs: RunsJson = self
            .api(&format!(
                "repos/{repo}/commits/{sha}/check-runs?per_page=100"
            ))
            .await?;
        let status: StatusJson = self
            .api(&format!("repos/{repo}/commits/{sha}/status"))
            .await?;
        Ok((runs.check_runs, status.statuses.unwrap_or_default()))
    }

    async fn behind_by(&self, repo: &str, base: &str, head: &str) -> Result<u64> {
        check_repo(repo)?;
        check_sha(base)?;
        check_sha(head)?;
        if base == head {
            return Ok(0);
        }
        let c: CompareJson = self
            .api(&format!("repos/{repo}/compare/{base}...{head}"))
            .await?;
        Ok(c.behind_by)
    }
}

/// `owner/repo` with GitHub's safe characters only.
pub fn check_repo(repo: &str) -> Result<()> {
    let ok = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && !s.starts_with('-')
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    match repo.split_once('/') {
        Some((owner, name)) if ok(owner) && ok(name) => Ok(()),
        _ => Err(CoreError::Invalid(format!("repository {repo:?}"))),
    }
}

fn check_sha(sha: &str) -> Result<()> {
    if sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(CoreError::Invalid(format!("commit {sha:?}")))
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Answers from memory. A read with no answer fails as a backend error, the
/// way an unreachable forge does.
#[derive(Debug, Default)]
pub struct FakeForge {
    tips: Mutex<HashMap<(String, String), String>>,
    checks: Mutex<HashMap<String, Checks>>,
    behind: Mutex<HashMap<(String, String), u64>>,
    reads: Mutex<usize>,
}

impl FakeForge {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_tip(&self, repo: &str, branch: &str, sha: &str) {
        self.tips
            .lock()
            .unwrap()
            .insert((repo.into(), branch.into()), sha.into());
    }

    pub fn set_checks(&self, sha: &str, runs: Vec<CheckRun>, statuses: Vec<CommitStatus>) {
        self.checks
            .lock()
            .unwrap()
            .insert(sha.into(), (runs, statuses));
    }

    pub fn set_behind(&self, base: &str, head: &str, n: u64) {
        self.behind
            .lock()
            .unwrap()
            .insert((base.into(), head.into()), n);
    }

    /// How many reads were made.
    pub fn reads(&self) -> usize {
        *self.reads.lock().unwrap()
    }

    fn read(&self) {
        *self.reads.lock().unwrap() += 1;
    }
}

fn missing(what: String) -> CoreError {
    CoreError::Backend(format!("fake forge has no answer for {what}"))
}

#[async_trait]
impl Forge for FakeForge {
    async fn branch_tip(&self, repo: &str, branch: &str) -> Result<String> {
        self.read();
        self.tips
            .lock()
            .unwrap()
            .get(&(repo.to_string(), branch.to_string()))
            .cloned()
            .ok_or_else(|| missing(format!("{repo} {branch}")))
    }

    async fn checks(&self, _repo: &str, sha: &str) -> Result<(Vec<CheckRun>, Vec<CommitStatus>)> {
        self.read();
        self.checks
            .lock()
            .unwrap()
            .get(sha)
            .cloned()
            .ok_or_else(|| missing(format!("checks on {sha}")))
    }

    async fn behind_by(&self, _repo: &str, base: &str, head: &str) -> Result<u64> {
        self.read();
        if base == head {
            return Ok(0);
        }
        self.behind
            .lock()
            .unwrap()
            .get(&(base.to_string(), head.to_string()))
            .copied()
            .ok_or_else(|| missing(format!("{base}...{head}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_inputs() {
        assert!(check_repo("acme/app").is_ok());
        assert!(check_repo("acme/app.rs").is_ok());
        for bad in ["acme", "acme/app/x", "-x/app", "acme/..", "a b/c", "/app"] {
            assert!(check_repo(bad).is_err(), "{bad}");
        }
        assert!(check_sha(&"a".repeat(40)).is_ok());
        assert!(check_sha("abc").is_err());
        assert_eq!(urlencode("release/1.0"), "release%2F1.0");
    }
}
