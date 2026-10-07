//! [`NativePipeline`]: the native [`VerifyPipeline`]. It runs a Project's
//! gates on a head in a scratch worktree, and rebases onto main first when
//! asked to re-verify.
//!
//! Gates come from firstmate's `config/gates.json` (schema `fm.gates.v1`),
//! so both engines run the same gates while slice 2 is in shadow:
//!
//! 1. **checks** ([`GateStage::RepoChecks`]): the repo's own commands, in
//!    order, stopping at the first failure.
//! 2. **journeys** ([`GateStage::Journeys`]): start the app in its own process
//!    group, wait until its URL accepts connections, run the journey command,
//!    then stop the whole group. The app stays up through the holdout gate.
//! 3. **holdout** ([`GateStage::Holdout`]): tests the worker never sees,
//!    cloned from the holdout repo. Each directory under the holdout path is
//!    one category with an executable `run`. Their summaries name only the
//!    category and whether it passed, never test names or output.
//!
//! A Project with no gates gets one passed [`GateStage::RepoChecks`] stage
//! that says so, because a verdict with no stages never passes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use quark_core::verify::{Change, GateStage, StageResult, Verdict, VerifyPipeline};
use quark_core::{CoreError, ProjectId, Result};
use serde::Deserialize;
use tokio::process::Command;

/// `config/gates.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct GatesConfig {
    #[serde(default)]
    pub schema: String,
    /// By repo name, as the clone's directory is named.
    #[serde(default)]
    pub repos: HashMap<String, RepoGates>,
}

impl GatesConfig {
    pub fn parse(json: &str) -> Result<Self> {
        let c: GatesConfig = serde_json::from_str(json)
            .map_err(|e| CoreError::Invalid(format!("gates config: {e}")))?;
        if c.schema != "fm.gates.v1" {
            return Err(CoreError::Invalid(format!(
                "gates config schema {:?}, expected fm.gates.v1",
                c.schema
            )));
        }
        Ok(c)
    }

    /// Read `path`; a missing file is an empty config.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => Self::parse(&s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(CoreError::Backend(format!("{}: {e}", path.display()))),
        }
    }
}

/// One repo's gates.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct RepoGates {
    #[serde(default)]
    pub checks: Vec<CheckGate>,
    #[serde(default)]
    pub journeys: Option<JourneysGate>,
    #[serde(default)]
    pub holdout: Option<HoldoutGate>,
}

impl RepoGates {
    /// Whether any gate is declared, firstmate's test for requiring evidence.
    pub fn declared(&self) -> bool {
        !self.checks.is_empty() || self.journeys.is_some() || self.holdout.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CheckGate {
    pub name: String,
    pub run: String,
    #[serde(default)]
    pub timeout_s: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct JourneysGate {
    #[serde(default)]
    pub dir: Option<String>,
    #[serde(default)]
    pub setup: Option<String>,
    pub start: String,
    pub url: String,
    #[serde(default)]
    pub ready_timeout_s: Option<u64>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub timeout_s: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HoldoutGate {
    pub repo: String,
    #[serde(default, rename = "ref")]
    pub git_ref: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub timeout_s: Option<u64>,
}

/// Where a Project's changes are verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineTarget {
    /// A clone of the Project's repo with an `origin` remote. Never changed:
    /// work happens in scratch worktrees made from it.
    pub checkout: PathBuf,
    /// The repo name the gates config is keyed by.
    pub repo_name: String,
    /// The branch changes merge into.
    pub base: String,
    pub gates: RepoGates,
    /// Push a rebased head to its branch (with a lease on the old head)
    /// before verifying it. Off in shadow mode, where native never writes.
    pub push_rebased: bool,
}

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(1800);

/// The native gate runner.
pub struct NativePipeline {
    scratch: PathBuf,
    targets: Mutex<HashMap<ProjectId, PipelineTarget>>,
}

impl NativePipeline {
    /// `scratch` holds worktrees and logs; it is created if missing.
    pub fn new(scratch: impl Into<PathBuf>) -> Self {
        Self {
            scratch: scratch.into(),
            targets: Mutex::default(),
        }
    }

    pub fn set_target(&self, project: ProjectId, target: PipelineTarget) {
        self.targets.lock().unwrap().insert(project, target);
    }

    fn target(&self, project: &ProjectId) -> Result<PipelineTarget> {
        self.targets
            .lock()
            .unwrap()
            .get(project)
            .cloned()
            .ok_or_else(|| CoreError::NotFound(format!("pipeline target for project {project}")))
    }

    /// A fresh detached worktree of `t.checkout` at `rev`.
    async fn worktree(&self, t: &PipelineTarget, change: &Change, rev: &str) -> Result<Scratch> {
        tokio::fs::create_dir_all(&self.scratch)
            .await
            .map_err(|e| CoreError::Backend(format!("{}: {e}", self.scratch.display())))?;
        let name = format!(
            "{}-{}-{}",
            safe(change.project.as_str()),
            safe(change.task.as_str()),
            uuid::Uuid::now_v7().simple()
        );
        let dir = self.scratch.join(name);
        // Best effort: the objects may already be local.
        let _ = git(
            &t.checkout,
            &["fetch", "--quiet", "origin", &t.base, &change.branch],
        )
        .await;
        git(
            &t.checkout,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                path_str(&dir)?,
                rev,
            ],
        )
        .await?;
        Ok(Scratch {
            checkout: t.checkout.clone(),
            dir,
        })
    }
}

/// A scratch worktree, removed by [`Scratch::remove`].
struct Scratch {
    checkout: PathBuf,
    dir: PathBuf,
}

impl Scratch {
    async fn remove(self) {
        if let Ok(dir) = path_str(&self.dir) {
            if let Err(e) = git(&self.checkout, &["worktree", "remove", "--force", dir]).await {
                tracing::warn!(dir, error = %e, "could not remove a scratch worktree");
            }
        }
    }
}

#[async_trait]
impl VerifyPipeline for NativePipeline {
    async fn verify(&self, change: &Change) -> Result<Verdict> {
        let t = self.target(&change.project)?;
        let wt = self.worktree(&t, change, &change.head).await?;
        let head = git(&wt.dir, &["rev-parse", "HEAD"]).await;
        let result = match head {
            Ok(head) => Ok(Verdict {
                stages: run_gates(&t, &wt.dir, &head, &self.scratch).await,
                head,
                conflict: None,
            }),
            Err(e) => Err(e),
        };
        wt.remove().await;
        result
    }

    async fn rebase_and_reverify(&self, change: &Change) -> Result<Verdict> {
        let t = self.target(&change.project)?;
        let wt = self.worktree(&t, change, &change.head).await?;
        let result = rebase_and_run(&t, change, &wt.dir, &self.scratch).await;
        wt.remove().await;
        result
    }
}

async fn rebase_and_run(
    t: &PipelineTarget,
    change: &Change,
    dir: &Path,
    scratch: &Path,
) -> Result<Verdict> {
    let onto = format!("origin/{}", t.base);
    if let Err(e) = git(dir, &["rebase", "--quiet", &onto]).await {
        let _ = git(dir, &["rebase", "--abort"]).await;
        return Ok(Verdict {
            head: change.head.clone(),
            stages: Vec::new(),
            conflict: Some(format!("could not rebase onto {onto}: {e}")),
        });
    }
    let head = git(dir, &["rev-parse", "HEAD"]).await?;
    if t.push_rebased && head != change.head {
        let lease = format!(
            "--force-with-lease=refs/heads/{}:{}",
            change.branch, change.head
        );
        let refspec = format!("HEAD:refs/heads/{}", change.branch);
        git(dir, &["push", "--quiet", &lease, "origin", &refspec]).await?;
    }
    Ok(Verdict {
        stages: run_gates(t, dir, &head, scratch).await,
        head,
        conflict: None,
    })
}

/// Run every declared gate on `dir`, stopping at the first failure.
async fn run_gates(t: &PipelineTarget, dir: &Path, head: &str, scratch: &Path) -> Vec<StageResult> {
    let gates = &t.gates;
    if !gates.declared() {
        return vec![StageResult {
            stage: GateStage::RepoChecks,
            passed: true,
            summary: format!("no gates are configured for {}", t.repo_name),
        }];
    }
    let logs = scratch.join("logs");
    let _ = tokio::fs::create_dir_all(&logs).await;
    let mut stages = Vec::new();

    if !gates.checks.is_empty() {
        let mut parts = Vec::new();
        let mut passed = true;
        for c in &gates.checks {
            let log = logs.join(format!(
                "{}-check-{}.log",
                &head[..head.len().min(12)],
                safe(&c.name)
            ));
            let out = run(&c.run, dir, &[], timeout(c.timeout_s), Some(&log)).await;
            parts.push(format!("{}: {}", c.name, out.describe()));
            if !out.ok() {
                passed = false;
                break;
            }
        }
        stages.push(StageResult {
            stage: GateStage::RepoChecks,
            passed,
            summary: parts.join("; "),
        });
        if !passed {
            return stages;
        }
    }

    let mut app: Option<App> = None;
    if let Some(j) = &gates.journeys {
        let (result, started) = journeys(j, dir, head, &logs).await;
        app = started;
        let ok = result.passed;
        stages.push(result);
        if !ok {
            if let Some(app) = app {
                app.stop().await;
            }
            return stages;
        }
    }

    if let Some(h) = &gates.holdout {
        let url = gates.journeys.as_ref().map(|j| j.url.as_str());
        stages.push(holdout(h, &t.repo_name, dir, head, url, scratch).await);
    }
    if let Some(app) = app {
        app.stop().await;
    }
    stages
}

async fn journeys(
    j: &JourneysGate,
    dir: &Path,
    head: &str,
    logs: &Path,
) -> (StageResult, Option<App>) {
    let fail = |summary: String| StageResult {
        stage: GateStage::Journeys,
        passed: false,
        summary,
    };
    let cwd = match &j.dir {
        Some(d) => dir.join(d),
        None => dir.to_path_buf(),
    };
    let short = &head[..head.len().min(12)];
    if let Some(setup) = &j.setup {
        let out = run(
            setup,
            &cwd,
            &[],
            DEFAULT_TIMEOUT,
            Some(&logs.join(format!("{short}-journeys-setup.log"))),
        )
        .await;
        if !out.ok() {
            return (fail(format!("setup: {}", out.describe())), None);
        }
    }
    let Some(addr) = host_port(&j.url) else {
        return (
            fail(format!("journeys url {:?} has no host and port", j.url)),
            None,
        );
    };
    if answers(&addr).await {
        return (
            fail(format!(
                "{} already answers; refusing to test an app this run did not start",
                j.url
            )),
            None,
        );
    }
    let app = match App::start(
        &j.start,
        &cwd,
        &logs.join(format!("{short}-journeys-app.log")),
    ) {
        Ok(a) => a,
        Err(e) => return (fail(format!("could not start the app: {e}")), None),
    };
    let ready = Duration::from_secs(j.ready_timeout_s.unwrap_or(120));
    let started = Instant::now();
    while !answers(&addr).await {
        if started.elapsed() > ready {
            app.stop().await;
            return (
                fail(format!(
                    "{} did not answer within {}s",
                    j.url,
                    ready.as_secs()
                )),
                None,
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let env = [
        ("BASE_URL", j.url.as_str()),
        ("PLAYWRIGHT_BASE_URL", j.url.as_str()),
        ("GATE_APP_URL", j.url.as_str()),
    ];
    let command = j.command.as_deref().unwrap_or("npx playwright test");
    let out = run(
        command,
        &cwd,
        &env,
        timeout(j.timeout_s),
        Some(&logs.join(format!("{short}-journeys.log"))),
    )
    .await;
    (
        StageResult {
            stage: GateStage::Journeys,
            passed: out.ok(),
            summary: format!("journeys: {}", out.describe()),
        },
        Some(app),
    )
}

async fn holdout(
    h: &HoldoutGate,
    repo_name: &str,
    target: &Path,
    head: &str,
    app_url: Option<&str>,
    scratch: &Path,
) -> StageResult {
    let fail = |summary: String| StageResult {
        stage: GateStage::Holdout,
        passed: false,
        summary,
    };
    let clone = scratch.join(format!("holdout-{}", uuid::Uuid::now_v7().simple()));
    let Ok(clone_str) = path_str(&clone) else {
        return fail("bad scratch path".into());
    };
    let mut args = vec!["clone", "--quiet", "--depth", "1"];
    if let Some(r) = &h.git_ref {
        args.extend(["--branch", r.as_str()]);
    }
    args.extend([h.repo.as_str(), clone_str]);
    if let Err(e) = git(scratch, &args).await {
        return fail(format!("could not fetch the holdout tests: {e}"));
    }
    let root = clone.join(
        h.path
            .clone()
            .unwrap_or_else(|| format!("holdout/{repo_name}")),
    );
    let mut categories = Vec::new();
    if let Ok(mut rd) = tokio::fs::read_dir(&root).await {
        while let Ok(Some(entry)) = rd.next_entry().await {
            let run = entry.path().join("run");
            if is_executable(&run) {
                categories.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    categories.sort();
    let result = if categories.is_empty() {
        fail("no holdout categories found".into())
    } else {
        let target = target.to_string_lossy().into_owned();
        let mut env = vec![("GATE_TARGET", target.as_str()), ("GATE_HEAD_SHA", head)];
        if let Some(url) = app_url {
            env.push(("GATE_APP_URL", url));
        }
        let mut parts = Vec::new();
        let mut passed = true;
        for c in &categories {
            let dir = root.join(c);
            // No log: holdout output never leaves the gate.
            let out = run("./run", &dir, &env, timeout(h.timeout_s), None).await;
            parts.push(format!(
                "{c}: {}",
                if out.ok() { "passed" } else { "failed" }
            ));
            passed &= out.ok();
        }
        StageResult {
            stage: GateStage::Holdout,
            passed,
            summary: parts.join("; "),
        }
    };
    let _ = tokio::fs::remove_dir_all(&clone).await;
    result
}

fn timeout(secs: Option<u64>) -> Duration {
    secs.map(Duration::from_secs).unwrap_or(DEFAULT_TIMEOUT)
}

/// How a gate command ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Exit(i32),
    Signal,
    TimedOut(Duration),
    Failed(String),
}

impl Outcome {
    fn ok(&self) -> bool {
        *self == Outcome::Exit(0)
    }

    fn describe(&self) -> String {
        match self {
            Outcome::Exit(0) => "passed".into(),
            Outcome::Exit(n) => format!("failed (exit {n})"),
            Outcome::Signal => "failed (killed by a signal)".into(),
            Outcome::TimedOut(d) => format!("failed (timed out after {}s)", d.as_secs()),
            Outcome::Failed(e) => format!("failed ({e})"),
        }
    }
}

fn shell(
    cmd: &str,
    cwd: &Path,
    env: &[(&str, &str)],
    log: Option<&Path>,
) -> std::io::Result<Command> {
    let mut c = Command::new("bash");
    c.args(["-c", cmd])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .process_group(0)
        .kill_on_drop(true);
    for (k, v) in env {
        c.env(k, v);
    }
    match log {
        Some(path) => {
            let f = std::fs::File::create(path)?;
            c.stdout(f.try_clone()?).stderr(f);
        }
        None => {
            c.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    Ok(c)
}

/// Run `cmd` through bash in its own process group; on timeout the whole
/// group is killed.
async fn run(
    cmd: &str,
    cwd: &Path,
    env: &[(&str, &str)],
    limit: Duration,
    log: Option<&Path>,
) -> Outcome {
    let mut child = match shell(cmd, cwd, env, log).and_then(|mut c| c.spawn()) {
        Ok(c) => c,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    let pgid = child.id();
    match tokio::time::timeout(limit, child.wait()).await {
        Ok(Ok(status)) => match status.code() {
            Some(code) => Outcome::Exit(code),
            None => Outcome::Signal,
        },
        Ok(Err(e)) => Outcome::Failed(e.to_string()),
        Err(_) => {
            if let Some(pgid) = pgid {
                kill_group(pgid, libc::SIGKILL);
            }
            let _ = child.wait().await;
            Outcome::TimedOut(limit)
        }
    }
}

fn kill_group(pgid: u32, signal: i32) {
    // SAFETY: kill(2) with a negative pid signals a process group; it has no
    // memory-safety preconditions.
    unsafe {
        libc::kill(-(pgid as i32), signal);
    }
}

/// The journeys app, running in its own process group.
struct App {
    child: tokio::process::Child,
    pgid: Option<u32>,
}

impl App {
    fn start(cmd: &str, cwd: &Path, log: &Path) -> std::io::Result<App> {
        let child = shell(cmd, cwd, &[], Some(log))?.spawn()?;
        let pgid = child.id();
        Ok(App { child, pgid })
    }

    /// TERM the group, then KILL it after 10 seconds.
    async fn stop(mut self) {
        if let Some(pgid) = self.pgid {
            kill_group(pgid, libc::SIGTERM);
            if tokio::time::timeout(Duration::from_secs(10), self.child.wait())
                .await
                .is_err()
            {
                kill_group(pgid, libc::SIGKILL);
                let _ = self.child.wait().await;
            }
        }
    }
}

fn host_port(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() {
        return None;
    }
    if authority.contains(':') && !authority.ends_with(']') {
        return Some(authority.to_string());
    }
    let port = if url.starts_with("https://") { 443 } else { 80 };
    Some(format!("{authority}:{port}"))
}

async fn answers(addr: &str) -> bool {
    matches!(
        tokio::time::timeout(Duration::from_secs(2), tokio::net::TcpStream::connect(addr)).await,
        Ok(Ok(_))
    )
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn safe(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn path_str(p: &Path) -> Result<&str> {
    p.to_str()
        .ok_or_else(|| CoreError::Invalid(format!("non-UTF-8 path {}", p.display())))
}

/// Run git in `dir`; stdout trimmed on success, the first stderr line on
/// failure.
async fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        // A rebase writes commits; it must not depend on the host's identity.
        .env("GIT_COMMITTER_NAME", "quark")
        .env("GIT_COMMITTER_EMAIL", "quark@localhost")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| CoreError::Backend(format!("git: {e}")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("failed")
            .trim();
        Err(CoreError::Backend(format!(
            "git {}: {line}",
            args.first().unwrap_or(&"")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gates_config() {
        let c = GatesConfig::parse(
            r#"{"schema":"fm.gates.v1","repos":{"app":{
              "checks":[{"name":"test","run":"cargo test","timeout_s":60}],
              "journeys":{"start":"npm run dev","url":"http://127.0.0.1:5173/"},
              "holdout":{"repo":"/h.git","ref":"main"}}}}"#,
        )
        .unwrap();
        let app = &c.repos["app"];
        assert!(app.declared());
        assert_eq!(app.checks[0].timeout_s, Some(60));
        assert_eq!(
            app.holdout.as_ref().unwrap().git_ref.as_deref(),
            Some("main")
        );
        assert!(GatesConfig::parse(r#"{"schema":"other","repos":{}}"#).is_err());
        assert!(!RepoGates::default().declared());
    }

    #[test]
    fn url_host_port() {
        assert_eq!(
            host_port("http://127.0.0.1:5173/").as_deref(),
            Some("127.0.0.1:5173")
        );
        assert_eq!(
            host_port("http://localhost/x").as_deref(),
            Some("localhost:80")
        );
        assert_eq!(
            host_port("https://example.com").as_deref(),
            Some("example.com:443")
        );
        assert_eq!(host_port("http:///"), None);
    }

    #[tokio::test]
    async fn run_reports_exit_and_timeout() {
        let dir = std::env::temp_dir();
        assert!(run("true", &dir, &[], Duration::from_secs(5), None)
            .await
            .ok());
        assert_eq!(
            run("exit 3", &dir, &[], Duration::from_secs(5), None).await,
            Outcome::Exit(3)
        );
        let t = run(
            "sleep 5 & sleep 5",
            &dir,
            &[],
            Duration::from_millis(200),
            None,
        )
        .await;
        assert!(matches!(t, Outcome::TimedOut(_)));
    }
}
