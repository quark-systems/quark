//! Test parts: a git repo with real linked worktrees, a fake agent that
//! speaks the file protocol, and a supervisor wired to them.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use quark_core::isolation::ProcessSpec;
use quark_core::session::SessionBackend;
use quark_core::worktree::{
    Holder, LandedWork, ProviderStatus, ReturnOutcome, Worktree, WorktreeProvider, WorktreeRequest,
};
use quark_core::{
    CoreError, EventLog, HostId, Isolation, IsolationMode, ProjectId, Result, TaskId,
};
use quark_harness::ManifestRegistry;
use quark_supervisor::{Assignment, Config, SpawnRequest, Supervisor};

/// A fake agent: prints a prompt, logs every line it reads next to its
/// status file, and acts on a few words.
const AGENT: &str = r#"#!/bin/sh
dir=$(dirname "$QUARK_STATUS_FILE")
echo "fake agent ready"
while IFS= read -r line; do
  clean=$(printf '%s' "$line" | tr -d '\033\r' | sed 's/\[20[01]~//g')
  printf '%s\n' "$clean" >> "$dir/received"
  case "$clean" in
    /exit) exit 0 ;;
    crash*) exit 3 ;;
    ask*) echo "needs-decision [key=k1]: which way?" >> "$QUARK_STATUS_FILE" ;;
    block*) echo "blocked: waiting on a token" >> "$QUARK_STATUS_FILE" ;;
    resume*) echo "working: back at it" >> "$QUARK_STATUS_FILE" ;;
    ship*) echo "done: PR https://github.com/o/r/pull/7 checks green" >> "$QUARK_STATUS_FILE" ;;
    commit*) echo x > file.txt && git add file.txt && git commit -qm x ;;
  esac
done
"#;

pub struct Env {
    pub dir: tempfile::TempDir,
    pub repo: PathBuf,
    pub agent: PathBuf,
    pub manifests: Arc<ManifestRegistry>,
    pub worktrees: Arc<GitWorktrees>,
}

impl Env {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@example.com"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("README"), "hi\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "init"]);
        let agent = dir.path().join("agent.sh");
        std::fs::write(&agent, AGENT).unwrap();
        let manifest = quark_harness::parse(&format!(
            r#"
schema = 1
id = "fake"
name = "Fake agent"
efforts = ["low", "high"]
[detect]
bins = ["sh"]
[launch]
argv = ["sh", "{agent}"]
prompt_via = "paste"
[turn_signals]
busy = "none"
turn_end = "none"
idle_patterns = ["fake agent ready"]
[keys]
interrupt = ["C-c"]
exit = "/exit"
"#,
            agent = agent.display()
        ))
        .unwrap();
        let worktrees = Arc::new(GitWorktrees::new(dir.path().join("pool")));
        Self {
            manifests: Arc::new(ManifestRegistry::with([manifest])),
            dir,
            repo,
            agent,
            worktrees,
        }
    }

    pub fn log_path(&self) -> PathBuf {
        self.dir.path().join("events.db")
    }

    pub fn log(&self) -> Arc<dyn EventLog> {
        Arc::new(quark_eventlog::SqliteEventLog::open(self.log_path()).unwrap())
    }

    pub fn config(&self) -> Config {
        let mut c = Config::new(HostId::from("h"), self.dir.path().join("state"));
        c.ready_timeout = Duration::from_secs(5);
        c.ready_quiet = Duration::from_millis(200);
        c.exit_grace = Duration::from_millis(500);
        c
    }

    pub async fn supervisor(&self, sessions: Arc<dyn SessionBackend>) -> Supervisor {
        self.supervisor_with(sessions, self.config()).await
    }

    pub async fn supervisor_with(
        &self,
        sessions: Arc<dyn SessionBackend>,
        config: Config,
    ) -> Supervisor {
        Supervisor::open(
            self.log(),
            self.manifests.clone(),
            self.worktrees.clone(),
            sessions,
            Arc::new(NativeOnly),
            config,
        )
        .await
        .unwrap()
    }

    pub fn request(&self, task: &str) -> SpawnRequest {
        SpawnRequest {
            project: ProjectId::from("p"),
            task: TaskId::from(task),
            assignment: Assignment {
                title: format!("task {task}"),
                brief: "Fix the parser.".into(),
                repo: self.repo.clone(),
                branch: format!("quark/{task}"),
                base: None,
                harness: "fake".into(),
                model: None,
                effort: Some("high".into()),
                isolation: IsolationMode::Native,
                policy: Default::default(),
                env: BTreeMap::new(),
            },
        }
    }
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Linked worktrees made with `git worktree add`, with the landed-work
/// check on return.
pub struct GitWorktrees {
    root: PathBuf,
    held: Mutex<BTreeMap<String, Worktree>>,
}

impl GitWorktrees {
    pub fn new(root: PathBuf) -> Self {
        std::fs::create_dir_all(&root).unwrap();
        Self {
            root,
            held: Mutex::default(),
        }
    }

    pub fn held(&self) -> usize {
        self.held.lock().unwrap().len()
    }
}

#[async_trait]
impl WorktreeProvider for GitWorktrees {
    async fn get(&self, r: &WorktreeRequest) -> Result<Worktree> {
        let id = format!("wt-{}", r.task);
        let path = self.root.join(&id);
        git(
            &r.repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &r.branch,
                path.to_str().unwrap(),
            ],
        );
        let wt = Worktree {
            id: id.clone(),
            path,
            repo: r.repo.clone(),
            branch: r.branch.clone(),
            holder: Holder::Task {
                task: r.task.clone(),
            },
        };
        self.held.lock().unwrap().insert(id, wt.clone());
        Ok(wt)
    }

    async fn return_worktree(&self, id: &str) -> Result<ReturnOutcome> {
        let wt = self
            .held
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| CoreError::NotFound(id.into()))?;
        let dirty = !git(&wt.path, &["status", "--porcelain"]).trim().is_empty();
        let ahead: u32 = git(&wt.path, &["rev-list", "--count", "main..HEAD"])
            .trim()
            .parse()
            .unwrap();
        let work = LandedWork {
            uncommitted: dirty,
            unpushed_commits: ahead,
        };
        if !work.is_landed() {
            return Ok(ReturnOutcome::Kept { work });
        }
        git(&wt.repo, &["worktree", "remove", wt.path.to_str().unwrap()]);
        self.held.lock().unwrap().remove(id);
        Ok(ReturnOutcome::Returned)
    }

    async fn lease(&self, _r: &WorktreeRequest, _owner: &str) -> Result<Worktree> {
        Err(CoreError::Unsupported("lease".into()))
    }

    async fn status(&self) -> Result<ProviderStatus> {
        Ok(ProviderStatus::default())
    }
}

/// Isolation that runs everything as given.
pub struct NativeOnly;

#[async_trait]
impl Isolation for NativeOnly {
    fn modes(&self) -> Vec<IsolationMode> {
        vec![IsolationMode::Native]
    }

    async fn wrap(&self, spec: &ProcessSpec) -> Result<(Vec<String>, BTreeMap<String, String>)> {
        Ok((spec.argv.clone(), spec.env.clone()))
    }
}

/// Polls `f` until it returns true or a few seconds pass.
pub async fn eventually(what: &str, mut f: impl AsyncFnMut() -> bool) {
    for _ in 0..100 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}

/// Lines the fake agent of `generation` has read.
pub fn received(env: &Env, task: &str, generation: &str) -> Vec<String> {
    let path = env
        .dir
        .path()
        .join("state/p")
        .join(task)
        .join(generation)
        .join("received");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}
