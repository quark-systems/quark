//! A Project's Beads database (<https://beads.gascity.com>): its issues,
//! decision beads and memories, read and written through `bd --json`.
//!
//! Each Project gets one database, in server mode so Quark, the coordinator
//! and every worker can write at once:
//!
//! ```text
//! <home>/beads/<project-id>/
//!   .beads/              the database (`bd init --server`); Dolt runs a sql-server for it
//!   quark-beads.json     what Quark knows about it: prefix, remote, GitHub repo, last sync
//! ```
//!
//! The database belongs to the Project, not to any of its repos: Quark never
//! reads or writes a repo's own `.beads`. Agents reach it through
//! `BEADS_DIR`, which the coordinator is started with and hands on to every
//! worker, and which wins over a `.beads` a repo may track.
//!
//! Nothing syncs with an issue tracker until the Project has a
//! [`SyncRule`] for it; see [`Beads::sync`].
//!
//! Live updates come from Beads' events journal (`bd events tail --follow`):
//! each burst of journal records becomes one `beads.changed` event. The
//! journal does not record syncs or memories, so Quark emits `beads.changed`
//! itself after those.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use quark_systems::{
    BeadsChanged, BeadsMemory, BeadsState, BeadsStatus, DraftIssue, EventType, Issue, IssueComment,
    IssueDetail, IssueRef, MemoryProposalState, Project, RelatedIssue, SyncDirection, SyncRule,
    SyncTracker, TrackerSync, WriteSyncRule,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::task::JoinHandle;

use crate::store::Store;

mod decisions;
pub use decisions::{answer_note, rule_memory, DecisionBead};

/// Quark's notes about a database, next to its `.beads`.
const SIDECAR: &str = "quark-beads.json";

/// Longest a `bd` call may run before it is killed.
const BD_TIMEOUT: Duration = Duration::from_secs(120);

/// How long the journal must be quiet before a burst becomes one event.
const COALESCE: Duration = Duration::from_millis(300);

/// How often a Project's enabled sync rules run on their own.
pub const SYNC_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// Pause before tailing the journal again after `bd` exits.
const RETAIL_DELAY: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum BeadsError {
    /// The Project has no database yet.
    #[error("the Project has no Beads database yet")]
    Missing,
    #[error("{0}")]
    Unavailable(String),
    /// `bd` ran and failed.
    #[error("bd {args}: {message}")]
    Bd { args: String, message: String },
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
}

/// What Quark keeps about a database it set up.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Sidecar {
    prefix: Option<String>,
    #[serde(default)]
    sync_rules: Vec<SyncRule>,
    last_sync: Option<TrackerSync>,
}

/// Beads for every Project of this daemon: where each database lives, the
/// `bd` binary, and the journal tails and sync loops that run per database.
pub struct Beads {
    bd: PathBuf,
    /// The daemon's base URL, for the coordinator to send drafts back to.
    api_base: String,
    /// Setup progress and failures, which have no file to read them from.
    transient: Mutex<HashMap<String, BeadsStatus>>,
    watchers: Mutex<HashMap<String, Vec<JoinHandle<()>>>>,
    /// One sync run or rule change at a time, so neither loses the other's
    /// sidecar write.
    syncing: tokio::sync::Mutex<()>,
    /// Whether a new Project gets its database at once. The daemon turns it
    /// on; tests that build an `AppState` leave it off.
    auto_setup: bool,
}

impl Default for Beads {
    fn default() -> Self {
        Beads::new(
            None,
            format!("http://127.0.0.1:{}", crate::config::DEFAULT_PORT),
        )
    }
}

impl Drop for Beads {
    fn drop(&mut self) {
        for (_, tasks) in self.watchers.get_mut().unwrap().drain() {
            tasks.iter().for_each(JoinHandle::abort);
        }
    }
}

/// The database directory of a Project under Quark's home.
pub fn dir_of(home: &Path, project_id: &str) -> PathBuf {
    home.join("beads").join(project_id)
}

/// `BEADS_DIR=<the Project's .beads>`, for the coordinator's launch; the
/// engine hands it on to every worker the coordinator starts.
pub fn agent_env(home: &Path, project_id: &str) -> (String, String) {
    (
        "BEADS_DIR".into(),
        beads_dir(home, project_id).display().to_string(),
    )
}

/// The `BEADS_DIR` the Project's agents run with: its database's `.beads`,
/// set even before setup finishes, since `bd` reads it only when run.
pub fn beads_dir(home: &Path, project_id: &str) -> PathBuf {
    dir_of(home, project_id).join(".beads")
}

impl Beads {
    /// `bd` from `PATH` unless given; `api_base` is where the daemon listens.
    pub fn new(bd: Option<PathBuf>, api_base: String) -> Self {
        Beads {
            bd: bd.unwrap_or_else(|| PathBuf::from("bd")),
            api_base,
            transient: Mutex::new(HashMap::new()),
            watchers: Mutex::new(HashMap::new()),
            syncing: tokio::sync::Mutex::new(()),
            auto_setup: false,
        }
    }

    /// Sets up a database for each new Project as it is created.
    pub fn with_auto_setup(mut self) -> Self {
        self.auto_setup = true;
        self
    }

    pub fn auto_setup(&self) -> bool {
        self.auto_setup
    }

    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    /// Where the Project's database stands.
    pub async fn status(&self, home: &Path, project_id: &str) -> BeadsStatus {
        if let Some(s) = self.transient.lock().unwrap().get(project_id) {
            return s.clone();
        }
        let dir = dir_of(home, project_id);
        if !dir.join(".beads").join("metadata.json").is_file() {
            let mut s = self.blank(project_id, BeadsState::Missing);
            if let Err(why) = self.installed().await {
                s.state = BeadsState::Unavailable;
                s.detail = Some(why);
            }
            return s;
        }
        let side = read_sidecar(&dir);
        BeadsStatus {
            project_id: project_id.to_string(),
            state: BeadsState::Ready,
            detail: None,
            dir: Some(dir.display().to_string()),
            prefix: side.prefix,
            sync_rules: side.sync_rules,
            last_sync: side.last_sync,
        }
    }

    fn blank(&self, project_id: &str, state: BeadsState) -> BeadsStatus {
        BeadsStatus {
            project_id: project_id.to_string(),
            state,
            detail: None,
            dir: None,
            prefix: None,
            sync_rules: Vec::new(),
            last_sync: None,
        }
    }

    /// Why `bd` or `dolt` cannot run here, if so. Server mode starts a Dolt
    /// sql-server, so both are needed.
    async fn installed(&self) -> Result<(), String> {
        let bd = Command::new(&self.bd).arg("version").output().await;
        if !matches!(bd, Ok(ref o) if o.status.success()) {
            return Err(format!(
                "bd is not installed ({}); install the latest bd from https://beads.gascity.com",
                self.bd.display()
            ));
        }
        let dolt_bin = std::env::var_os("BEADS_DOLT_BIN").unwrap_or_else(|| "dolt".into());
        let dolt = Command::new(&dolt_bin).arg("version").output().await;
        if !matches!(dolt, Ok(ref o) if o.status.success()) {
            return Err(
                "dolt is not installed; Beads needs it to run the Project's database in server mode \
                 (https://docs.dolthub.com/introduction/installation)"
                    .into(),
            );
        }
        Ok(())
    }

    /// Runs `bd <args>` in `dir` and returns its stdout. JSON output is
    /// asked for with `--json` in `args`.
    async fn bd(
        &self,
        dir: &Path,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> Result<String, BeadsError> {
        let mut cmd = Command::new(&self.bd);
        cmd.args(args)
            .current_dir(dir)
            .env("BD_NON_INTERACTIVE", "1")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        if std::env::var_os("BEADS_ACTOR").is_none() {
            cmd.env("BEADS_ACTOR", "quark");
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        let shown = args.join(" ");
        let out = match tokio::time::timeout(BD_TIMEOUT, cmd.output()).await {
            Err(_) => {
                return Err(BeadsError::Bd {
                    args: shown,
                    message: format!("no answer in {}s", BD_TIMEOUT.as_secs()),
                })
            }
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(BeadsError::Unavailable(format!(
                    "bd is not installed ({})",
                    self.bd.display()
                )))
            }
            Ok(Err(e)) => {
                return Err(BeadsError::Bd {
                    args: shown,
                    message: e.to_string(),
                })
            }
            Ok(Ok(out)) => out,
        };
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let text = if err.trim().is_empty() {
                String::from_utf8_lossy(&out.stdout).into_owned()
            } else {
                err.into_owned()
            };
            return Err(BeadsError::Bd {
                args: shown,
                message: tail(&text, 600),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// `bd --json` in the Project's database, parsed.
    async fn json<T: serde::de::DeserializeOwned>(
        &self,
        dir: &Path,
        args: &[&str],
    ) -> Result<T, BeadsError> {
        let mut all = args.to_vec();
        all.push("--json");
        let out = self.bd(dir, &all, &[]).await?;
        serde_json::from_str(&out).map_err(|e| BeadsError::Bd {
            args: args.join(" "),
            message: format!("unreadable output: {e}"),
        })
    }

    fn ready_dir(home: &Path, project_id: &str) -> Result<PathBuf, BeadsError> {
        let dir = dir_of(home, project_id);
        if dir.join(".beads").join("metadata.json").is_file() {
            Ok(dir)
        } else {
            Err(BeadsError::Missing)
        }
    }

    fn set_transient(&self, store: &Store, status: Option<BeadsStatus>, project_id: &str) {
        let mut t = self.transient.lock().unwrap();
        match &status {
            Some(s) => {
                t.insert(project_id.to_string(), s.clone());
            }
            None => {
                t.remove(project_id);
            }
        }
        drop(t);
        if let Some(s) = status {
            publish(store, &s);
        }
    }

    /// Creates the Project's database, or adopts the one its first repo
    /// syncs with, in the background. Returns the status to show now.
    pub async fn start_setup(
        self: &Arc<Self>,
        store: Arc<Store>,
        home: PathBuf,
        project: Project,
    ) -> BeadsStatus {
        let current = self.status(&home, &project.id).await;
        if matches!(
            current.state,
            BeadsState::Ready | BeadsState::SettingUp | BeadsState::Unavailable
        ) {
            return current;
        }
        let mut s = self.blank(&project.id, BeadsState::SettingUp);
        s.detail = Some("Creating the Beads database".into());
        self.set_transient(&store, Some(s.clone()), &project.id);
        let this = self.clone();
        tokio::spawn(async move {
            let pid = project.id.clone();
            match this.setup(&store, &home, &project).await {
                Ok(()) => {
                    this.set_transient(&store, None, &pid);
                    let ready = this.status(&home, &pid).await;
                    publish(&store, &ready);
                    changed(&store, &pid, None, "setup");
                    this.watch(store.clone(), home.clone(), pid);
                }
                Err(e) => {
                    tracing::warn!(project = %pid, error = %e, "setting up Beads failed");
                    let mut failed = this.blank(&pid, BeadsState::Failed);
                    failed.detail = Some(e.to_string());
                    this.set_transient(&store, Some(failed), &pid);
                }
            }
        });
        s
    }

    async fn setup(&self, store: &Store, home: &Path, project: &Project) -> Result<(), BeadsError> {
        self.installed().await.map_err(BeadsError::Unavailable)?;
        let dir = dir_of(home, &project.id);
        let prefix = prefix_for(&project.name);

        // A failed earlier attempt may have left a partial database.
        if dir.exists() && !dir.join(".beads").join("metadata.json").is_file() {
            std::fs::remove_dir_all(&dir).map_err(|e| BeadsError::Invalid(e.to_string()))?;
        }
        std::fs::create_dir_all(&dir).map_err(|e| BeadsError::Invalid(e.to_string()))?;
        let step = |detail: &str| {
            let mut s = self.blank(&project.id, BeadsState::SettingUp);
            s.detail = Some(detail.to_string());
            self.set_transient(store, Some(s), &project.id);
        };

        self.bd(
            &dir,
            &[
                "init",
                "--server",
                "--prefix",
                &prefix,
                "--non-interactive",
                "--skip-agents",
                "--skip-hooks",
                "--quiet",
            ],
            &[],
        )
        .await?;
        step("Turning on the events journal");
        self.bd(&dir, &["config", "set", "events-journal", "true"], &[])
            .await?;
        write_sidecar(
            &dir,
            &Sidecar {
                prefix: Some(prefix),
                ..Sidecar::default()
            },
        )
        .map_err(BeadsError::Invalid)
    }

    /// Sets up a database for every Project that has none, in the
    /// background. Called once at daemon start, so Projects made before
    /// Beads was created with them get one too.
    pub async fn setup_missing(self: &Arc<Self>, store: Arc<Store>, home: PathBuf) {
        let Ok(projects) = store.list_projects() else {
            return;
        };
        for p in projects {
            if self.status(&home, &p.id).await.state == BeadsState::Missing {
                self.start_setup(store.clone(), home.clone(), p).await;
            }
        }
    }

    /// Starts tailing the journal (and running sync rules) for every
    /// Project that has a database. Called once at daemon start.
    pub fn watch_all(self: &Arc<Self>, store: Arc<Store>, home: PathBuf) {
        let Ok(projects) = store.list_projects() else {
            return;
        };
        for p in projects {
            if Self::ready_dir(&home, &p.id).is_ok() {
                self.watch(store.clone(), home.clone(), p.id);
            }
        }
    }

    /// Tails one database's journal and runs its enabled sync rules
    /// periodically,
    /// until the daemon stops.
    fn watch(self: &Arc<Self>, store: Arc<Store>, home: PathBuf, project_id: String) {
        let mut watchers = self.watchers.lock().unwrap();
        if watchers.contains_key(&project_id) {
            return;
        }
        let dir = dir_of(&home, &project_id);
        let tail = tokio::spawn(tail_journal(
            self.bd.clone(),
            dir,
            store.clone(),
            project_id.clone(),
        ));
        let this = Arc::downgrade(self);
        let (h, pid) = (home.clone(), project_id.clone());
        let sync = tokio::spawn(async move {
            loop {
                tokio::time::sleep(SYNC_INTERVAL).await;
                let Some(this) = this.upgrade() else { return };
                let rules = this.status(&h, &pid).await.sync_rules;
                if rules.iter().any(|r| r.enabled) {
                    if let Err(e) = this.sync(&store, &h, &pid).await {
                        tracing::info!(project = %pid, error = %e, "issue sync failed");
                    }
                }
            }
        });
        watchers.insert(project_id, vec![tail, sync]);
    }

    /// Replaces the Project's sync rules. Each rule is checked first, and
    /// a rule kept from before keeps its last outcome.
    pub async fn set_sync_rules(
        &self,
        store: &Store,
        home: &Path,
        project_id: &str,
        rules: Vec<WriteSyncRule>,
    ) -> Result<BeadsStatus, BeadsError> {
        let dir = Self::ready_dir(home, project_id)?;
        let _turn = self.syncing.lock().await;
        let mut side = read_sidecar(&dir);
        side.sync_rules = sync_rules(&side.sync_rules, rules).map_err(BeadsError::Invalid)?;
        write_sidecar(&dir, &side).map_err(BeadsError::Invalid)?;
        let status = self.status(home, project_id).await;
        publish(store, &status);
        Ok(status)
    }

    /// Runs every enabled sync rule once, pulls before pushes, and records
    /// each rule's outcome whether or not it worked. A Project without an
    /// enabled rule has nothing to sync.
    pub async fn sync(
        &self,
        store: &Store,
        home: &Path,
        project_id: &str,
    ) -> Result<BeadsStatus, BeadsError> {
        let dir = Self::ready_dir(home, project_id)?;
        let _turn = self.syncing.lock().await;
        let mut side = read_sidecar(&dir);
        if !side.sync_rules.iter().any(|r| r.enabled) {
            return Err(BeadsError::Invalid(
                "no sync rule is turned on; add one in the Project's settings".into(),
            ));
        }
        let mut messages = Vec::new();
        let mut ok = true;
        for rule in side.sync_rules.iter_mut().filter(|r| r.enabled) {
            let outcome = self.sync_rule(&dir, rule).await;
            ok &= outcome.is_ok();
            let message = match &outcome {
                Ok(m) => m.clone(),
                Err(e) => e.to_string(),
            };
            messages.push(format!("{}: {message}", rule.repository));
            rule.last_sync = Some(TrackerSync {
                at: crate::now_rfc3339(),
                ok: outcome.is_ok(),
                message,
            });
        }
        side.last_sync = Some(TrackerSync {
            at: crate::now_rfc3339(),
            ok,
            message: messages.join("; "),
        });
        write_sidecar(&dir, &side).map_err(BeadsError::Invalid)?;
        let status = self.status(home, project_id).await;
        publish(store, &status);
        changed(store, project_id, None, "sync");
        Ok(status)
    }

    async fn sync_rule(&self, dir: &Path, rule: &SyncRule) -> Result<String, BeadsError> {
        let SyncTracker::Github = rule.tracker;
        let token = github_token().await.ok_or_else(|| {
            BeadsError::Unavailable(
                "no GitHub token: set GITHUB_TOKEN or sign in with `gh auth login`".into(),
            )
        })?;
        let (owner, repo) = rule
            .repository
            .split_once('/')
            .ok_or_else(|| BeadsError::Invalid(format!("{} is not owner/repo", rule.repository)))?;
        // bd reads the repository from its config before the environment;
        // Quark never sets it there, so each rule names its own.
        let env = [
            ("GITHUB_TOKEN", token.as_str()),
            ("GITHUB_OWNER", owner),
            ("GITHUB_REPO", repo),
        ];
        let mut said = Vec::new();
        if matches!(rule.direction, SyncDirection::Pull | SyncDirection::Both) {
            let out = self
                .bd(dir, &["github", "sync", "--pull-only"], &env)
                .await?;
            said.extend(last_line(&out));
        }
        if matches!(rule.direction, SyncDirection::Push | SyncDirection::Both) {
            let issues = self.raw_issues(dir).await?;
            let push = pushed_by(rule, &issues);
            if !push.is_empty() {
                let ids = push.join(",");
                let out = self
                    .bd(
                        dir,
                        &["github", "sync", "--push-only", "--issues", &ids],
                        &env,
                    )
                    .await?;
                said.extend(last_line(&out));
            }
        }
        Ok(if said.is_empty() {
            "nothing to sync".into()
        } else {
            said.join("; ")
        })
    }

    async fn raw_issues(&self, dir: &Path) -> Result<Vec<BdIssue>, BeadsError> {
        self.json(dir, &["list", "--all", "--limit", "0"]).await
    }

    /// Every bead in the Project's database, highest priority first.
    pub async fn issues(&self, home: &Path, project_id: &str) -> Result<Vec<Issue>, BeadsError> {
        let dir = Self::ready_dir(home, project_id)?;
        Ok(to_issues(self.raw_issues(&dir).await?))
    }

    /// One bead with its links and comments.
    pub async fn issue(
        &self,
        home: &Path,
        project_id: &str,
        id: &str,
    ) -> Result<IssueDetail, BeadsError> {
        valid_id(id)?;
        let dir = Self::ready_dir(home, project_id)?;
        let raw = self.raw_issues(&dir).await?;
        let shown: Vec<BdShown> = match self.json(&dir, &["show", "--id", id]).await {
            Ok(s) => s,
            Err(BeadsError::Bd { .. }) if !raw.iter().any(|i| i.id == id) => {
                return Err(BeadsError::NotFound(format!("no issue {id}")))
            }
            Err(e) => return Err(e),
        };
        let comments: Vec<BdComment> = self.json(&dir, &["comments", id]).await.unwrap_or_default();
        let shown = shown.into_iter().next();
        detail(raw, id, shown, comments)
    }

    /// Creates the drafted beads in one transaction. Returns the new ids by
    /// draft key.
    pub async fn create(
        &self,
        home: &Path,
        project_id: &str,
        drafts: &[DraftIssue],
    ) -> Result<BTreeMap<String, String>, BeadsError> {
        let dir = Self::ready_dir(home, project_id)?;
        let plan = graph_plan(drafts)?;
        let file = tempfile::Builder::new()
            .prefix("quark-plan-")
            .suffix(".json")
            .tempfile_in(&dir)
            .map_err(|e| BeadsError::Invalid(e.to_string()))?;
        std::fs::write(file.path(), plan.to_string())
            .map_err(|e| BeadsError::Invalid(e.to_string()))?;
        let path = file.path().to_string_lossy().into_owned();
        let out: GraphResult = self.json(&dir, &["create", "--graph", &path]).await?;
        Ok(out.ids)
    }

    /// The Project's memories, with where each came from when Quark
    /// accepted it.
    pub async fn memories(
        &self,
        store: &Store,
        home: &Path,
        project_id: &str,
    ) -> Result<Vec<BeadsMemory>, BeadsError> {
        let dir = Self::ready_dir(home, project_id)?;
        let raw: BTreeMap<String, serde_json::Value> = self.json(&dir, &["memories"]).await?;
        let accepted = store
            .list_memory_proposals(project_id, Some(MemoryProposalState::Accepted))
            .unwrap_or_default();
        Ok(raw
            .into_iter()
            .filter_map(|(key, v)| Some((key, v.as_str()?.to_string())))
            .map(|(key, value)| {
                let from = accepted
                    .iter()
                    .filter_map(|p| p.entry.as_ref())
                    .find(|e| e.beads_key.as_deref() == Some(&key));
                BeadsMemory {
                    evidence: from.map(|e| e.evidence.clone()),
                    source: from.and_then(|e| e.source),
                    accepted_at: from.and_then(|e| e.accepted_at.clone()),
                    accepted_by: from.and_then(|e| e.accepted_by.clone()),
                    key,
                    value,
                }
            })
            .collect())
    }

    /// Stores `text` as a new memory and returns its key, unique among the
    /// Project's memories.
    pub async fn remember(
        &self,
        store: &Store,
        home: &Path,
        project_id: &str,
        text: &str,
    ) -> Result<String, BeadsError> {
        let dir = Self::ready_dir(home, project_id)?;
        let raw: BTreeMap<String, serde_json::Value> = self.json(&dir, &["memories"]).await?;
        let key = memory_key(text, |k| raw.contains_key(k));
        self.bd(&dir, &["remember", "--key", &key, "--", text], &[])
            .await?;
        changed(store, project_id, None, "memory");
        Ok(key)
    }

    pub async fn forget(
        &self,
        store: &Store,
        home: &Path,
        project_id: &str,
        key: &str,
    ) -> Result<(), BeadsError> {
        if key.is_empty()
            || key.len() > 200
            || key.starts_with('-')
            || key.chars().any(char::is_control)
        {
            return Err(BeadsError::Invalid("not a memory key".into()));
        }
        let dir = Self::ready_dir(home, project_id)?;
        let raw: BTreeMap<String, serde_json::Value> = self.json(&dir, &["memories"]).await?;
        if !raw.contains_key(key) {
            return Err(BeadsError::NotFound(format!("no memory {key}")));
        }
        self.bd(&dir, &["forget", key], &[]).await?;
        changed(store, project_id, None, "memory");
        Ok(())
    }
}

fn publish(store: &Store, status: &BeadsStatus) {
    let payload = serde_json::to_value(status).unwrap_or_default();
    if let Err(e) = store.emit(Some(&status.project_id), EventType::BeadsStatus, payload) {
        tracing::warn!(error = %e, "recording a beads.status event failed");
    }
}

/// Tells the app the Project's memories changed.
pub fn memories_changed(store: &Store, project_id: &str) {
    changed(store, project_id, None, "memory");
}

fn changed(store: &Store, project_id: &str, issue_id: Option<String>, op: &str) {
    let payload = serde_json::to_value(BeadsChanged {
        project_id: project_id.to_string(),
        issue_id,
        op: op.to_string(),
    })
    .unwrap_or_default();
    if let Err(e) = store.emit(Some(project_id), EventType::BeadsChanged, payload) {
        tracing::warn!(error = %e, "recording a beads.changed event failed");
    }
}

/// Follows the journal and turns each burst of records into one
/// `beads.changed` event; starts again after `bd` exits.
async fn tail_journal(bd: PathBuf, dir: PathBuf, store: Arc<Store>, project_id: String) {
    let mut since: i64 = 0;
    loop {
        let child = Command::new(&bd)
            .args(["events", "tail", "--follow", "--json", "--since"])
            .arg(since.to_string())
            .current_dir(&dir)
            .env("BD_NON_INTERACTIVE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn();
        if let Ok(mut child) = child {
            let mut lines = BufReader::new(child.stdout.take().expect("piped")).lines();
            // The first records replay what happened before; they still
            // count, since the app may have missed them.
            let mut pending: Option<Burst> = None;
            loop {
                let next = if pending.is_some() {
                    match tokio::time::timeout(COALESCE, lines.next_line()).await {
                        Ok(l) => l,
                        Err(_) => {
                            flush(&store, &project_id, pending.take());
                            continue;
                        }
                    }
                } else {
                    lines.next_line().await
                };
                let Ok(Some(line)) = next else { break };
                let Ok(rec) = serde_json::from_str::<JournalRecord>(&line) else {
                    continue;
                };
                since = since.max(rec.seq);
                let burst = pending.get_or_insert_with(|| Burst {
                    issue: rec.issue_id.clone(),
                    op: rec.op.clone(),
                    one_issue: true,
                });
                burst.one_issue &= burst.issue == rec.issue_id;
                burst.op = rec.op;
            }
            flush(&store, &project_id, pending.take());
            let _ = child.kill().await;
        }
        tokio::time::sleep(RETAIL_DELAY).await;
    }
}

/// Journal records that arrived together.
struct Burst {
    issue: Option<String>,
    /// The last record's operation.
    op: String,
    /// Every record touched `issue`.
    one_issue: bool,
}

/// One event for a burst: about its issue when it touched only one, else a
/// `batch`.
fn flush(store: &Store, project_id: &str, burst: Option<Burst>) {
    match burst {
        Some(b) if b.one_issue => changed(store, project_id, b.issue, &b.op),
        Some(_) => changed(store, project_id, None, "batch"),
        None => {}
    }
}

#[derive(Debug, Deserialize)]
struct JournalRecord {
    seq: i64,
    op: String,
    issue_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GraphResult {
    ids: BTreeMap<String, String>,
}

/// A bead as `bd list --json` prints it.
#[derive(Debug, Clone, Deserialize)]
struct BdIssue {
    id: String,
    title: String,
    #[serde(default)]
    description: String,
    status: String,
    #[serde(default)]
    priority: u8,
    #[serde(default)]
    issue_type: String,
    #[serde(default)]
    labels: Vec<String>,
    assignee: Option<String>,
    owner: Option<String>,
    created_by: Option<String>,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    updated_at: String,
    closed_at: Option<String>,
    external_ref: Option<String>,
    #[serde(default)]
    dependencies: Vec<BdDep>,
}

#[derive(Debug, Clone, Deserialize)]
struct BdDep {
    depends_on_id: String,
    #[serde(rename = "type")]
    kind: String,
}

/// The fields of `bd show --json` that `bd list` leaves out.
#[derive(Debug, Clone, Default, Deserialize)]
struct BdShown {
    notes: Option<String>,
    design: Option<String>,
    acceptance_criteria: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct BdComment {
    #[serde(default)]
    author: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    created_at: String,
}

const CLOSED: &str = "closed";

/// Ids of the beads each one waits for (`blocks` dependencies).
fn blockers(i: &BdIssue) -> Vec<String> {
    i.dependencies
        .iter()
        .filter(|d| d.kind == "blocks")
        .map(|d| d.depends_on_id.clone())
        .collect()
}

fn to_issues(raw: Vec<BdIssue>) -> Vec<Issue> {
    let open: HashSet<&str> = raw
        .iter()
        .filter(|i| i.status != CLOSED)
        .map(|i| i.id.as_str())
        .collect();
    let mut out: Vec<Issue> = raw
        .iter()
        .map(|i| {
            let blocked_by = blockers(i);
            let blocked =
                i.status != CLOSED && blocked_by.iter().any(|b| open.contains(b.as_str()));
            Issue {
                id: i.id.clone(),
                title: i.title.clone(),
                description: i.description.clone(),
                status: i.status.clone(),
                priority: i.priority,
                issue_type: i.issue_type.clone(),
                labels: i.labels.clone(),
                assignee: i.assignee.clone(),
                owner: i.owner.clone(),
                created_by: i.created_by.clone(),
                created_at: i.created_at.clone(),
                updated_at: i.updated_at.clone(),
                closed_at: i.closed_at.clone(),
                external_ref: i.external_ref.clone(),
                ready: i.status == "open" && !blocked,
                blocked: blocked || i.status == "blocked",
                blocked_by,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        (a.status == CLOSED)
            .cmp(&(b.status == CLOSED))
            .then(a.priority.cmp(&b.priority))
            .then(b.updated_at.cmp(&a.updated_at))
            .then(a.id.cmp(&b.id))
    });
    out
}

fn as_ref(i: &BdIssue) -> IssueRef {
    IssueRef {
        id: i.id.clone(),
        title: i.title.clone(),
        status: i.status.clone(),
        issue_type: i.issue_type.clone(),
    }
}

fn detail(
    raw: Vec<BdIssue>,
    id: &str,
    shown: Option<BdShown>,
    comments: Vec<BdComment>,
) -> Result<IssueDetail, BeadsError> {
    let by_id: HashMap<&str, &BdIssue> = raw.iter().map(|i| (i.id.as_str(), i)).collect();
    let this = *by_id
        .get(id)
        .ok_or_else(|| BeadsError::NotFound(format!("no issue {id}")))?;
    let refer = |other: &str| {
        by_id.get(other).map(|i| as_ref(i)).unwrap_or(IssueRef {
            id: other.to_string(),
            title: String::new(),
            status: String::new(),
            issue_type: String::new(),
        })
    };
    let blocked_by_issues = blockers(this).iter().map(|b| refer(b)).collect();
    let mut related_issues: Vec<RelatedIssue> = this
        .dependencies
        .iter()
        .filter(|d| d.kind != "blocks")
        .map(|d| RelatedIssue {
            issue: refer(&d.depends_on_id),
            kind: if d.kind == "parent-child" {
                "parent".into()
            } else {
                d.kind.clone()
            },
        })
        .collect();
    let mut blocks_issues = Vec::new();
    for other in &raw {
        for d in other.dependencies.iter().filter(|d| d.depends_on_id == id) {
            match d.kind.as_str() {
                "blocks" => blocks_issues.push(as_ref(other)),
                "parent-child" => related_issues.push(RelatedIssue {
                    issue: as_ref(other),
                    kind: "child".into(),
                }),
                "related" => related_issues.push(RelatedIssue {
                    issue: as_ref(other),
                    kind: "related".into(),
                }),
                _ => {}
            }
        }
    }
    let shown = shown.unwrap_or_default();
    let issue = to_issues(raw.clone())
        .into_iter()
        .find(|i| i.id == id)
        .expect("listed above");
    Ok(IssueDetail {
        issue,
        notes: shown.notes.filter(|s| !s.is_empty()),
        design: shown.design.filter(|s| !s.is_empty()),
        acceptance_criteria: shown.acceptance_criteria.filter(|s| !s.is_empty()),
        blocked_by_issues,
        blocks_issues,
        related_issues,
        comments: comments
            .into_iter()
            .map(|c| IssueComment {
                author: c.author,
                text: c.text,
                created_at: c.created_at,
            })
            .collect(),
    })
}

/// Longest title or description accepted from a draft, in bytes.
const MAX_FIELD_BYTES: usize = 64 * 1024;

/// Most beads one draft may create.
pub const MAX_DRAFT_ISSUES: usize = 50;

/// Checks drafted beads before they are stored or created.
pub fn validate_drafts(drafts: &[DraftIssue]) -> Result<(), String> {
    if drafts.len() > MAX_DRAFT_ISSUES {
        return Err(format!("at most {MAX_DRAFT_ISSUES} issues in one draft"));
    }
    let mut keys = HashSet::new();
    for d in drafts {
        if d.key.trim().is_empty() || !keys.insert(d.key.as_str()) {
            return Err(format!(
                "each draft needs its own key; {:?} is empty or repeated",
                d.key
            ));
        }
        if d.title.trim().is_empty() {
            return Err(format!("draft {} has no title", d.key));
        }
        if d.title.len() > 500 || d.description.len() > MAX_FIELD_BYTES {
            return Err(format!("draft {} is too long", d.key));
        }
        if d.priority > 4 {
            return Err(format!("draft {}: priority is 0 to 4", d.key));
        }
        if d.issue_type.trim().is_empty()
            || d.issue_type
                .chars()
                .any(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        {
            return Err(format!(
                "draft {}: {:?} is not an issue type",
                d.key, d.issue_type
            ));
        }
    }
    Ok(())
}

/// `bd create --graph` input for the drafts: a draft's `blocked_by` names
/// another draft by key or an existing bead by id.
fn graph_plan(drafts: &[DraftIssue]) -> Result<serde_json::Value, BeadsError> {
    validate_drafts(drafts).map_err(BeadsError::Invalid)?;
    if drafts.is_empty() {
        return Err(BeadsError::Invalid(
            "the draft has no issues to create".into(),
        ));
    }
    let nodes: Vec<serde_json::Value> = drafts
        .iter()
        .map(|d| {
            let deps: Vec<serde_json::Value> = d
                .blocked_by
                .iter()
                .map(|t| serde_json::json!({ "type": "blocks", "target": t }))
                .collect();
            let mut node = serde_json::json!({
                "key": d.key,
                "title": d.title.trim(),
                "type": d.issue_type,
                "priority": d.priority,
                "labels": d.labels,
                "deps": deps,
            });
            if !d.description.trim().is_empty() {
                node["description"] = d.description.trim().into();
            }
            node
        })
        .collect();
    Ok(serde_json::json!({
        "commit_message": "quark: create issues from a New issue draft",
        "nodes": nodes,
    }))
}

fn valid_id(id: &str) -> Result<(), BeadsError> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(BeadsError::NotFound(format!("no issue {id}")));
    }
    Ok(())
}

/// A memory key from the first words of `text`, made unique with a number
/// when `taken`.
pub fn memory_key(text: &str, taken: impl Fn(&str) -> bool) -> String {
    let words: Vec<String> = text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(6)
        .map(str::to_ascii_lowercase)
        .collect();
    let mut base = words.join("-");
    base.truncate(48);
    let base = base.trim_matches('-').to_string();
    let base = if base.is_empty() {
        "memory".to_string()
    } else {
        base
    };
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|k| !taken(k))
        .expect("an unused key exists")
}

/// An issue prefix from the Project's name: its first word, lower case.
pub fn prefix_for(name: &str) -> String {
    let p: String = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .find(|w| !w.is_empty())
        .unwrap_or("q")
        .to_ascii_lowercase()
        .chars()
        .take(12)
        .collect();
    p
}

/// The issues `rule` pushes: those that came from its repository, and
/// those made in Beads that carry its label. Decisions, and the gates
/// holding them, stay in Quark.
fn pushed_by<'a>(rule: &SyncRule, issues: &'a [BdIssue]) -> Vec<&'a str> {
    let home = format!("https://github.com/{}/issues/", rule.repository);
    issues
        .iter()
        .filter(|i| i.issue_type != "decision" && i.issue_type != "gate")
        .filter(|i| match &i.external_ref {
            Some(r) if !r.is_empty() => r.starts_with(&home),
            _ => i.labels.iter().any(|l| l == &rule.label),
        })
        .map(|i| i.id.as_str())
        .collect()
}

/// The rule list `wanted` asks for, checked, with each kept rule's last
/// outcome carried over from `current`.
fn sync_rules(current: &[SyncRule], wanted: Vec<WriteSyncRule>) -> Result<Vec<SyncRule>, String> {
    let mut out: Vec<SyncRule> = Vec::new();
    for w in wanted {
        let repository = w.repository.trim().trim_end_matches(".git").to_string();
        let name = match repository.split_once('/') {
            Some((o, r)) if valid_part(o) && valid_part(r) => r.to_string(),
            _ => {
                return Err(format!(
                    "{:?} is not a GitHub repository as owner/repo",
                    w.repository
                ))
            }
        };
        let label = w
            .label
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| format!("repo:{name}"));
        if label.contains(',') || label.chars().any(char::is_control) {
            return Err(format!("the label {label:?} cannot hold a comma"));
        }
        if out
            .iter()
            .any(|r| r.repository.eq_ignore_ascii_case(&repository))
        {
            return Err(format!("{repository} has two rules; keep one"));
        }
        let kept = match &w.id {
            Some(id) => Some(
                current
                    .iter()
                    .find(|r| &r.id == id)
                    .ok_or_else(|| format!("no sync rule {id}"))?,
            ),
            None => None,
        };
        out.push(SyncRule {
            id: kept.map_or_else(
                || format!("sync_{}", uuid::Uuid::now_v7().simple()),
                |r| r.id.clone(),
            ),
            tracker: w.tracker,
            repository,
            direction: w.direction,
            label,
            enabled: w.enabled,
            last_sync: kept.and_then(|r| r.last_sync.clone()),
        });
    }
    Ok(out)
}

fn valid_part(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn read_sidecar(dir: &Path) -> Sidecar {
    std::fs::read_to_string(dir.join(SIDECAR))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_sidecar(dir: &Path, side: &Sidecar) -> Result<(), String> {
    let tmp = dir.join(format!("{SIDECAR}.tmp"));
    let body = serde_json::to_string_pretty(side).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join(SIDECAR)).map_err(|e| e.to_string())
}

/// `GITHUB_TOKEN`, else the token `gh` is signed in with.
async fn github_token() -> Option<String> {
    if let Ok(t) = std::env::var("GITHUB_TOKEN") {
        if !t.trim().is_empty() {
            return Some(t.trim().to_string());
        }
    }
    let out = Command::new("gh")
        .args(["auth", "token"])
        .stdin(Stdio::null())
        .output()
        .await
        .ok()?;
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !t.is_empty()).then_some(t)
}

fn last_line(s: &str) -> Option<String> {
    s.lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(str::to_string)
}

/// The last `max` bytes of `s`, trimmed, on a character boundary.
fn tail(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &s[start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bd(id: &str, status: &str, deps: &[(&str, &str)]) -> BdIssue {
        BdIssue {
            id: id.into(),
            title: format!("title {id}"),
            description: String::new(),
            status: status.into(),
            priority: 2,
            issue_type: "task".into(),
            labels: vec![],
            assignee: None,
            owner: None,
            created_by: None,
            created_at: String::new(),
            updated_at: String::new(),
            closed_at: None,
            external_ref: None,
            dependencies: deps
                .iter()
                .map(|(t, k)| BdDep {
                    depends_on_id: t.to_string(),
                    kind: k.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn an_issue_is_blocked_only_while_a_blocker_is_open() {
        let issues = to_issues(vec![
            bd("q-1", "open", &[("q-2", "blocks"), ("q-3", "related")]),
            bd("q-2", "open", &[]),
            bd("q-3", "open", &[]),
            bd("q-4", "open", &[("q-5", "blocks")]),
            bd("q-5", "closed", &[]),
        ]);
        let get = |id: &str| issues.iter().find(|i| i.id == id).unwrap();
        assert!(get("q-1").blocked && !get("q-1").ready);
        assert_eq!(get("q-1").blocked_by, vec!["q-2"]);
        assert!(get("q-2").ready);
        assert!(get("q-4").ready && !get("q-4").blocked);
        assert!(!get("q-5").ready, "closed is never ready");
        assert_eq!(issues.last().unwrap().id, "q-5", "closed issues come last");
    }

    #[test]
    fn detail_links_both_ways() {
        let raw = vec![
            bd("q-1", "open", &[("q-2", "blocks"), ("q-3", "related")]),
            bd("q-2", "open", &[]),
            bd("q-3", "open", &[]),
            bd("q-4", "open", &[("q-2", "parent-child")]),
        ];
        let d = detail(raw, "q-2", None, vec![]).unwrap();
        assert_eq!(
            d.blocks_issues.iter().map(|i| &i.id).collect::<Vec<_>>(),
            ["q-1"]
        );
        assert_eq!(d.related_issues.len(), 1);
        assert_eq!(d.related_issues[0].kind, "child");
        assert_eq!(d.related_issues[0].issue.id, "q-4");
        let one = detail(
            vec![
                bd("q-1", "open", &[("q-2", "blocks"), ("q-3", "related")]),
                bd("q-2", "open", &[]),
            ],
            "q-1",
            None,
            vec![],
        )
        .unwrap();
        assert_eq!(one.blocked_by_issues[0].title, "title q-2");
        assert_eq!(one.related_issues[0].issue.id, "q-3");
        assert_eq!(
            one.related_issues[0].issue.title, "",
            "an unknown bead keeps its id"
        );
        assert!(matches!(
            detail(vec![], "q-9", None, vec![]),
            Err(BeadsError::NotFound(_))
        ));
    }

    fn draft(key: &str, blocked_by: &[&str]) -> DraftIssue {
        DraftIssue {
            key: key.into(),
            title: format!("Draft {key}"),
            issue_type: "bug".into(),
            priority: 1,
            labels: vec!["attention".into()],
            description: String::new(),
            blocked_by: blocked_by.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn a_plan_links_drafts_and_existing_beads() {
        let plan = graph_plan(&[draft("1", &[]), draft("2", &["1", "qk-41"])]).unwrap();
        let nodes = plan["nodes"].as_array().unwrap();
        assert_eq!(
            nodes[1]["deps"][0],
            serde_json::json!({"type": "blocks", "target": "1"})
        );
        assert_eq!(nodes[1]["deps"][1]["target"], "qk-41");
        assert!(nodes[0].get("description").is_none());
        assert!(graph_plan(&[]).is_err());
        assert!(graph_plan(&[draft("1", &[]), draft("1", &[])]).is_err());
        let mut bad = draft("1", &[]);
        bad.issue_type = "bug; rm".into();
        assert!(graph_plan(&[bad]).is_err());
    }

    #[test]
    fn memory_keys_come_from_the_text_and_never_collide() {
        assert_eq!(
            memory_key(
                "tmux misses pane exits; `run-shell true` reaps them",
                |_| false
            ),
            "tmux-misses-pane-exits-run-shell"
        );
        assert_eq!(memory_key("!!!", |_| false), "memory");
        assert_eq!(
            memory_key("Run tests", |k| k == "run-tests" || k == "run-tests-2"),
            "run-tests-3"
        );
    }

    #[test]
    fn prefixes_come_from_the_project_name() {
        assert_eq!(prefix_for("Quark MVP"), "quark");
    }

    fn rule(repository: &str, direction: SyncDirection) -> WriteSyncRule {
        WriteSyncRule {
            id: None,
            tracker: SyncTracker::Github,
            repository: repository.into(),
            direction,
            label: None,
            enabled: true,
        }
    }

    #[test]
    fn a_rule_pushes_its_own_issues_and_never_decisions() {
        let rules = sync_rules(&[], vec![rule("o/web", SyncDirection::Both)]).unwrap();
        let r = &rules[0];
        assert_eq!(r.label, "repo:web");
        let mut from_web = bd("p-1", "open", &[]);
        from_web.external_ref = Some("https://github.com/o/web/issues/4".into());
        let mut from_api = bd("p-2", "open", &[]);
        from_api.external_ref = Some("https://github.com/o/api/issues/4".into());
        let mut labelled = bd("p-3", "open", &[]);
        labelled.labels = vec!["repo:web".into()];
        let unlabelled = bd("p-4", "open", &[]);
        let mut decision = bd("p-5", "open", &[]);
        decision.issue_type = "decision".into();
        decision.labels = vec!["repo:web".into()];
        let issues = [from_web, from_api, labelled, unlabelled, decision];
        assert_eq!(pushed_by(r, &issues), ["p-1", "p-3"]);
    }

    #[test]
    fn rules_are_checked_and_keep_their_last_outcome() {
        assert!(sync_rules(&[], vec![rule("not a repo", SyncDirection::Pull)]).is_err());
        assert!(sync_rules(
            &[],
            vec![
                rule("o/a", SyncDirection::Pull),
                rule("O/A", SyncDirection::Push)
            ]
        )
        .is_err());
        let mut first = sync_rules(&[], vec![rule("o/a.git", SyncDirection::Pull)]).unwrap();
        assert_eq!(first[0].repository, "o/a");
        first[0].last_sync = Some(TrackerSync {
            at: "t".into(),
            ok: true,
            message: "m".into(),
        });
        let mut again = rule("o/a", SyncDirection::Both);
        again.id = Some(first[0].id.clone());
        again.label = Some("web".into());
        again.enabled = false;
        let next = sync_rules(&first, vec![again]).unwrap();
        assert_eq!(next[0].id, first[0].id);
        assert_eq!(next[0].label, "web");
        assert!(!next[0].enabled);
        assert_eq!(next[0].last_sync, first[0].last_sync);
        let mut unknown = rule("o/b", SyncDirection::Pull);
        unknown.id = Some("sync_x".into());
        assert!(sync_rules(&first, vec![unknown]).is_err());
    }
}
