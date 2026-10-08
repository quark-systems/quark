//! [`EngineAdapter`] over a firstmate home, built on the `quark-engine`
//! readers and its allowlisted writer.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use quark_engine::gates;
use quark_systems::{
    AgentConfig, ArtifactKind, DecisionBrief, DeliveryPolicy, DispatchCandidate, DispatchChoice,
    DispatchRule, DispatchStatus, Evidence, EvidenceArtifact, Gate, GateCase, GateKind, GateState,
    MergeMethod, TaskKind, TaskState,
};

use quark_engine::dispatch::Resolution;
use quark_engine::holds::decisions;
use quark_engine::runner::CallLog;
use quark_engine::snapshot::{BacklogState, FleetSnapshot as FmSnapshot, Task};
use quark_engine::status::StatusTail as FmTail;
use quark_engine::write::{self, DeliveryMode, WriteOp};
use quark_engine::{EngineReader, EngineWriter, Error, Workspace};

use super::{
    EngineAdapter, EngineError, EngineResolution, EngineSpawn, EngineTask, FleetSnapshot, Hold,
    SourceRepo, StatusEntry, StatusTail, TaskControl, WorkspacePlan, WorkspaceRef,
};

/// Bound on one dispatch resolution: the classifier request, one quota
/// snapshot and any local quota reading.
const RESOLVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Reads firstmate homes with scripts from one pinned engine checkout.
/// `WorkspaceRef::root` is the home (`FM_HOME`).
pub struct FirstmateEngine {
    engine_root: PathBuf,
    log: Arc<dyn CallLog>,
    tmux: Option<String>,
    /// Coordinators are tasks of the one command-center home, whose
    /// `fm-spawn.sh` refuses to run beside another spawn there; provisioning
    /// and coordinator recovery take turns instead.
    coordinator_spawns: Arc<tokio::sync::Mutex<()>>,
}

impl FirstmateEngine {
    pub fn new(engine_root: impl Into<PathBuf>, log: Arc<dyn CallLog>) -> Self {
        Self {
            engine_root: engine_root.into(),
            log,
            tmux: None,
            coordinator_spawns: Arc::default(),
        }
    }

    /// Runs every engine script with `TMUX` set to `value`, so the engine
    /// opens and finds windows on quarkd's tmux server rather than the
    /// user's own.
    pub fn with_tmux(mut self, value: Option<String>) -> Self {
        self.tmux = value;
        self
    }

    fn at(&self, home: &Path) -> Workspace {
        let ws = Workspace::new(home, &self.engine_root);
        match &self.tmux {
            Some(t) => ws.with_env("TMUX", t.clone()),
            None => ws,
        }
    }

    fn reader(&self, ws: &WorkspaceRef) -> Result<EngineReader, EngineError> {
        Ok(EngineReader::new(self.workspace(ws)?, self.log.clone()))
    }

    fn workspace(&self, ws: &WorkspaceRef) -> Result<Workspace, EngineError> {
        if !ws.root.is_dir() {
            return Err(EngineError::WorkspaceNotFound(ws.root.clone()));
        }
        Ok(self.at(&ws.root))
    }

    async fn write(&self, ws: &WorkspaceRef, op: WriteOp) -> Result<(), EngineError> {
        self.write_at(&ws.root, op).await.map(drop)
    }

    /// Run `op` with `FM_HOME` at `home` and return its stdout.
    async fn write_at(&self, home: &Path, op: WriteOp) -> Result<String, EngineError> {
        self.write_env(home, op, &[]).await
    }

    /// [`Self::write_at`] with account variables in the script's
    /// environment, which `fm-spawn.sh` carries onto the agent's launch.
    async fn write_env(
        &self,
        home: &Path,
        op: WriteOp,
        account_env: &[(String, String)],
    ) -> Result<String, EngineError> {
        if !home.is_dir() {
            return Err(EngineError::WorkspaceNotFound(home.to_path_buf()));
        }
        let mut ws = self.at(home);
        for (key, value) in account_env {
            check_account_env(key, value)?;
            ws = ws.with_env(key.clone(), value.clone());
        }
        let writer = EngineWriter::new(ws, self.log.clone());
        blocking(move || writer.write(&op)).await
    }

    async fn read_snapshot(&self, ws: &WorkspaceRef) -> Result<FmSnapshot, EngineError> {
        let reader = self.reader(ws)?;
        blocking(move || reader.fleet_snapshot()).await
    }
}

#[async_trait]
impl EngineAdapter for FirstmateEngine {
    fn name(&self) -> &'static str {
        "firstmate"
    }

    async fn snapshot(&self, ws: &WorkspaceRef) -> Result<FleetSnapshot, EngineError> {
        Ok(neutral_snapshot(&self.read_snapshot(ws).await?))
    }

    async fn status_tail(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
        offset: u64,
    ) -> Result<StatusTail, EngineError> {
        let path = self
            .reader(ws)?
            .workspace()
            .status_log_path(task_id)
            .map_err(convert)?;
        blocking(move || {
            let mut tail = FmTail::resume(path, offset);
            let lines = tail.read_new()?;
            Ok(StatusTail {
                entries: lines
                    .into_iter()
                    .map(|l| StatusEntry {
                        decision_key: l.event.key.fold_key().map(str::to_string),
                        kind: l.event.verb,
                        note: l.event.note,
                        raw: l.event.raw,
                    })
                    .collect(),
                next_offset: tail.offset(),
            })
        })
        .await
    }

    async fn holds(&self, ws: &WorkspaceRef) -> Result<Vec<Hold>, EngineError> {
        Ok(neutral_holds(&self.read_snapshot(ws).await?))
    }

    async fn send_message(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
        text: &str,
    ) -> Result<(), EngineError> {
        let op = WriteOp::Send {
            task_id: task_id.into(),
            text: text.into(),
        };
        self.write(ws, op).await
    }

    async fn inbox_note(&self, ws: &WorkspaceRef, text: &str) -> Result<(), EngineError> {
        self.write(ws, WriteOp::InboxNote { text: text.into() })
            .await
    }

    /// Open keyed decisions (`<task>:<key>`) are answered through the task's
    /// inbox, which closes the key in the same act. Captain holds (a backlog
    /// task id) are answered with the engine's hold record: a held work item
    /// (ship or scout) is released to resume, any other held task is closed.
    async fn answer(
        &self,
        ws: &WorkspaceRef,
        hold_id: &str,
        answer: &str,
        answered_by: &str,
    ) -> Result<(), EngineError> {
        if let Some((task_id, key)) = hold_id.split_once(':') {
            let op = WriteOp::Answer {
                task_id: task_id.into(),
                key: key.into(),
                text: answer.into(),
                answered_by: answered_by.into(),
            };
            return self.write(ws, op).await;
        }
        write::check_hold_answer(answer).map_err(EngineError::Invalid)?;
        let snapshot = self.read_snapshot(ws).await?;
        let held = snapshot
            .backlog_items()
            .find(|r| r.id.as_deref() == Some(hold_id) && r.captain_actionable)
            .ok_or_else(|| EngineError::TaskNotFound(hold_id.to_string()))?;
        let release = task_kind(held.kind.as_deref()).is_some();
        let mut file = tempfile::Builder::new()
            .prefix("quark-answer-")
            .tempfile()?;
        std::io::Write::write_all(&mut file, answer.as_bytes())?;
        let op = WriteOp::AnswerHold {
            task_id: hold_id.to_string(),
            decision_file: file.path().to_path_buf(),
            release,
            answered_by: answered_by.into(),
        };
        // The file is removed when `file` drops, after the script has run.
        let res = self.write(ws, op).await;
        drop(file);
        res
    }

    /// Read from the task's metadata record. Secondmates are workspaces,
    /// not workers, so they have no spawn here.
    async fn spawn(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
    ) -> Result<Option<EngineSpawn>, EngineError> {
        let reader = self.reader(ws)?;
        let task = task_id.to_string();
        let meta = blocking(move || reader.spawn_meta(&task)).await?;
        Ok(meta
            .filter(|m| m.kind.as_deref() != Some("secondmate"))
            .map(|m| EngineSpawn {
                spawned_at: m.spawned_at(),
                project: m.project_name().map(str::to_string),
                generation: m.generation,
                harness: m.harness,
                model: m.model,
                effort: m.effort,
            }))
    }

    /// `fm-dispatch-resolve.sh` on the task's brief, the resolution the
    /// engine's own intake runs before a spawn.
    async fn resolve_dispatch(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
        project: Option<&str>,
    ) -> Result<Option<EngineResolution>, EngineError> {
        let reader = self.reader(ws)?.with_timeout(RESOLVE_TIMEOUT);
        let task = task_id.to_string();
        let project = project.map(str::to_string);
        let r = blocking(move || reader.dispatch_resolve(&task, project.as_deref())).await?;
        Ok(r.map(neutral_resolution))
    }

    /// `fm-dispatch-resolve.sh` on `description` written to a temporary
    /// brief, which is removed when the script returns.
    async fn resolve_description(
        &self,
        ws: &WorkspaceRef,
        description: &str,
    ) -> Result<Option<EngineResolution>, EngineError> {
        let reader = self.reader(ws)?.with_timeout(RESOLVE_TIMEOUT);
        let description = description.to_string();
        let r = blocking(move || {
            let io = |source| Error::Io {
                path: std::env::temp_dir(),
                source,
            };
            let mut brief = tempfile::Builder::new()
                .prefix("quark-dispatch-test-")
                .suffix(".md")
                .tempfile()
                .map_err(io)?;
            brief.write_all(description.as_bytes()).map_err(io)?;
            brief.flush().map_err(io)?;
            reader.dispatch_resolve_file(brief.path(), None)
        })
        .await?;
        Ok(Some(neutral_resolution(r)))
    }

    async fn set_gates(&self, ws: &WorkspaceRef, config: &str) -> Result<(), EngineError> {
        let op = WriteOp::GatesConfig {
            json: config.to_string(),
        };
        self.write(ws, op).await
    }

    /// `fm-crew-dispatch.sh config-set` exits 1 when the config is invalid,
    /// leaving the old file in place; that is a refusal, not a failed run.
    async fn set_crew_dispatch(&self, ws: &WorkspaceRef, config: &str) -> Result<(), EngineError> {
        let op = WriteOp::CrewDispatchConfig {
            json: config.to_string(),
        };
        if !ws.root.is_dir() {
            return Err(EngineError::WorkspaceNotFound(ws.root.clone()));
        }
        let writer = EngineWriter::new(self.at(&ws.root), self.log.clone());
        let refused = blocking(move || match writer.write(&op) {
            Ok(_) => Ok(None),
            Err(Error::ScriptFailed {
                exit_code: Some(1),
                stderr,
                ..
            }) => Ok(Some(stderr)),
            Err(e) => Err(e),
        })
        .await?;
        match refused {
            None => Ok(()),
            Some(stderr) => {
                let reason = stderr.trim();
                let reason = reason.strip_prefix("error: ").unwrap_or(reason);
                Err(EngineError::Invalid(format!(
                    "crew dispatch config refused: {reason}"
                )))
            }
        }
    }

    /// Cancel is `exit`, never teardown: the worktree and its changes stay.
    async fn control(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
        action: &TaskControl,
    ) -> Result<(), EngineError> {
        let task_id = task_id.to_string();
        let (op, account_env) = match action.clone() {
            TaskControl::Cancel => (WriteOp::Exit { task_id }, Vec::new()),
            TaskControl::Relaunch {
                harness,
                model,
                effort,
                note,
                account_env,
            } => (
                WriteOp::Relaunch {
                    task_id,
                    harness,
                    model,
                    effort,
                    note,
                },
                account_env,
            ),
        };
        self.write_env(&ws.root, op, &account_env).await.map(drop)
    }

    fn account_envs(&self) -> &'static [&'static str] {
        FORWARDED_ACCOUNT_ENVS
    }

    async fn gate_evidence(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
    ) -> Result<Option<Evidence>, EngineError> {
        let reader = self.reader(ws)?;
        let task = task_id.to_string();
        blocking(move || {
            let Some(m) = reader.gates(&task)? else {
                return Ok(None);
            };
            let dir = reader.workspace().gates_dir(&task)?;
            Ok(Some(neutral_evidence(&m, &dir)))
        })
        .await
    }

    fn gate_artifact(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
        path: &str,
    ) -> Result<PathBuf, EngineError> {
        if !gates::safe_relative(path) {
            return Err(EngineError::Invalid(format!("no gate artifact {path:?}")));
        }
        let reader = self.reader(ws)?;
        let m = reader
            .gates(task_id)
            .map_err(convert)?
            .ok_or_else(|| EngineError::TaskNotFound(task_id.into()))?;
        let dir = reader.workspace().gates_dir(task_id).map_err(convert)?;
        super::listed_artifact(&neutral_evidence(&m, &dir), path)?;
        Ok(dir.join(path))
    }

    async fn merge_pull_request(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
        url: &str,
        method: Option<MergeMethod>,
    ) -> Result<(), EngineError> {
        let op = WriteOp::PrMerge {
            task_id: task_id.into(),
            url: url.into(),
            method: method.map(|m| match m {
                MergeMethod::Squash => write::MergeMethod::Squash,
                MergeMethod::Merge => write::MergeMethod::Merge,
                MergeMethod::Rebase => write::MergeMethod::Rebase,
            }),
        };
        self.write(ws, op).await
    }

    /// Standing approval is the registry's yolo posture for each repo, so
    /// the coordinator merges green work itself instead of asking.
    async fn set_standing_approval(
        &self,
        ws: &WorkspaceRef,
        repos: &[String],
        on: bool,
    ) -> Result<(), EngineError> {
        for name in repos {
            let op = WriteOp::ProjectYolo {
                name: name.clone(),
                on,
            };
            let out = self.write_at(&ws.root, op).await?;
            if write::parse_project_yolo(&out).map_err(convert)? != on {
                return Err(EngineError::Command(format!(
                    "{} did not record yolo={} for {name}",
                    write::PROJECT_YOLO,
                    if on { "on" } else { "off" }
                )));
            }
        }
        Ok(())
    }

    /// `state/` holds task records and status logs; `data/` holds the backlog
    /// that queued tasks come from.
    fn watch_dirs(&self, ws: &WorkspaceRef) -> Vec<PathBuf> {
        vec![ws.root.join("state"), ws.root.join("data")]
    }

    fn is_task_change(&self, path: &Path) -> bool {
        is_task_file(path)
    }

    async fn add_source(
        &self,
        command: &Path,
        source: &SourceRepo,
        delivery: DeliveryPolicy,
    ) -> Result<(), EngineError> {
        let op = WriteOp::ProjectAdd {
            name: source.name.clone(),
            origin: source.url.clone(),
            mode: delivery_mode(delivery),
            description: format!("{} (added by Quark)", source.url),
        };
        let out = self.write_at(command, op).await?;
        write::parse_project_added(&out).map_err(convert)?;
        Ok(())
    }

    /// Seeds the Project workspace as a local secondmate of the command
    /// center, keyed by the Project id.
    async fn seed_workspace(
        &self,
        command: &Path,
        plan: &WorkspacePlan,
    ) -> Result<PathBuf, EngineError> {
        let (charter, scope) = charter(plan);
        let op = WriteOp::HomeSeed {
            id: plan.project_id.clone(),
            home: plan.root.clone(),
            projects: plan.sources.iter().map(|s| s.name.clone()).collect(),
            charter,
            scope,
        };
        let out = self.write_at(command, op).await?;
        write::parse_seeded_home(&out).map_err(convert)
    }

    async fn start_coordinator(
        &self,
        command: &Path,
        ws: &WorkspaceRef,
        agent: &AgentConfig,
        account_env: &[(String, String)],
        resume: bool,
    ) -> Result<(), EngineError> {
        let op = WriteOp::SpawnSecondmate {
            id: ws.project_id.clone(),
            home: ws.root.clone(),
            harness: engine_harness(&agent.harness).to_string(),
            model: agent.model.clone(),
            effort: agent.effort.clone(),
            resume,
        };
        let _turn = self.coordinator_spawns.lock().await;
        let out = self.write_env(command, op, account_env).await?;
        write::parse_spawned(&out).map_err(convert)?;
        Ok(())
    }

    /// Coordinators are the command center's secondmates, keyed by Project
    /// id, so their windows come from its metadata and survive restarts.
    async fn coordinator_terminals(
        &self,
        command: &Path,
    ) -> Result<HashMap<String, String>, EngineError> {
        if !command.is_dir() {
            return Ok(HashMap::new());
        }
        let ws = WorkspaceRef {
            project_id: String::new(),
            root: command.to_path_buf(),
        };
        Ok(coordinator_targets(&self.read_snapshot(&ws).await?))
    }
}

/// Account variables `fm-spawn.sh` carries onto an agent's launch and onto
/// `fm-control.sh <task> relaunch`: an ambient `CLAUDE_CONFIG_DIR` to Claude
/// Code and `CODEX_HOME` to Codex. The other harnesses' variables do not
/// reach the agent's pane yet.
const FORWARDED_ACCOUNT_ENVS: &[&str] = &["CLAUDE_CONFIG_DIR", "CODEX_HOME"];

/// Only a forwarded account variable, set to an absolute directory path,
/// reaches an engine script.
fn check_account_env(key: &str, value: &str) -> Result<(), EngineError> {
    if !FORWARDED_ACCOUNT_ENVS.contains(&key) {
        return Err(EngineError::Invalid(format!(
            "the engine cannot launch an agent under {key}"
        )));
    }
    if !Path::new(value).is_absolute() || value.chars().any(char::is_control) {
        return Err(EngineError::Invalid(format!(
            "{key} must be an absolute path, got {value:?}"
        )));
    }
    Ok(())
}

/// A dispatch resolution in neutral names. A status word the engine adds
/// later reads as an error naming it, never as a clear result.
pub fn neutral_resolution(r: Resolution) -> EngineResolution {
    let classifier_consulted = r.classifier_consulted();
    let (status, reason) = match r.status.as_str() {
        "clear" => (DispatchStatus::Clear, r.reason),
        "ambiguous" => (DispatchStatus::Ambiguous, r.reason),
        "escalate" => (DispatchStatus::Escalate, r.reason),
        "error" => (DispatchStatus::Error, r.reason),
        "off" => (DispatchStatus::Off, r.reason),
        other => (
            DispatchStatus::Error,
            Some(format!("unrecognized resolution status {other:?}")),
        ),
    };
    EngineResolution {
        status,
        rule: r.rule.map(|id| DispatchRule {
            id,
            when: r.rule_when,
        }),
        reason,
        notes: r.notes,
        candidates: r
            .candidates
            .into_iter()
            .map(|c| DispatchCandidate {
                harness: c.harness,
                model: c.model,
                passed: c.eligible,
                reason: c.reason,
                evidence: c.evidence,
            })
            .collect(),
        profile: r.profile.map(|p| DispatchChoice {
            harness: p.harness,
            model: p.model,
            effort: p.effort,
            account: None,
        }),
        fallback: r.fallback,
        classifier_consulted,
        classifier_model: r.model,
        confidence: r.confidence,
        output: Some(r.raw).filter(|o| !o.is_empty()),
    }
}

/// Gate results in neutral names. Artifact ids are their relative paths; the
/// daemon assigns public ids and URLs.
pub fn neutral_evidence(m: &gates::GateManifest, dir: &Path) -> Evidence {
    let state = |s: gates::State| match s {
        gates::State::Pending => GateState::Pending,
        gates::State::Running => GateState::Running,
        gates::State::Passed => GateState::Passed,
        gates::State::Failed => GateState::Failed,
        gates::State::Skipped => GateState::Skipped,
    };
    Evidence {
        head_sha: m.head_sha.clone(),
        state: state(m.state),
        stale: false,
        started_at: m.started_at.clone(),
        completed_at: m.completed_at.clone(),
        gates: m
            .gates
            .iter()
            .map(|g| Gate {
                kind: match g.kind {
                    gates::Kind::Checks => GateKind::Checks,
                    gates::Kind::Journeys => GateKind::Journeys,
                    gates::Kind::Holdout => GateKind::Holdout,
                },
                state: state(g.state),
                summary: g.summary.clone(),
                started_at: g.started_at.clone(),
                completed_at: g.completed_at.clone(),
                cases: g
                    .cases
                    .iter()
                    .map(|c| GateCase {
                        name: c.name.clone(),
                        state: state(c.state),
                        duration_ms: c.duration_ms,
                        message: c.message.clone(),
                        artifacts: c
                            .artifacts
                            .iter()
                            .map(|a| EvidenceArtifact {
                                id: a.path.clone(),
                                kind: match a.kind {
                                    gates::ArtifactKind::Trace => ArtifactKind::Trace,
                                    gates::ArtifactKind::Screenshot => ArtifactKind::Screenshot,
                                    gates::ArtifactKind::Video => ArtifactKind::Video,
                                    gates::ArtifactKind::Log => ArtifactKind::Log,
                                    gates::ArtifactKind::Report => ArtifactKind::Report,
                                },
                                name: Path::new(&a.path)
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default(),
                                content_type: a
                                    .content_type
                                    .clone()
                                    .unwrap_or_else(|| content_type_for(&a.path).into()),
                                size_bytes: std::fs::metadata(dir.join(&a.path))
                                    .ok()
                                    .filter(|m| m.is_file())
                                    .map(|m| m.len()),
                                url: String::new(),
                            })
                            .collect(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// A content type from a file extension, for artifacts that name none.
pub fn content_type_for(path: &str) -> &'static str {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webm") => "video/webm",
        Some("mp4") => "video/mp4",
        Some("zip") => "application/zip",
        Some("html" | "htm") => "text/html",
        Some("json") => "application/json",
        Some("txt" | "log") => "text/plain",
        _ => "application/octet-stream",
    }
}

fn delivery_mode(d: DeliveryPolicy) -> DeliveryMode {
    match d {
        DeliveryPolicy::Gated => DeliveryMode::NoMistakes,
        DeliveryPolicy::Direct => DeliveryMode::DirectPr,
    }
}

/// Neutral harness ids that differ from the engine's adapter names.
pub fn engine_harness(harness: &str) -> &str {
    match harness {
        "claude-code" => "claude",
        "cursor-agent" => "cursor",
        "bob-shell" => "bob",
        other => other,
    }
}

/// The secondmate charter and routing scope for a Project workspace. The
/// coordinator reads the charter as its standing job description.
pub fn charter(plan: &WorkspacePlan) -> (String, String) {
    let repos: Vec<_> = plan.sources.iter().map(|s| s.name.as_str()).collect();
    let goal = plan
        .goal
        .as_deref()
        .map(str::trim)
        .filter(|g| !g.is_empty())
        .map(|g| format!(" Its goal: {g}"))
        .unwrap_or_default();
    let shared = plan
        .user_memory
        .as_deref()
        .map(|dir| {
            format!(
                " Memory shared by every Project is in {}; read every entry there too, and reread it when told a new one landed.",
                dir.display()
            )
        })
        .unwrap_or_default();
    let charter = format!(
        "Coordinate the Quark Project \"{}\" across {}.{goal} The Project repo checked out at project/ holds its instructions.md and memory/; read instructions.md and every entry in memory/ before planning work, and reread memory/ when told a new entry landed. Have workers report what a task taught them that is worth keeping as `learned: <text>` status lines before done:, and add your own for a finished task as `learned [source=coordinator]: <text>` in its status log; each becomes a memory proposal for review.{shared}",
        plan.name,
        repos.join(", "),
    );
    let scope = format!(
        "All work for the Quark Project \"{}\" ({}) in {}.",
        plan.name,
        plan.project_id,
        repos.join(", ")
    );
    (charter, scope)
}

/// Task records (`<id>.meta`), status logs (`<id>.status`) and the backlog.
/// Dot-files are the engine's own bookkeeping (watcher beats, cursors, temp
/// files before an atomic rename) and never mean a task changed by themselves.
pub fn is_task_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if name.starts_with('.') {
        return false;
    }
    name == "backlog.md" || name.ends_with(".status") || name.ends_with(".meta")
}

/// Live tasks from metadata, plus queued backlog work that has not started.
/// Secondmates are workspaces, not tasks, so they are left out.
pub fn neutral_snapshot(s: &FmSnapshot) -> FleetSnapshot {
    let mut tasks: Vec<EngineTask> = s
        .tasks
        .iter()
        .filter(|t| t.kind.as_deref() != Some("secondmate"))
        .map(|t| {
            let backlog = s.backlog_items().find(|r| r.id.as_deref() == Some(&t.id));
            let current = t.current_state.as_ref();
            let pull_request_url =
                t.pr.as_ref()
                    .and_then(|p| p.url.clone())
                    .or_else(|| backlog.and_then(|r| r.pr_url.clone()));
            EngineTask {
                id: t.id.clone(),
                title: backlog
                    .and_then(|r| r.title.clone())
                    .unwrap_or_else(|| t.id.clone()),
                kind: task_kind(t.kind.as_deref()),
                state: task_state(t, pull_request_url.is_some()),
                state_note: current.and_then(|c| c.detail.clone()),
                state_source: current.and_then(|c| c.source.clone()),
                harness: t.harness.clone(),
                pull_request_url,
                terminal: tmux_target(t),
                worktree: t
                    .paths
                    .worktree
                    .as_ref()
                    .filter(|w| w.present)
                    .and_then(|w| w.path.as_ref())
                    .map(PathBuf::from),
            }
        })
        .collect();

    for r in s.backlog_items() {
        let (Some(id), BacklogState::Queued) = (&r.id, r.state) else {
            continue;
        };
        let Some(kind) = task_kind(r.kind.as_deref()) else {
            continue;
        };
        if tasks.iter().any(|t| &t.id == id) {
            continue;
        }
        tasks.push(EngineTask {
            id: id.clone(),
            title: r.title.clone().unwrap_or_else(|| id.clone()),
            kind: Some(kind),
            state: TaskState::Queued,
            state_note: r.hold_reason.clone(),
            state_source: None,
            harness: None,
            pull_request_url: None,
            terminal: None,
            worktree: None,
        });
    }
    FleetSnapshot { tasks }
}

/// Questions waiting on a person now: captain holds in the live bucket and
/// every open keyed decision. Deferred, aged and blocked holds are not
/// waiting on anyone yet.
pub fn neutral_holds(s: &FmSnapshot) -> Vec<Hold> {
    let d = decisions(s);
    let held = d.actionable_holds().map(|h| Hold {
        id: h.task_id.clone(),
        task_id: Some(h.task_id.clone()),
        question: match (&h.title, &h.reason) {
            (Some(t), Some(r)) => format!("{t}: {r}"),
            (Some(t), None) => t.clone(),
            (None, Some(r)) => r.clone(),
            (None, None) => h.task_id.clone(),
        },
        answer: None,
        answered_by: None,
        brief: hold_brief(h.brief.as_ref()),
    });
    let open = d.open.iter().map(|o| Hold {
        id: format!("{}:{}", o.task_id, o.key),
        task_id: Some(o.task_id.clone()),
        question: o.summary.clone().unwrap_or_else(|| o.verb.clone()),
        answer: None,
        answered_by: None,
        brief: worker_brief(&o.task_id),
    });
    held.chain(open).collect()
}

/// A captain hold's brief as firstmate recorded it; its coordinator asked
/// unless the brief names someone else. An unreadable brief reads as empty.
fn hold_brief(raw: Option<&serde_json::Value>) -> DecisionBrief {
    let mut brief: DecisionBrief = raw
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    brief.asked_by.get_or_insert_with(|| "coordinator".into());
    brief
}

/// The brief for a worker's keyed decision: the worker asked, and the
/// answer unblocks its task. The event log builds the same one.
pub fn worker_brief(task_id: &str) -> DecisionBrief {
    DecisionBrief {
        asked_by: Some(task_id.to_string()),
        blocks: vec![task_id.to_string()],
        ..Default::default()
    }
}

/// Secondmate window targets by secondmate id, which is the Project id for
/// workspaces Quark seeded.
pub fn coordinator_targets(s: &FmSnapshot) -> HashMap<String, String> {
    s.tasks
        .iter()
        .filter(|t| t.kind.as_deref() == Some("secondmate"))
        .filter_map(|t| Some((t.id.clone(), tmux_target(t)?)))
        .collect()
}

/// The task's tmux window target. Other backends' endpoints are not tmux
/// targets, and remote ones (`remote:<id>`) are not on this machine.
fn tmux_target(t: &Task) -> Option<String> {
    if !matches!(t.backend.as_deref(), None | Some("tmux")) {
        return None;
    }
    let target = t.endpoint.as_ref()?.target.as_deref()?;
    if target.starts_with("remote:") || !target.contains(':') {
        return None;
    }
    Some(target.to_string())
}

fn task_kind(kind: Option<&str>) -> Option<TaskKind> {
    match kind? {
        "ship" => Some(TaskKind::Ship),
        "scout" => Some(TaskKind::Scout),
        _ => None,
    }
}

/// Engine states come from `fm-crew-state.sh`:
/// working, parked, done, blocked, paused, failed, unknown.
fn task_state(t: &Task, has_pr: bool) -> TaskState {
    let Some(current) = &t.current_state else {
        return TaskState::Unknown;
    };
    match current.state.as_str() {
        "working" => TaskState::Running,
        "parked" => TaskState::NeedsDecision,
        "blocked" => TaskState::Blocked,
        "paused" => TaskState::Paused,
        "failed" => TaskState::Failed,
        // A finished ship task keeps its record until its PR lands.
        "done" if has_pr => TaskState::InReview,
        "done" => TaskState::Done,
        _ => TaskState::Unknown,
    }
}

async fn blocking<T, F>(f: F) -> Result<T, EngineError>
where
    T: Send + 'static,
    F: FnOnce() -> quark_engine::Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| EngineError::Command(format!("engine task failed: {e}")))?
        .map_err(convert)
}

fn convert(e: Error) -> EngineError {
    match e {
        Error::Io { source, .. } => EngineError::Io(source),
        Error::InvalidTaskId(id) => EngineError::TaskNotFound(id),
        Error::InvalidArgument { reason, .. } => EngineError::Invalid(reason),
        e @ (Error::Json { .. } | Error::Schema { .. } | Error::Malformed { .. }) => {
            EngineError::Parse(e.to_string())
        }
        e => EngineError::Command(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Firstmate's coordinator (a captain hold's brief file) and the native
    /// coordinator (`ask_user`) produce the same decision record.
    #[test]
    fn hold_briefs_match_native_ask_user() {
        let brief = serde_json::json!({
            "context": "Shadow agreed for 7 days.",
            "options": [{"label": "Switch now", "consequence": "Merges #95"}, "Wait a week"],
            "recommended": "Switch now",
            "recommended_why": "No disagreements.",
            "blocks": ["https://github.com/quark-systems/quark/pull/95"],
            "evidence": [{"label": "Shadow report", "url": "https://example.test/r"}]
        });
        let mut call = brief.clone();
        call["key"] = "switch-2".into();
        call["question"] = "Switch slice 2?".into();
        let native = quark_coordinator::ToolCall::parse("ask_user", &call)
            .unwrap()
            .brief()
            .unwrap();
        let firstmate = hold_brief(Some(&brief)).normalized();
        assert_eq!(firstmate, native);
        assert_eq!(firstmate.asked_by.as_deref(), Some("coordinator"));
        assert_eq!(firstmate.options.len(), 2);
    }

    #[test]
    fn an_unreadable_hold_brief_reads_as_asked_by_the_coordinator() {
        let b = hold_brief(Some(&serde_json::json!({"options": 3})));
        assert_eq!(
            b,
            DecisionBrief {
                asked_by: Some("coordinator".into()),
                ..Default::default()
            }
        );
    }
}
