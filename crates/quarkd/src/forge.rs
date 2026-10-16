//! Reads pull requests from their forge.
//!
//! The engine records which pull request a task opened; the forge has
//! everything else (state, checks, reviews, diff). [`GhForge`] reads GitHub
//! through the `gh` CLI, so it uses the account `gh` is logged in to and
//! needs no token of its own. [`StubForge`] serves canned answers for tests
//! and for running the daemon without `gh`.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use quark_systems::{
    Check, CheckStatus, ChecksState, DiffSide, ForgeRepository, Mergeability, PrComment,
    PrCommentKind, PrFileSide, PullRequestState, Review, ReviewDecision, ReviewState,
};
use serde::Deserialize;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// Largest diff returned, in bytes; the rest is cut and flagged.
pub const MAX_PATCH_BYTES: usize = 2 * 1024 * 1024;

const GH_TIMEOUT: Duration = Duration::from_secs(30);

/// Fields read with `gh pr view --json`.
const GH_FIELDS: &str =
    "number,title,body,state,isDraft,author,headRefName,baseRefName,headRefOid,\
mergeable,reviewDecision,additions,deletions,changedFiles,createdAt,updatedAt,mergedAt,\
closedAt,statusCheckRollup,reviews";

/// Which pull request a URL names.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PrRef {
    /// `github` or `gitlab`.
    pub provider: &'static str,
    pub host: String,
    /// Repository path, e.g. `owner/repo`.
    pub repo: String,
    pub number: u64,
}

impl PrRef {
    /// Parses a GitHub pull request URL (`https://host/owner/repo/pull/N`) or
    /// a GitLab merge request URL (`https://host/group/repo/-/merge_requests/N`).
    pub fn parse(url: &str) -> Option<PrRef> {
        let rest = url.strip_prefix("https://")?;
        let rest = rest.split(['?', '#']).next()?.trim_end_matches('/');
        let (host, path) = rest.split_once('/')?;
        if host.is_empty() || path.split('/').any(str::is_empty) {
            return None;
        }
        let number = |n: &str| n.parse::<u64>().ok().filter(|n| *n > 0);
        if let Some((repo, n)) = path.rsplit_once("/-/merge_requests/") {
            return Some(PrRef {
                provider: "gitlab",
                host: host.into(),
                repo: repo.into(),
                number: number(n)?,
            });
        }
        let (repo, n) = path.rsplit_once("/pull/")?;
        if repo.split('/').count() != 2 {
            return None;
        }
        Some(PrRef {
            provider: "github",
            host: host.into(),
            repo: repo.into(),
            number: number(n)?,
        })
    }
}

/// A pull request as the forge reports it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgePr {
    pub title: String,
    /// The description, Markdown; empty when it has none.
    pub body: String,
    pub author: Option<String>,
    pub state: PullRequestState,
    pub head_ref: Option<String>,
    pub base_ref: Option<String>,
    pub head_sha: Option<String>,
    pub mergeable: Mergeability,
    pub review_decision: ReviewDecision,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
    pub changed_files: Option<u64>,
    pub opened_at: Option<String>,
    pub updated_at: Option<String>,
    pub merged_at: Option<String>,
    pub closed_at: Option<String>,
    pub checks: Vec<Check>,
    pub reviews: Vec<Review>,
    /// Every comment, on the conversation and inline; `None` when they were
    /// not read, which keeps the ones recorded.
    pub comments: Option<Vec<PrComment>>,
}

impl ForgePr {
    pub fn checks_state(&self) -> ChecksState {
        checks_state(&self.checks)
    }
}

/// One state for a set of checks: any failure fails, else anything
/// unfinished is pending.
pub fn checks_state(checks: &[Check]) -> ChecksState {
    if checks.is_empty() {
        ChecksState::None
    } else if checks
        .iter()
        .any(|c| matches!(c.status, CheckStatus::Failure | CheckStatus::Cancelled))
    {
        ChecksState::Failing
    } else if checks.iter().any(|c| c.status == CheckStatus::Pending) {
        ChecksState::Pending
    } else {
        ChecksState::Passing
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ForgeError {
    #[error("{0} pull requests are not supported yet")]
    Unsupported(&'static str),
    #[error("gh is not installed or not on PATH")]
    Missing,
    #[error("gh failed: {0}")]
    Command(String),
    #[error("could not parse the forge's answer: {0}")]
    Parse(String),
}

#[async_trait]
pub trait Forge: Send + Sync {
    /// The pull request at `url` as it is now.
    async fn pull_request(&self, url: &str) -> Result<ForgePr, ForgeError>;

    /// The pull request's whole unified diff, cut at [`MAX_PATCH_BYTES`].
    /// Returns the patch and whether it was cut.
    async fn diff(&self, url: &str) -> Result<(String, bool), ForgeError>;

    /// Repositories the forge account owns, collaborates on or reaches
    /// through its organizations, most recently pushed first.
    /// `refresh` skips any recently cached answer.
    async fn repositories(&self, refresh: bool) -> Result<Vec<ForgeRepository>, ForgeError>;

    /// The bytes of `path` on one side of the pull request at `url`: the
    /// head at `head_sha`, or the base where the head branched from
    /// `base_ref`. At most `max` bytes, and whether it was cut there;
    /// `None` when the file does not exist on that side.
    async fn file(
        &self,
        url: &str,
        file: &FileRef<'_>,
        max: usize,
    ) -> Result<Option<(Vec<u8>, bool)>, ForgeError>;
}

/// One version of one file of a pull request.
#[derive(Debug, Clone, Copy)]
pub struct FileRef<'a> {
    pub path: &'a str,
    pub side: PrFileSide,
    pub head_sha: &'a str,
    pub base_ref: &'a str,
}

/// A repository list and when gh answered it.
type CachedRepos = (std::time::Instant, Vec<ForgeRepository>);

/// How long a repository list is reused before gh is asked again.
const REPOS_TTL: Duration = Duration::from_secs(120);

/// How many repositories are listed at most.
pub const MAX_REPOSITORIES: usize = 1000;

/// Parses `gh api --paginate user/repos`: one JSON array per page,
/// concatenated.
pub fn parse_gh_repos(out: &[u8]) -> Result<Vec<ForgeRepository>, ForgeError> {
    let mut repos = Vec::new();
    for page in serde_json::Deserializer::from_slice(out).into_iter::<Vec<ForgeRepository>>() {
        repos.extend(page.map_err(|e| ForgeError::Parse(e.to_string()))?);
    }
    repos.truncate(MAX_REPOSITORIES);
    Ok(repos)
}

/// GitHub through the `gh` CLI.
#[derive(Debug, Clone)]
pub struct GhForge {
    gh: String,
    repos: std::sync::Arc<Mutex<Option<CachedRepos>>>,
}

impl Default for GhForge {
    fn default() -> Self {
        Self::new("gh")
    }
}

impl GhForge {
    pub fn new(gh: impl Into<String>) -> Self {
        Self {
            gh: gh.into(),
            repos: Default::default(),
        }
    }

    fn github(url: &str) -> Result<(), ForgeError> {
        match PrRef::parse(url) {
            Some(r) if r.provider == "github" => Ok(()),
            Some(r) => Err(ForgeError::Unsupported(r.provider)),
            None => Err(ForgeError::Parse(format!("not a pull request URL: {url}"))),
        }
    }

    /// Runs gh and returns stdout, cut at `max` bytes, and whether it was cut.
    async fn run(&self, args: &[&str], max: usize) -> Result<(Vec<u8>, bool), ForgeError> {
        let mut child = Command::new(&self.gh)
            .args(args)
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => ForgeError::Missing,
                _ => ForgeError::Command(e.to_string()),
            })?;
        let mut stdout = child.stdout.take().expect("piped");
        let mut stderr = child.stderr.take().expect("piped");
        let work = async {
            let mut out = Vec::new();
            let mut err = Vec::new();
            let read_out = async {
                let mut buf = [0u8; 64 * 1024];
                loop {
                    let n = stdout.read(&mut buf).await?;
                    if n == 0 || out.len() > max {
                        return Ok::<_, std::io::Error>(());
                    }
                    out.extend_from_slice(&buf[..n]);
                }
            };
            let (r1, r2) = tokio::join!(read_out, stderr.read_to_end(&mut err));
            r1.map_err(|e| ForgeError::Command(e.to_string()))?;
            r2.map_err(|e| ForgeError::Command(e.to_string()))?;
            Ok::<_, ForgeError>((out, err))
        };
        let (mut out, err) = tokio::time::timeout(GH_TIMEOUT, work)
            .await
            .map_err(|_| ForgeError::Command("timed out".into()))??;
        let cut = out.len() > max;
        if cut {
            let _ = child.start_kill();
            out.truncate(max);
            return Ok((out, true));
        }
        let status = child
            .wait()
            .await
            .map_err(|e| ForgeError::Command(e.to_string()))?;
        if !status.success() {
            let msg = String::from_utf8_lossy(&err).trim().to_string();
            return Err(ForgeError::Command(if msg.is_empty() {
                format!("exit {status}")
            } else {
                msg
            }));
        }
        Ok((out, false))
    }
}

impl GhForge {
    /// Every comment on the pull request at `url`: its conversation
    /// comments and its inline review comments, through the REST API. The
    /// reviews are read too, to name each inline comment's review by the id
    /// `gh pr view` gives it.
    async fn comments(&self, url: &str) -> Result<Vec<PrComment>, ForgeError> {
        let r = PrRef::parse(url).ok_or_else(|| ForgeError::Parse(url.into()))?;
        let page = |path: String| {
            let host = r.host.clone();
            async move {
                let (out, cut) = self
                    .run(
                        &["api", "--hostname", &host, "--paginate", &path],
                        16 * 1024 * 1024,
                    )
                    .await?;
                if cut {
                    return Err(ForgeError::Parse("answer too large".into()));
                }
                Ok(out)
            }
        };
        let base = format!("repos/{}", r.repo);
        let issue = page(format!("{base}/issues/{}/comments?per_page=100", r.number)).await?;
        let inline = page(format!("{base}/pulls/{}/comments?per_page=100", r.number)).await?;
        let reviews = page(format!("{base}/pulls/{}/reviews?per_page=100", r.number)).await?;
        parse_gh_comments(&issue, &inline, &reviews)
    }
}

/// Percent-encodes one path segment or ref for a REST path.
fn path_escape(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// A comment as the REST API reports it, on the conversation or inline.
#[derive(Deserialize)]
struct RestComment {
    id: u64,
    node_id: Option<String>,
    user: Option<GhLogin>,
    #[serde(default)]
    body: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    html_url: Option<String>,
    // Inline only.
    pull_request_review_id: Option<u64>,
    path: Option<String>,
    line: Option<u64>,
    original_line: Option<u64>,
    side: Option<String>,
    commit_id: Option<String>,
    in_reply_to_id: Option<u64>,
}

#[derive(Deserialize)]
struct RestReview {
    id: u64,
    node_id: Option<String>,
}

/// Concatenated pages of one REST list.
fn rest_pages<T: serde::de::DeserializeOwned>(out: &[u8]) -> Result<Vec<T>, ForgeError> {
    let mut items = Vec::new();
    for page in serde_json::Deserializer::from_slice(out).into_iter::<Vec<T>>() {
        items.extend(page.map_err(|e| ForgeError::Parse(e.to_string()))?);
    }
    Ok(items)
}

/// Parses the REST lists of a pull request's conversation comments, inline
/// comments and reviews (each `gh api --paginate` output) into comments.
/// Comment ids are the forge's node ids; an inline comment's `review_id` is
/// its review's node id, as `gh pr view` names reviews.
pub fn parse_gh_comments(
    issue: &[u8],
    inline: &[u8],
    reviews: &[u8],
) -> Result<Vec<PrComment>, ForgeError> {
    let reviews: HashMap<u64, String> = rest_pages::<RestReview>(reviews)?
        .into_iter()
        .filter_map(|r| Some((r.id, r.node_id?)))
        .collect();
    let inline: Vec<RestComment> = rest_pages(inline)?;
    let node_of: HashMap<u64, String> = inline
        .iter()
        .map(|c| (c.id, c.node_id.clone().unwrap_or_else(|| c.id.to_string())))
        .collect();
    let comment = |c: RestComment, kind: PrCommentKind| PrComment {
        id: c.node_id.unwrap_or_else(|| c.id.to_string()),
        kind,
        author: c.user.map(|u| u.login),
        body: c.body.unwrap_or_default(),
        created_at: c.created_at,
        updated_at: c.updated_at,
        url: c.html_url,
        review_id: c
            .pull_request_review_id
            .map(|r| reviews.get(&r).cloned().unwrap_or_else(|| r.to_string())),
        path: c.path,
        line: c.line,
        original_line: c.original_line,
        side: c.side.as_deref().map(|s| match s {
            "LEFT" => DiffSide::Old,
            _ => DiffSide::New,
        }),
        commit: c.commit_id,
        in_reply_to: c
            .in_reply_to_id
            .map(|r| node_of.get(&r).cloned().unwrap_or_else(|| r.to_string())),
    };
    let mut out: Vec<PrComment> = rest_pages::<RestComment>(issue)?
        .into_iter()
        .map(|c| comment(c, PrCommentKind::Conversation))
        .collect();
    out.extend(
        inline
            .into_iter()
            .map(|c| comment(c, PrCommentKind::Inline)),
    );
    Ok(out)
}

#[async_trait]
impl Forge for GhForge {
    async fn pull_request(&self, url: &str) -> Result<ForgePr, ForgeError> {
        Self::github(url)?;
        let (out, cut) = self
            .run(&["pr", "view", url, "--json", GH_FIELDS], 16 * 1024 * 1024)
            .await?;
        if cut {
            return Err(ForgeError::Parse("answer too large".into()));
        }
        let mut pr = parse_gh_view(&out)?;
        // Comments are a second read: a failure keeps the ones recorded.
        match self.comments(url).await {
            Ok(c) => pr.comments = Some(c),
            Err(e) => tracing::debug!(pr = url, error = %e, "pull request comments not read"),
        }
        Ok(pr)
    }

    async fn diff(&self, url: &str) -> Result<(String, bool), ForgeError> {
        Self::github(url)?;
        let (out, cut) = self
            .run(&["pr", "diff", url, "--color", "never"], MAX_PATCH_BYTES)
            .await?;
        Ok((String::from_utf8_lossy(&out).into_owned(), cut))
    }

    async fn file(
        &self,
        url: &str,
        file: &FileRef<'_>,
        max: usize,
    ) -> Result<Option<(Vec<u8>, bool)>, ForgeError> {
        Self::github(url)?;
        let r = PrRef::parse(url).ok_or_else(|| ForgeError::Parse(url.into()))?;
        let sha = match file.side {
            PrFileSide::Head => file.head_sha.to_string(),
            PrFileSide::Base => {
                let path = format!(
                    "repos/{}/compare/{}...{}",
                    r.repo,
                    path_escape(file.base_ref),
                    file.head_sha
                );
                let (out, _) = self
                    .run(
                        &[
                            "api",
                            "--hostname",
                            &r.host,
                            &path,
                            "--jq",
                            ".merge_base_commit.sha",
                        ],
                        4096,
                    )
                    .await?;
                let sha = String::from_utf8_lossy(&out).trim().to_string();
                if sha.is_empty() {
                    return Err(ForgeError::Parse("no merge base".into()));
                }
                sha
            }
        };
        let path = format!(
            "repos/{}/contents/{}?ref={sha}",
            r.repo,
            file.path
                .split('/')
                .map(path_escape)
                .collect::<Vec<_>>()
                .join("/")
        );
        let res = self
            .run(
                &[
                    "api",
                    "--hostname",
                    &r.host,
                    "-H",
                    "Accept: application/vnd.github.raw",
                    &path,
                ],
                max,
            )
            .await;
        match res {
            Ok(bytes) => Ok(Some(bytes)),
            Err(ForgeError::Command(m)) if m.contains("Not Found") || m.contains("HTTP 404") => {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    async fn repositories(&self, refresh: bool) -> Result<Vec<ForgeRepository>, ForgeError> {
        if !refresh {
            if let Some((at, repos)) = &*self.repos.lock().unwrap() {
                if at.elapsed() < REPOS_TTL {
                    return Ok(repos.clone());
                }
            }
        }
        let pages = MAX_REPOSITORIES / 100;
        let mut repos = Vec::new();
        for page in 1..=pages {
            let path = format!(
                "user/repos?per_page=100&sort=pushed&affiliation=owner,collaborator,organization_member&page={page}"
            );
            let (out, cut) = self.run(&["api", &path], 16 * 1024 * 1024).await?;
            if cut {
                return Err(ForgeError::Parse("answer too large".into()));
            }
            let got = parse_gh_repos(&out)?;
            let last = got.len() < 100;
            repos.extend(got);
            if last {
                break;
            }
        }
        *self.repos.lock().unwrap() = Some((std::time::Instant::now(), repos.clone()));
        Ok(repos)
    }
}

/// Canned forge answers by URL. A URL with no answer fails as a gh error.
#[derive(Debug, Default)]
pub struct StubForge {
    prs: Mutex<HashMap<String, ForgePr>>,
    diffs: Mutex<HashMap<String, String>>,
    reads: Mutex<Vec<String>>,
    repos: Mutex<Option<Vec<ForgeRepository>>>,
    files: Mutex<HashMap<(String, String, PrFileSide), Vec<u8>>>,
}

impl StubForge {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, url: &str, pr: ForgePr) {
        self.prs.lock().unwrap().insert(url.into(), pr);
    }

    pub fn set_diff(&self, url: &str, patch: &str) {
        self.diffs.lock().unwrap().insert(url.into(), patch.into());
    }

    /// What [`Forge::repositories`] answers; until set it fails as if gh
    /// were missing.
    pub fn set_repositories(&self, repos: Vec<ForgeRepository>) {
        *self.repos.lock().unwrap() = Some(repos);
    }

    /// `path`'s bytes on `side` of the pull request at `url`.
    pub fn set_file(&self, url: &str, path: &str, side: PrFileSide, bytes: &[u8]) {
        self.files
            .lock()
            .unwrap()
            .insert((url.into(), path.into(), side), bytes.to_vec());
    }

    /// URLs read with [`Forge::pull_request`], oldest first.
    pub fn reads(&self) -> Vec<String> {
        self.reads.lock().unwrap().clone()
    }
}

#[async_trait]
impl Forge for StubForge {
    async fn pull_request(&self, url: &str) -> Result<ForgePr, ForgeError> {
        self.reads.lock().unwrap().push(url.into());
        self.prs
            .lock()
            .unwrap()
            .get(url)
            .cloned()
            .ok_or_else(|| ForgeError::Command(format!("no pull request at {url}")))
    }

    async fn diff(&self, url: &str) -> Result<(String, bool), ForgeError> {
        self.diffs
            .lock()
            .unwrap()
            .get(url)
            .cloned()
            .map(|d| (d, false))
            .ok_or_else(|| ForgeError::Command(format!("no pull request at {url}")))
    }

    async fn file(
        &self,
        url: &str,
        file: &FileRef<'_>,
        max: usize,
    ) -> Result<Option<(Vec<u8>, bool)>, ForgeError> {
        let files = self.files.lock().unwrap();
        Ok(files
            .get(&(url.to_string(), file.path.to_string(), file.side))
            .map(|b| {
                let cut = b.len() > max;
                (b[..b.len().min(max)].to_vec(), cut)
            }))
    }

    async fn repositories(&self, _refresh: bool) -> Result<Vec<ForgeRepository>, ForgeError> {
        self.repos
            .lock()
            .unwrap()
            .clone()
            .ok_or(ForgeError::Missing)
    }
}

/// The part of `patch` for one file, matched against either side's path in
/// its `diff --git` header. `None` when the diff does not touch the file.
pub fn file_patch(patch: &str, path: &str) -> Option<String> {
    let mut out = String::new();
    let mut taking = false;
    for chunk in patch.split_inclusive('\n') {
        if let Some(header) = chunk.strip_prefix("diff --git ") {
            let header = header.trim_end();
            taking = header == format!("a/{path} b/{path}")
                || header.starts_with(&format!("a/{path} b/"))
                || header.ends_with(&format!(" b/{path}"));
        }
        if taking {
            out.push_str(chunk);
        }
    }
    (!out.is_empty()).then_some(out)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhView {
    title: String,
    #[serde(default)]
    body: Option<String>,
    state: String,
    #[serde(default)]
    is_draft: bool,
    author: Option<GhLogin>,
    head_ref_name: Option<String>,
    base_ref_name: Option<String>,
    head_ref_oid: Option<String>,
    mergeable: Option<String>,
    review_decision: Option<String>,
    additions: Option<u64>,
    deletions: Option<u64>,
    changed_files: Option<u64>,
    created_at: Option<String>,
    updated_at: Option<String>,
    merged_at: Option<String>,
    closed_at: Option<String>,
    #[serde(default)]
    status_check_rollup: Vec<GhCheck>,
    #[serde(default)]
    reviews: Vec<GhReview>,
}

#[derive(Deserialize)]
struct GhLogin {
    login: String,
}

/// A `CheckRun` or a `StatusContext`, told apart by `__typename`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhCheck {
    #[serde(rename = "__typename")]
    typename: Option<String>,
    // CheckRun
    name: Option<String>,
    workflow_name: Option<String>,
    status: Option<String>,
    conclusion: Option<String>,
    details_url: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
    // StatusContext
    context: Option<String>,
    state: Option<String>,
    target_url: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhReview {
    id: Option<String>,
    author: Option<GhLogin>,
    state: String,
    #[serde(default)]
    body: String,
    submitted_at: Option<String>,
    commit: Option<GhCommit>,
}

#[derive(Deserialize)]
struct GhCommit {
    oid: String,
}

/// Parses `gh pr view --json` output for [`GH_FIELDS`].
pub fn parse_gh_view(bytes: &[u8]) -> Result<ForgePr, ForgeError> {
    let v: GhView = serde_json::from_slice(bytes).map_err(|e| ForgeError::Parse(e.to_string()))?;
    let state = match (v.state.as_str(), v.is_draft) {
        ("MERGED", _) => PullRequestState::Merged,
        ("CLOSED", _) => PullRequestState::Closed,
        ("OPEN", true) => PullRequestState::Draft,
        ("OPEN", false) => PullRequestState::Open,
        (other, _) => return Err(ForgeError::Parse(format!("unknown state {other}"))),
    };
    let mergeable = match v.mergeable.as_deref() {
        Some("MERGEABLE") => Mergeability::Mergeable,
        Some("CONFLICTING") => Mergeability::Conflicting,
        _ => Mergeability::Unknown,
    };
    let review_decision = match v.review_decision.as_deref() {
        Some("APPROVED") => ReviewDecision::Approved,
        Some("CHANGES_REQUESTED") => ReviewDecision::ChangesRequested,
        Some("REVIEW_REQUIRED") => ReviewDecision::ReviewRequired,
        _ => ReviewDecision::None,
    };
    // A re-run check reports once per run; the rollup lists the latest, but
    // keep only the last entry per name so names stay unique.
    let mut checks: Vec<Check> = Vec::new();
    for c in v.status_check_rollup {
        let Some(check) = gh_check(c) else { continue };
        match checks.iter_mut().find(|x| x.name == check.name) {
            Some(x) => *x = check,
            None => checks.push(check),
        }
    }
    let reviews = v
        .reviews
        .into_iter()
        .enumerate()
        .filter_map(|(i, r)| {
            let state = match r.state.as_str() {
                "APPROVED" => ReviewState::Approved,
                "CHANGES_REQUESTED" => ReviewState::ChangesRequested,
                "COMMENTED" => ReviewState::Commented,
                "DISMISSED" => ReviewState::Dismissed,
                "PENDING" => ReviewState::Pending,
                _ => return None,
            };
            Some(Review {
                id: r.id.unwrap_or_else(|| format!("review-{i}")),
                author: r.author.map(|a| a.login),
                state,
                body: r.body,
                submitted_at: r.submitted_at,
                commit: r.commit.map(|c| c.oid),
            })
        })
        .collect();
    Ok(ForgePr {
        title: v.title,
        body: v.body.unwrap_or_default(),
        author: v.author.map(|a| a.login),
        state,
        head_ref: v.head_ref_name,
        base_ref: v.base_ref_name,
        head_sha: v.head_ref_oid,
        mergeable,
        review_decision,
        additions: v.additions,
        deletions: v.deletions,
        changed_files: v.changed_files,
        opened_at: v.created_at,
        updated_at: v.updated_at,
        merged_at: v.merged_at,
        closed_at: v.closed_at,
        checks,
        reviews,
        comments: None,
    })
}

fn gh_check(c: GhCheck) -> Option<Check> {
    if c.typename.as_deref() == Some("StatusContext") || c.context.is_some() {
        let state = c.state?;
        let status = match state.as_str() {
            "SUCCESS" => CheckStatus::Success,
            "FAILURE" | "ERROR" => CheckStatus::Failure,
            _ => CheckStatus::Pending,
        };
        return Some(Check {
            name: c.context?,
            status,
            conclusion: (status == CheckStatus::Failure).then(|| state.to_lowercase()),
            details_url: c.target_url,
            started_at: c.started_at,
            completed_at: None,
        });
    }
    let name = c.name?;
    let name = match c.workflow_name.filter(|w| !w.is_empty()) {
        Some(w) => format!("{w} / {name}"),
        None => name,
    };
    let conclusion = c.conclusion.filter(|s| !s.is_empty());
    let status = if c.status.as_deref() != Some("COMPLETED") {
        CheckStatus::Pending
    } else {
        match conclusion.as_deref() {
            Some("SUCCESS") => CheckStatus::Success,
            Some("NEUTRAL") | Some("SKIPPED") => CheckStatus::Neutral,
            Some("CANCELLED") => CheckStatus::Cancelled,
            _ => CheckStatus::Failure,
        }
    };
    Some(Check {
        name,
        status,
        conclusion: conclusion.map(|s| s.to_lowercase()),
        details_url: c.details_url,
        started_at: c.started_at,
        completed_at: c.completed_at.filter(|s| !s.starts_with("0001-")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_repository_pages() {
        let page = |name: &str| {
            format!(
                r#"[{{"full_name":"{name}","private":true,"archived":false,"description":null,
                "pushed_at":"2026-10-09T12:00:00Z","ssh_url":"git@github.com:{name}.git",
                "clone_url":"https://github.com/{name}.git","stargazers_count":3}}]"#
            )
        };
        let out = format!("{}\n{}", page("a/one"), page("b/two"));
        let repos = parse_gh_repos(out.as_bytes()).unwrap();
        assert_eq!(repos.len(), 2);
        assert_eq!(repos[1].full_name, "b/two");
        assert_eq!(repos[0].ssh_url, "git@github.com:a/one.git");
        assert!(repos[0].private && repos[0].description.is_none());
        assert!(parse_gh_repos(b"{").is_err());
    }

    #[test]
    fn parses_rest_comments_and_links_them_to_reviews() {
        let issue =
            br#"[{"id": 1, "node_id": "IC_1", "user": {"login": "matt"}, "body": "Looks good",
            "created_at": "2026-10-02T10:00:00Z", "updated_at": "2026-10-02T11:00:00Z",
            "html_url": "https://github.com/a/b/pull/1#issuecomment-1"}]
            [{"id": 2, "node_id": "IC_2", "user": null, "body": null}]"#;
        let inline = br#"[{"id": 10, "node_id": "RC_10", "user": {"login": "rev"}, "body": "Why?",
            "pull_request_review_id": 100, "path": "src/a.rs", "line": 7, "original_line": 5,
            "side": "RIGHT", "commit_id": "abc", "created_at": "2026-10-02T10:30:00Z"},
            {"id": 11, "node_id": "RC_11", "user": {"login": "matt"}, "body": "Because.",
            "pull_request_review_id": 101, "path": "src/a.rs", "line": null, "original_line": 5,
            "side": "LEFT", "in_reply_to_id": 10}]"#;
        let reviews = br#"[{"id": 100, "node_id": "PRR_100"}]"#;
        let c = parse_gh_comments(issue, inline, reviews).unwrap();
        assert_eq!(c.len(), 4);
        assert_eq!(
            (c[0].id.as_str(), c[0].kind, c[0].author.as_deref()),
            ("IC_1", PrCommentKind::Conversation, Some("matt"))
        );
        assert_eq!(c[0].updated_at.as_deref(), Some("2026-10-02T11:00:00Z"));
        assert_eq!((c[1].body.as_str(), c[1].author.as_deref()), ("", None));
        assert_eq!(c[2].kind, PrCommentKind::Inline);
        assert_eq!(c[2].review_id.as_deref(), Some("PRR_100"));
        assert_eq!((c[2].line, c[2].side), (Some(7), Some(DiffSide::New)));
        assert_eq!(c[3].in_reply_to.as_deref(), Some("RC_10"));
        assert_eq!(
            c[3].review_id.as_deref(),
            Some("101"),
            "an unlisted review keeps its number"
        );
        assert_eq!((c[3].line, c[3].side), (None, Some(DiffSide::Old)));
        assert!(parse_gh_comments(b"{", b"", b"").is_err());
    }

    #[test]
    fn parses_pull_request_urls() {
        let r = PrRef::parse("https://github.com/quark-systems/quark/pull/16").unwrap();
        assert_eq!(
            (r.provider, r.host.as_str(), r.repo.as_str(), r.number),
            ("github", "github.com", "quark-systems/quark", 16)
        );
        let r = PrRef::parse("https://gitlab.com/g/sub/repo/-/merge_requests/3").unwrap();
        assert_eq!(
            (r.provider, r.repo.as_str(), r.number),
            ("gitlab", "g/sub/repo", 3)
        );
        assert!(PrRef::parse("http://github.com/a/b/pull/1").is_none());
        assert!(PrRef::parse("https://github.com/a/b/issues/1").is_none());
        assert!(PrRef::parse("https://github.com/a/b/pull/0").is_none());
        assert!(PrRef::parse("https://github.com/a/b/c/pull/1").is_none());
    }

    #[test]
    fn parses_gh_view() {
        let pr = parse_gh_view(
            br#"{
            "number": 16, "title": "PR center", "state": "OPEN", "isDraft": false,
            "author": {"login": "mattsanchez"}, "headRefName": "f", "baseRefName": "main",
            "headRefOid": "abc", "mergeable": "MERGEABLE", "reviewDecision": "",
            "additions": 10, "deletions": 2, "changedFiles": 3,
            "createdAt": "2026-10-02T00:00:00Z", "updatedAt": "2026-10-02T01:00:00Z",
            "mergedAt": null, "closedAt": null,
            "statusCheckRollup": [
              {"__typename": "CheckRun", "name": "test", "workflowName": "CI",
               "status": "COMPLETED", "conclusion": "SUCCESS", "detailsUrl": "https://x",
               "startedAt": "2026-10-02T00:00:00Z", "completedAt": "2026-10-02T00:05:00Z"},
              {"__typename": "CheckRun", "name": "lint", "workflowName": "CI",
               "status": "IN_PROGRESS", "conclusion": "", "completedAt": "0001-01-01T00:00:00Z"},
              {"__typename": "StatusContext", "context": "deploy", "state": "FAILURE",
               "targetUrl": "https://y"}
            ],
            "reviews": [
              {"id": "R1", "author": {"login": "a"}, "state": "APPROVED", "body": "ok",
               "submittedAt": "2026-10-02T02:00:00Z", "commit": {"oid": "abc"}}
            ]
        }"#,
        )
        .unwrap();
        assert_eq!(pr.state, PullRequestState::Open);
        assert_eq!(pr.mergeable, Mergeability::Mergeable);
        assert_eq!(pr.review_decision, ReviewDecision::None);
        assert_eq!(pr.checks.len(), 3);
        assert_eq!(pr.checks[0].name, "CI / test");
        assert_eq!(pr.checks[0].status, CheckStatus::Success);
        assert_eq!(pr.checks[1].status, CheckStatus::Pending);
        assert_eq!(pr.checks[1].completed_at, None);
        assert_eq!(pr.checks[2].status, CheckStatus::Failure);
        assert_eq!(pr.checks_state(), ChecksState::Failing);
        assert_eq!(pr.reviews[0].state, ReviewState::Approved);
        assert_eq!(pr.reviews[0].commit.as_deref(), Some("abc"));

        let draft = parse_gh_view(br#"{"title":"t","state":"OPEN","isDraft":true}"#).unwrap();
        assert_eq!(draft.state, PullRequestState::Draft);
        assert_eq!(draft.checks_state(), ChecksState::None);
    }

    #[test]
    fn rolls_checks_up() {
        let c = |status| Check {
            name: format!("{status:?}"),
            status,
            conclusion: None,
            details_url: None,
            started_at: None,
            completed_at: None,
        };
        assert_eq!(
            checks_state(&[c(CheckStatus::Success), c(CheckStatus::Neutral)]),
            ChecksState::Passing
        );
        assert_eq!(
            checks_state(&[c(CheckStatus::Success), c(CheckStatus::Pending)]),
            ChecksState::Pending
        );
        assert_eq!(
            checks_state(&[c(CheckStatus::Pending), c(CheckStatus::Cancelled)]),
            ChecksState::Failing
        );
    }

    #[test]
    fn cuts_one_file_out_of_a_patch() {
        let patch = "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -1 +1 @@\n-a\n+b\n\
                     diff --git a/old.rs b/new.rs\nsimilarity index 90%\n";
        assert_eq!(
            file_patch(patch, "x.rs").unwrap(),
            "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -1 +1 @@\n-a\n+b\n"
        );
        assert!(file_patch(patch, "new.rs").unwrap().contains("similarity"));
        assert!(file_patch(patch, "old.rs").unwrap().contains("similarity"));
        assert_eq!(file_patch(patch, "y.rs"), None);
    }
}
