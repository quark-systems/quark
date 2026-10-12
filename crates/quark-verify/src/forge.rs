//! The forge reads the guard needs (a branch's tip, a commit's checks, and
//! how far a head is behind a base), a pull request's head, and the merge
//! itself.
//!
//! [`GhForge`] reads GitHub through the `gh` CLI, the same reads firstmate
//! makes, so it uses whatever account `gh` is logged in to. [`FakeForge`]
//! answers from memory for tests.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use quark_core::{CoreError, Result};
use serde::{Deserialize, Serialize};
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

    /// Pull request `number`'s head and state.
    async fn pull_request(&self, repo: &str, number: u64) -> Result<PullHead>;

    /// Merge pull request `number` only if its head is still `sha`; the
    /// forge refuses a head that moved since it was verified.
    async fn merge(&self, repo: &str, number: u64, sha: &str, method: MergeMethod) -> Result<()>;
}

/// Where a pull request stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullState {
    Open,
    Closed,
    Merged,
}

/// A pull request's head as the forge reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullHead {
    /// The head commit.
    pub sha: String,
    /// The branch the head is on.
    pub branch: String,
    /// The branch it merges into.
    pub base: String,
    pub state: PullState,
}

/// How a pull request is merged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeMethod {
    /// firstmate's default.
    #[default]
    Squash,
    Merge,
    Rebase,
}

impl MergeMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            MergeMethod::Squash => "squash",
            MergeMethod::Merge => "merge",
            MergeMethod::Rebase => "rebase",
        }
    }
}

/// `owner/repo` and the number of a GitHub pull request URL.
pub fn parse_pull_url(url: &str) -> Result<(String, u64)> {
    let bad = || CoreError::Invalid(format!("pull request {url:?}"));
    let rest = url
        .strip_prefix("https://github.com/")
        .ok_or_else(bad)?
        .trim_end_matches('/');
    let mut parts = rest.split('/');
    let (Some(owner), Some(name), Some("pull"), Some(n), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return Err(bad());
    };
    let repo = format!("{owner}/{name}");
    check_repo(&repo)?;
    let number = n.parse().ok().filter(|n| *n > 0).ok_or_else(bad)?;
    Ok((repo, number))
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
        let out = self.gh(&["api", path], path).await?;
        serde_json::from_slice(&out).map_err(|e| CoreError::Backend(format!("gh api {path}: {e}")))
    }

    /// Run `gh` with `args`: its stdout, or its first stderr line as the
    /// error, prefixed with `gh api <path>`.
    async fn gh(&self, args: &[&str], path: &str) -> Result<Vec<u8>> {
        let mut cmd = Command::new(&self.gh);
        cmd.args(args).kill_on_drop(true);
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
        Ok(out.stdout)
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

#[derive(Deserialize)]
struct PullJson {
    state: String,
    #[serde(default)]
    merged: bool,
    head: RefJson,
    base: RefJson,
}

#[derive(Deserialize)]
struct RefJson {
    #[serde(rename = "ref")]
    branch: String,
    sha: String,
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

    async fn pull_request(&self, repo: &str, number: u64) -> Result<PullHead> {
        check_repo(repo)?;
        let p: PullJson = self.api(&format!("repos/{repo}/pulls/{number}")).await?;
        check_sha(&p.head.sha)?;
        Ok(PullHead {
            sha: p.head.sha,
            branch: p.head.branch,
            base: p.base.branch,
            state: match (p.merged, p.state.as_str()) {
                (true, _) => PullState::Merged,
                (false, "open") => PullState::Open,
                _ => PullState::Closed,
            },
        })
    }

    async fn merge(&self, repo: &str, number: u64, sha: &str, method: MergeMethod) -> Result<()> {
        check_repo(repo)?;
        check_sha(sha)?;
        let path = format!("repos/{repo}/pulls/{number}/merge");
        let sha = format!("sha={sha}");
        let method = format!("merge_method={}", method.as_str());
        let args = ["api", "-X", "PUT", &path, "-f", &sha, "-f", &method];
        self.gh(&args, &path).await.map(drop)
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
    pulls: Mutex<HashMap<(String, u64), PullHead>>,
    merges: Mutex<Vec<(String, u64, String, MergeMethod)>>,
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

    pub fn set_pull(&self, repo: &str, number: u64, pull: PullHead) {
        self.pulls
            .lock()
            .unwrap()
            .insert((repo.into(), number), pull);
    }

    /// Merges made, oldest first: repo, number, head and method.
    pub fn merges(&self) -> Vec<(String, u64, String, MergeMethod)> {
        self.merges.lock().unwrap().clone()
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

    async fn pull_request(&self, repo: &str, number: u64) -> Result<PullHead> {
        self.read();
        self.pulls
            .lock()
            .unwrap()
            .get(&(repo.to_string(), number))
            .cloned()
            .ok_or_else(|| missing(format!("{repo}#{number}")))
    }

    /// Merges an open pull request whose head is `sha`, as GitHub does.
    async fn merge(&self, repo: &str, number: u64, sha: &str, method: MergeMethod) -> Result<()> {
        let mut pulls = self.pulls.lock().unwrap();
        let pull = pulls
            .get_mut(&(repo.to_string(), number))
            .ok_or_else(|| missing(format!("{repo}#{number}")))?;
        if pull.state != PullState::Open || pull.sha != sha {
            return Err(CoreError::Backend(format!(
                "{repo}#{number} is not open at {sha}"
            )));
        }
        pull.state = PullState::Merged;
        self.merges
            .lock()
            .unwrap()
            .push((repo.into(), number, sha.into(), method));
        Ok(())
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

    #[test]
    fn parses_pull_request_urls() {
        for (url, n) in [
            ("https://github.com/acme/app/pull/12", 12),
            ("https://github.com/acme/app/pull/12/", 12),
        ] {
            assert_eq!(parse_pull_url(url).unwrap(), ("acme/app".to_string(), n));
        }
        for bad in [
            "https://gitlab.com/acme/app/pull/1",
            "https://github.com/acme/app/issues/1",
            "https://github.com/acme/app/pull/0",
            "https://github.com/acme/app/pull/x",
            "https://github.com/acme/app/pull/1/files",
            "https://github.com/../app/pull/1",
        ] {
            assert!(parse_pull_url(bad).is_err(), "{bad}");
        }
    }
}
