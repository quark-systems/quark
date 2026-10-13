//! Slice 2 for the native engine: the guarded merge, standing approval and
//! the verification gates, on [`quark_verify`] with no firstmate.
//!
//! State lives in the event log (merges, red-main episodes, standing
//! approval) and under the Project workspace root:
//!
//! - `config/gates.json`: the gates config (schema `fm.gates.v1`, see
//!   [`crate::gates`]), the path firstmate's engine reads it from;
//! - `verify/<task>/`: where a task's gates run, with their logs under
//!   `logs/` and the last run's evidence in `evidence.json`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use quark_core::{CoreError, EventLog, HostId, ProjectId, TaskId};
use quark_supervisor::Supervisor;
use quark_systems::{
    ArtifactKind, Evidence, EvidenceArtifact, Gate, GateCase, GateKind, GateState, MergeMethod,
    TaskState,
};
use quark_verify::{
    Forge, GatesConfig, GuardedMerge, LandRequest, Outcome, RepoGates, StandingApproval,
};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use super::{EngineError, WorkspaceRef};

/// The gates config, relative to the workspace root.
pub const GATES_CONFIG: &str = "config/gates.json";
/// Each task's gate directory, relative to the workspace root.
pub const VERIFY_DIR: &str = "verify";
const EVIDENCE: &str = "evidence.json";

pub struct NativeVerify {
    merge: GuardedMerge,
}

impl NativeVerify {
    pub fn new(forge: Arc<dyn Forge>, log: Arc<dyn EventLog>, host: HostId) -> Self {
        Self {
            merge: GuardedMerge::new(forge, log, host),
        }
    }

    /// Merge `url`, the pull request `task` opened, through the guarded
    /// merge, and mark the task landed. A rule's refusal is an
    /// [`EngineError::Command`] naming it.
    pub async fn merge(
        &self,
        supervisor: &Supervisor,
        ws: &WorkspaceRef,
        task: &TaskId,
        url: &str,
        method: Option<MergeMethod>,
    ) -> Result<(), EngineError> {
        let project = ProjectId::new(ws.project_id.clone());
        let (record, worker) = supervisor
            .tasks(&project)
            .await
            .map_err(core)?
            .into_iter()
            .find(|(_, w)| &w.task == task)
            .ok_or_else(|| EngineError::TaskNotFound(task.to_string()))?;
        if !matches!(
            record.state,
            TaskState::InReview | TaskState::Running | TaskState::Done
        ) {
            return Err(EngineError::Invalid(format!(
                "task {task} is {}, so its pull request is not merged",
                record.state.as_str()
            )));
        }
        if let Some(pr) = record.pull_request.as_deref().filter(|pr| *pr != url) {
            return Err(EngineError::Invalid(format!(
                "task {task} opened {pr}, not {url}"
            )));
        }
        let checkout = worker.assignment.repo.clone();
        let repo_name = checkout
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| EngineError::Invalid(format!("task {task} has no repo")))?;
        let gates = GatesConfig::load(&ws.root.join(GATES_CONFIG))
            .map_err(core)?
            .repos
            .remove(&repo_name)
            .unwrap_or_default();
        let dir = task_dir(ws, task);
        // Only the last run's logs are kept; its evidence lists them.
        let logs = quark_verify::logs_dir(&dir);
        if logs.exists() {
            std::fs::remove_dir_all(&logs)?;
        }
        let started_at = now();
        let landing = self
            .merge
            .land(&LandRequest {
                project: project.clone(),
                task: task.clone(),
                pull_request: url.to_string(),
                method: method.map(verify_method).unwrap_or_default(),
                checkout,
                repo_name,
                gates: gates.clone(),
                scratch: dir.clone(),
            })
            .await
            .map_err(core)?;
        match (&landing.verdict, gates.declared()) {
            (Some(v), true) => {
                let e = evidence(v, &gates, &dir, started_at, now());
                write_atomic(&dir.join(EVIDENCE), &serde_json::to_vec_pretty(&e).unwrap())?;
            }
            // A run without gates leaves no evidence, as firstmate's does not.
            (Some(_), false) => remove_if_present(&dir.join(EVIDENCE))?,
            (None, _) => {}
        }
        match landing.outcome {
            Outcome::Merged { .. } => {
                if record.state != TaskState::Done {
                    supervisor.complete(&project, task).await.map_err(core)?;
                }
                Ok(())
            }
            Outcome::AlreadyMerged => {
                if record.state != TaskState::Done {
                    self.land_outside(supervisor, &project, task, url).await?;
                }
                Ok(())
            }
            Outcome::Refused { reason } => {
                Err(EngineError::Command(format!("merge refused: {reason}")))
            }
        }
    }

    /// The forge reports `url` merged outside [`Self::merge`]: mark the task
    /// landed and record the merge, so the coordinator wakes for it as it
    /// does for its own merges. A task that is gone, already finished, or
    /// that opened another pull request is left alone.
    pub async fn merged_outside(
        &self,
        supervisor: &Supervisor,
        ws: &WorkspaceRef,
        task: &TaskId,
        url: &str,
    ) -> Result<(), EngineError> {
        let project = ProjectId::new(ws.project_id.clone());
        let Some((record, _)) = supervisor
            .tasks(&project)
            .await
            .map_err(core)?
            .into_iter()
            .find(|(_, w)| &w.task == task)
        else {
            return Ok(());
        };
        if matches!(record.state, TaskState::Done | TaskState::Failed)
            || record.pull_request.as_deref().is_some_and(|pr| pr != url)
        {
            return Ok(());
        }
        self.land_outside(supervisor, &project, task, url).await
    }

    /// Record the merge first: a lost completion is retried, a lost record
    /// would leave the coordinator waiting.
    async fn land_outside(
        &self,
        supervisor: &Supervisor,
        project: &ProjectId,
        task: &TaskId,
        url: &str,
    ) -> Result<(), EngineError> {
        if self
            .merge
            .record_outside(project, task, url)
            .await
            .map_err(core)?
        {
            supervisor.complete(project, task).await.map_err(core)?;
        }
        Ok(())
    }

    /// The Project's red-main episode waiting on a person, as its decision
    /// id and the question: whether to merge anyway while main is red.
    pub async fn red_main_question(
        &self,
        project: &ProjectId,
    ) -> Result<Option<(String, String)>, EngineError> {
        self.merge.restore().await.map_err(core)?;
        let guard = self.merge.guard();
        let Some(ep) = guard.episode(project) else {
            return Ok(None);
        };
        let Some(decision) = ep.decision.filter(|d| !guard.is_answered(d)) else {
            return Ok(None);
        };
        let short: String = ep.commit.chars().take(12).collect();
        Ok(Some((
            decision,
            format!(
                "Main is red at {short} (failing: {}). Merges wait until it is green. \
                 Merge anyway until then?",
                ep.checks.join(", ")
            ),
        )))
    }

    /// A person answered the red-main decision: merges go ahead until main
    /// is green again.
    pub async fn override_red_main(&self, decision: &str) -> Result<(), EngineError> {
        self.merge.restore().await.map_err(core)?;
        self.merge.guard().answer(decision).await.map_err(core)
    }

    /// Standing approval is recorded per repo in the event log.
    pub async fn set_standing_approval(
        &self,
        ws: &WorkspaceRef,
        repos: &[String],
        on: bool,
    ) -> Result<(), EngineError> {
        let project = ProjectId::new(ws.project_id.clone());
        self.merge
            .set_standing_approval(&project, repos, on)
            .await
            .map_err(core)
    }

    /// Whether each repo with recorded standing approval has it on.
    pub async fn standing_approvals(
        &self,
        ws: &WorkspaceRef,
    ) -> Result<Vec<StandingApproval>, EngineError> {
        let project = ProjectId::new(ws.project_id.clone());
        Ok(self
            .merge
            .standing_approvals(&project)
            .await
            .map_err(core)?
            .into_iter()
            .map(|(repo, on)| StandingApproval { repo, on })
            .collect())
    }

    /// Replace the gates config. A config that is not `fm.gates.v1` is
    /// refused and the old one stays.
    pub fn set_gates(&self, ws: &WorkspaceRef, config: &str) -> Result<(), EngineError> {
        GatesConfig::parse(config).map_err(core)?;
        if !ws.root.is_dir() {
            return Err(EngineError::WorkspaceNotFound(ws.root.clone()));
        }
        write_atomic(&ws.root.join(GATES_CONFIG), config.as_bytes())
    }

    /// The last gate run's evidence for `task`, if its gates have run.
    pub fn gate_evidence(
        &self,
        ws: &WorkspaceRef,
        task: &TaskId,
    ) -> Result<Option<Evidence>, EngineError> {
        let path = task_dir(ws, task).join(EVIDENCE);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| EngineError::Parse(format!("{}: {e}", path.display())))
    }

    /// The file of an artifact `task`'s evidence lists.
    pub fn gate_artifact(
        &self,
        ws: &WorkspaceRef,
        task: &TaskId,
        path: &str,
    ) -> Result<PathBuf, EngineError> {
        if !quark_engine::gates::safe_relative(path) {
            return Err(EngineError::Invalid(format!("no gate artifact {path:?}")));
        }
        let e = self
            .gate_evidence(ws, task)?
            .ok_or_else(|| EngineError::TaskNotFound(task.to_string()))?;
        super::listed_artifact(&e, path)?;
        Ok(task_dir(ws, task).join(path))
    }
}

fn task_dir(ws: &WorkspaceRef, task: &TaskId) -> PathBuf {
    ws.root.join(VERIFY_DIR).join(task.as_str())
}

fn verify_method(m: MergeMethod) -> quark_verify::MergeMethod {
    match m {
        MergeMethod::Squash => quark_verify::MergeMethod::Squash,
        MergeMethod::Merge => quark_verify::MergeMethod::Merge,
        MergeMethod::Rebase => quark_verify::MergeMethod::Rebase,
    }
}

fn core(e: CoreError) -> EngineError {
    match e {
        CoreError::NotFound(m) => EngineError::TaskNotFound(m),
        CoreError::Invalid(m) | CoreError::Refused(m) | CoreError::Unsupported(m) => {
            EngineError::Invalid(m)
        }
        other => EngineError::Command(other.to_string()),
    }
}

fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), EngineError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    std::io::Write::write_all(&mut tmp, bytes)?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<(), EngineError> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

fn state(passed: bool) -> GateState {
    if passed {
        GateState::Passed
    } else {
        GateState::Failed
    }
}

/// A verdict as gate evidence. Each declared gate is listed in run order;
/// one the run stopped before is skipped. Artifacts are the gate logs, by
/// their path relative to `dir`.
pub fn evidence(
    v: &quark_core::verify::Verdict,
    gates: &RepoGates,
    dir: &Path,
    started_at: String,
    completed_at: String,
) -> Evidence {
    use quark_core::verify::GateStage;
    let stage = |s: GateStage| v.stages.iter().find(|r| r.stage == s);
    let logs = quark_verify::logs_dir(dir);
    let log = |file: String| {
        let path = logs.join(&file);
        let size = std::fs::metadata(&path).ok().filter(|m| m.is_file())?.len();
        Some(EvidenceArtifact {
            id: format!("logs/{file}"),
            kind: ArtifactKind::Log,
            name: file,
            content_type: "text/plain".into(),
            size_bytes: Some(size),
            url: String::new(),
        })
    };
    let gate = |kind, result: Option<&quark_core::verify::StageResult>, cases| Gate {
        kind,
        state: result.map_or(GateState::Skipped, |r| state(r.passed)),
        summary: result.map(|r| r.summary.clone()),
        started_at: result.map(|_| started_at.clone()),
        completed_at: result.map(|_| completed_at.clone()),
        cases,
    };
    let mut out = Vec::new();

    if !gates.checks.is_empty() {
        let result = stage(GateStage::RepoChecks);
        // The checks run in order and stop at the first failure, so the
        // summary has one `name: outcome` part per check that ran.
        let parts: Vec<&str> = result.map_or(Vec::new(), |r| r.summary.split("; ").collect());
        let cases = gates
            .checks
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let outcome = parts
                    .get(i)
                    .and_then(|p| p.strip_prefix(&format!("{}: ", c.name)));
                GateCase {
                    name: c.name.clone(),
                    state: outcome.map_or(GateState::Skipped, |o| state(o == "passed")),
                    duration_ms: None,
                    message: outcome.filter(|o| *o != "passed").map(str::to_string),
                    artifacts: outcome
                        .and_then(|_| log(quark_verify::check_log(&v.head, &c.name)))
                        .into_iter()
                        .collect(),
                }
            })
            .collect();
        out.push(gate(GateKind::Checks, result, cases));
    }

    if gates.journeys.is_some() {
        let result = stage(GateStage::Journeys);
        let cases = result
            .map(|r| {
                let prefix = quark_verify::journeys_log_prefix(&v.head);
                let mut files: Vec<String> = std::fs::read_dir(&logs)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.starts_with(&prefix))
                    .collect();
                files.sort();
                GateCase {
                    name: "journeys".into(),
                    state: state(r.passed),
                    duration_ms: None,
                    message: (!r.passed).then(|| r.summary.clone()),
                    artifacts: files.into_iter().filter_map(log).collect(),
                }
            })
            .into_iter()
            .collect();
        out.push(gate(GateKind::Journeys, result, cases));
    }

    if gates.holdout.is_some() {
        let result = stage(GateStage::Holdout);
        // Holdout cases carry only a category and pass or fail.
        let cases = result
            .map(|r| {
                let categories: Option<Vec<GateCase>> = r
                    .summary
                    .split("; ")
                    .map(|p| {
                        let (name, outcome) = p.split_once(": ")?;
                        matches!(outcome, "passed" | "failed").then(|| GateCase {
                            name: name.to_string(),
                            state: state(outcome == "passed"),
                            duration_ms: None,
                            message: None,
                            artifacts: Vec::new(),
                        })
                    })
                    .collect();
                categories.unwrap_or_default()
            })
            .unwrap_or_default();
        out.push(gate(GateKind::Holdout, result, cases));
    }

    Evidence {
        head_sha: Some(v.head.clone()),
        state: state(v.passed()),
        stale: false,
        started_at: Some(started_at.clone()),
        completed_at: Some(completed_at.clone()),
        gates: out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quark_core::verify::{GateStage, StageResult, Verdict};
    use quark_verify::{CheckGate, HoldoutGate};

    const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn evidence_lists_each_declared_gate_and_its_logs() {
        let dir = tempfile::tempdir().unwrap();
        let logs = quark_verify::logs_dir(dir.path());
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join(quark_verify::check_log(HEAD, "lint")), "ok\n").unwrap();
        std::fs::write(logs.join(quark_verify::check_log(HEAD, "test")), "no\n").unwrap();
        let check = |name: &str| CheckGate {
            name: name.into(),
            run: "true".into(),
            timeout_s: None,
        };
        let gates = RepoGates {
            checks: vec![check("lint"), check("test"), check("e2e")],
            journeys: None,
            holdout: Some(HoldoutGate {
                repo: "/h.git".into(),
                git_ref: None,
                path: None,
                timeout_s: None,
            }),
        };
        let v = Verdict {
            head: HEAD.into(),
            stages: vec![StageResult {
                stage: GateStage::RepoChecks,
                passed: false,
                summary: "lint: passed; test: failed (exit 1)".into(),
            }],
            conflict: None,
        };
        let e = evidence(&v, &gates, dir.path(), "a".into(), "b".into());
        assert_eq!(
            (e.state, e.head_sha.as_deref()),
            (GateState::Failed, Some(HEAD))
        );
        let checks = &e.gates[0];
        let states: Vec<_> = checks.cases.iter().map(|c| c.state).collect();
        assert_eq!(
            states,
            vec![GateState::Passed, GateState::Failed, GateState::Skipped]
        );
        assert_eq!(checks.cases[1].message.as_deref(), Some("failed (exit 1)"));
        let a = &checks.cases[0].artifacts[0];
        assert_eq!(
            a.id,
            format!("logs/{}", quark_verify::check_log(HEAD, "lint"))
        );
        assert_eq!(a.size_bytes, Some(3));
        assert!(checks.cases[2].artifacts.is_empty());
        // The run stopped before the holdout gate.
        assert_eq!(e.gates[1].kind, GateKind::Holdout);
        assert_eq!(e.gates[1].state, GateState::Skipped);

        let v = Verdict {
            head: HEAD.into(),
            stages: vec![StageResult {
                stage: GateStage::Holdout,
                passed: false,
                summary: "auth: passed; billing: failed".into(),
            }],
            conflict: None,
        };
        let gates = RepoGates {
            checks: Vec::new(),
            ..gates
        };
        let e = evidence(&v, &gates, dir.path(), "a".into(), "b".into());
        let cases: Vec<_> = e.gates[0]
            .cases
            .iter()
            .map(|c| (c.name.as_str(), c.state, c.message.is_none()))
            .collect();
        assert_eq!(
            cases,
            vec![
                ("auth", GateState::Passed, true),
                ("billing", GateState::Failed, true)
            ]
        );
    }
}
