use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use quark_systems::{
    AgentRole, AnswerDecision, CoordinatorMessage, CoordinatorMessageAccepted, CreateProject,
    Decision, DecisionState, DispatchRecord, ErrorBody, Health, Project, ProjectStatus,
    RelaunchTask, SendTaskMessage, Task, TaskChanges, TaskDiff, TaskEvent, TranscriptItem,
    UpdateProject,
};
use std::path::PathBuf;

use crate::chat::{self, ChatError, Delivery};
use crate::store::TranscriptSource;
use serde::Deserialize;
use utoipa::IntoParams;

use super::{db, ApiError, AppState};
use crate::accounts::Holder;
use crate::engine::{TaskControl, WorkspaceRef};
use crate::failover::Failover;
use crate::provision;

/// Note given to a relaunched worker when the client sends none.
const DEFAULT_RELAUNCH_NOTE: &str =
    "Relaunched from Quark. Pick up from the current state of the worktree and the task brief.";
use crate::worktree::{self, WorktreeError};

/// Daemon health and the latest event `seq`.
#[utoipa::path(
    get,
    path = "/v1/health",
    tag = "daemon",
    responses((status = 200, body = Health))
)]
pub async fn health(State(state): State<AppState>) -> Result<Json<Health>, ApiError> {
    let last_seq = db(&state, |s| s.last_seq()).await?;
    Ok(Json(Health {
        status: "ok".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        engine: state.engine.name().into(),
        last_seq,
    }))
}

/// List Projects.
#[utoipa::path(
    get,
    path = "/v1/projects",
    tag = "projects",
    responses((status = 200, body = Vec<Project>))
)]
pub async fn list_projects(State(state): State<AppState>) -> Result<Json<Vec<Project>>, ApiError> {
    Ok(Json(db(&state, |s| s.list_projects()).await?))
}

/// Create a Project.
///
/// With `repos`, returns at once with status `provisioning` and provisions in
/// the background: clones the repos into a new Project workspace, writes the
/// Project repo and starts the coordinator. `project.updated` events report
/// each step and the final `ready` or `failed` status.
#[utoipa::path(
    post,
    path = "/v1/projects",
    tag = "projects",
    request_body = CreateProject,
    responses(
        (status = 201, body = Project),
        (status = 400, body = ErrorBody)
    )
)]
pub async fn create_project(
    State(state): State<AppState>,
    Json(input): Json<CreateProject>,
) -> Result<(StatusCode, Json<Project>), ApiError> {
    let input = provision::normalize(input).map_err(ApiError::invalid)?;
    if let Some(agent) = &input.agent_config {
        let check = state.harnesses.validate(agent, AgentRole::Coordinator);
        if !check.valid {
            let msgs: Vec<_> = check.errors.into_iter().map(|e| e.message).collect();
            return Err(ApiError::invalid(format!(
                "agent_config: {}",
                msgs.join("; ")
            )));
        }
        if let Some(pool) = &agent.pool {
            let accounts = state.accounts.list().await?;
            if !accounts
                .iter()
                .any(|a| a.harness == agent.harness && a.pools.contains(pool))
            {
                return Err(ApiError::invalid(format!(
                    "agent_config: pool `{pool}` has no {} accounts",
                    agent.harness
                )));
            }
        }
    }
    let project = db(&state, move |s| s.create_project(input)).await?;
    if project.status == ProjectStatus::Provisioning {
        start_provisioning(&state, &project.id);
    }
    Ok((StatusCode::CREATED, Json(project)))
}

fn start_provisioning(state: &AppState, project_id: &str) {
    tokio::spawn(provision::provision(
        state.store.clone(),
        state.engine.clone(),
        state.accounts.clone(),
        state.layout.clone(),
        project_id.to_string(),
    ));
}

/// Dispatches `POST /v1/projects/{id}:<action>`.
pub async fn project_action(
    state: State<AppState>,
    Path(target): Path<String>,
) -> Result<(StatusCode, Json<Project>), ApiError> {
    match target.rsplit_once(':') {
        Some((id, "provision")) => provision_project(state, Path(id.to_string())).await,
        Some(_) => Err(ApiError::not_found()),
        None => Err(ApiError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "use POST /v1/projects/{id}:provision",
        )),
    }
}

/// Retry provisioning a Project whose provisioning failed.
///
/// Every step is safe to repeat, so provisioning starts again from the first
/// step. Returns the Project with status `provisioning`.
#[utoipa::path(
    post,
    path = "/v1/projects/{id}:provision",
    tag = "projects",
    params(("id" = String, Path, description = "Project id")),
    responses(
        (status = 202, body = Project),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "The Project is not in a failed state")
    )
)]
pub async fn provision_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Project>), ApiError> {
    let project = db(&state, move |s| s.retry_provisioning(&id)).await?;
    start_provisioning(&state, &project.id);
    Ok((StatusCode::ACCEPTED, Json(project)))
}

/// Get one Project.
#[utoipa::path(
    get,
    path = "/v1/projects/{id}",
    tag = "projects",
    params(("id" = String, Path, description = "Project id")),
    responses(
        (status = 200, body = Project),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn get_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Project>, ApiError> {
    Ok(Json(db(&state, move |s| s.get_project(&id)).await?))
}

/// Configure a Project. Absent fields are left unchanged.
#[utoipa::path(
    patch,
    path = "/v1/projects/{id}",
    tag = "projects",
    params(("id" = String, Path, description = "Project id")),
    request_body = UpdateProject,
    responses(
        (status = 200, body = Project),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "Standing approval needs the Project's workspace", body = ErrorBody),
        (status = 502, description = "The engine refused or failed the call", body = ErrorBody)
    )
)]
pub async fn update_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<UpdateProject>,
) -> Result<Json<Project>, ApiError> {
    if let Some(on) = input.standing_approval {
        let pid = id.clone();
        let project = db(&state, move |s| s.get_project(&pid)).await?;
        set_standing_approval(&state, &project, on).await?;
    }
    Ok(Json(
        db(&state, move |s| s.update_project(&id, input)).await?,
    ))
}

/// Applies standing approval to the engine for the Project's repos, so its
/// coordinator merges green work too. A Project without repos has nothing
/// registered with the engine; the daemon alone applies it.
async fn set_standing_approval(
    state: &AppState,
    project: &Project,
    on: bool,
) -> Result<(), ApiError> {
    let repos: Vec<String> = project
        .repos
        .iter()
        .filter_map(|r| r.name.clone())
        .collect();
    if repos.is_empty() || project.standing_approval == on {
        return Ok(());
    }
    let Some(root) = &project.workspace_path else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "workspace_missing",
            "the Project has no workspace yet; retry once it is provisioned",
        ));
    };
    let ws = WorkspaceRef {
        project_id: project.id.clone(),
        root: root.into(),
    };
    state.engine.set_standing_approval(&ws, &repos, on).await?;
    Ok(())
}

/// The task board for one Project.
#[utoipa::path(
    get,
    path = "/v1/projects/{id}/tasks",
    tag = "tasks",
    params(("id" = String, Path, description = "Project id")),
    responses(
        (status = 200, body = Vec<Task>),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn list_tasks(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<Task>>, ApiError> {
    Ok(Json(db(&state, move |s| s.list_tasks(&id)).await?))
}

/// Task detail.
#[utoipa::path(
    get,
    path = "/v1/tasks/{id}",
    tag = "tasks",
    params(("id" = String, Path, description = "Task id")),
    responses(
        (status = 200, body = Task),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn get_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Task>, ApiError> {
    Ok(Json(db(&state, move |s| s.get_task(&id)).await?))
}

/// The engine task id and workspace for an API task id.
async fn engine_target(state: &AppState, id: String) -> Result<(WorkspaceRef, String), ApiError> {
    let t = db(state, move |s| s.task_target(&id)).await?;
    let Some(root) = t.workspace_path else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "workspace_missing",
            "the task's Project has no workspace attached",
        ));
    };
    Ok((
        WorkspaceRef {
            project_id: t.project_id,
            root: root.into(),
        },
        t.engine_id,
    ))
}

/// Steer a task's worker.
///
/// The message is durably recorded for the worker before this returns; the
/// worker reads it on its next turn.
#[utoipa::path(
    post,
    path = "/v1/tasks/{id}/messages",
    tag = "tasks",
    params(("id" = String, Path, description = "Task id")),
    request_body = SendTaskMessage,
    responses(
        (status = 204, description = "Message recorded for the worker"),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody),
        (status = 502, body = ErrorBody, description = "The engine refused or failed the call")
    )
)]
pub async fn send_task_message(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<SendTaskMessage>,
) -> Result<StatusCode, ApiError> {
    if input.text.trim().is_empty() {
        return Err(ApiError::invalid("text is empty"));
    }
    let (ws, task) = engine_target(&state, id).await?;
    state.engine.send_message(&ws, &task, &input.text).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Dispatches `POST /v1/tasks/{id}:<action>`.
pub async fn task_action(
    state: State<AppState>,
    Path(target): Path<String>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let Some((id, action)) = target.rsplit_once(':') else {
        return Err(ApiError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "use POST /v1/tasks/{id}:cancel or :relaunch",
        ));
    };
    let id = Path(id.to_string());
    match action {
        "cancel" => cancel_task(state, id).await,
        "relaunch" => {
            // The body is optional, so an empty one is accepted whatever its
            // content type says.
            let input = if body.iter().all(u8::is_ascii_whitespace) {
                None
            } else {
                Some(Json(serde_json::from_slice(&body).map_err(|e| {
                    ApiError::invalid(format!("invalid relaunch body: {e}"))
                })?))
            };
            relaunch_task(state, id, input).await
        }
        _ => Err(ApiError::not_found()),
    }
}

/// Cancel a task's worker.
///
/// Stops the worker and keeps its worktree and every uncommitted change;
/// relaunch picks the task up again.
#[utoipa::path(
    post,
    path = "/v1/tasks/{id}:cancel",
    tag = "tasks",
    params(("id" = String, Path, description = "Task id")),
    responses(
        (status = 204, description = "The worker is stopped"),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody),
        (status = 502, body = ErrorBody, description = "The engine refused or failed the call")
    )
)]
pub async fn cancel_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let (ws, task) = engine_target(&state, id).await?;
    state
        .engine
        .control(&ws, &task, &TaskControl::Cancel)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Relaunch a task's worker in the same worktree.
#[utoipa::path(
    post,
    path = "/v1/tasks/{id}:relaunch",
    tag = "tasks",
    params(("id" = String, Path, description = "Task id")),
    request_body = RelaunchTask,
    responses(
        (status = 204, description = "A new worker is running"),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody),
        (status = 502, body = ErrorBody, description = "The engine refused or failed the call")
    )
)]
pub async fn relaunch_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
    input: Option<Json<RelaunchTask>>,
) -> Result<StatusCode, ApiError> {
    let input = input.map(|Json(i)| i).unwrap_or_default();
    let (ws, task) = engine_target(&state, id.clone()).await?;
    let account_env = relaunch_account(&state, &id, &input).await?;
    let note = input
        .note
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_RELAUNCH_NOTE.into());
    let action = TaskControl::Relaunch {
        harness: input.harness,
        model: input.model,
        effort: input.effort,
        note,
        account_env,
    };
    state.engine.control(&ws, &task, &action).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The account a relaunched worker runs under: the task's own while it is
/// still usable (sticky), else one from `pool`, or from the Project's agent
/// config's pool when the worker keeps that harness. Recorded on the task.
async fn relaunch_account(
    state: &AppState,
    id: &str,
    input: &RelaunchTask,
) -> Result<Vec<(String, String)>, ApiError> {
    let task_id = id.to_string();
    let task = db(state, move |s| s.get_task(&task_id)).await?;
    let Some(harness) = input.harness.clone().or(task.harness) else {
        return Ok(Vec::new());
    };
    let pid = task.project_id.clone();
    let project = db(state, move |s| s.get_project(&pid)).await?;
    let same_harness = |a: &&quark_systems::AgentConfig| {
        state.harnesses.resolve(&a.harness).map(|h| h.id())
            == state.harnesses.resolve(&harness).map(|h| h.id())
    };
    let pool = input.pool.clone().or_else(|| {
        project
            .agent_config
            .as_ref()
            .filter(same_harness)
            .and_then(|a| a.pool.clone())
    });
    let config = quark_systems::AgentConfig {
        harness,
        model: None,
        effort: None,
        pool,
    };
    let lease = state
        .accounts
        .lease(&Holder::Task(id.to_string()), &config)
        .await?;
    Ok(lease.map(|l| l.env).unwrap_or_default())
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TaskEventQuery {
    /// Only entries with a greater `id`; 0 or absent starts from the oldest.
    pub after: Option<i64>,
    /// At most this many entries (default 200, max 1000).
    pub limit: Option<u32>,
}

/// A task's activity log, oldest first. Live entries arrive on the event
/// stream as `task.event`.
#[utoipa::path(
    get,
    path = "/v1/tasks/{id}/events",
    tag = "tasks",
    params(("id" = String, Path, description = "Task id"), TaskEventQuery),
    responses(
        (status = 200, body = Vec<TaskEvent>),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn list_task_events(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<TaskEventQuery>,
) -> Result<Json<Vec<TaskEvent>>, ApiError> {
    let after = q.after.unwrap_or(0);
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);
    Ok(Json(
        db(&state, move |s| s.list_task_events(&id, after, limit)).await?,
    ))
}

/// Why the task got its agent: one dispatch record per worker spawn, oldest
/// first (the first spawn, then each relaunch). Empty until the task's worker
/// has been spawned. Records outlive the task. New records arrive on the
/// event stream as `dispatch.recorded`.
#[utoipa::path(
    get,
    path = "/v1/tasks/{id}/dispatch",
    tag = "tasks",
    params(("id" = String, Path, description = "Task id")),
    responses(
        (status = 200, body = Vec<DispatchRecord>),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn list_task_dispatch(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<DispatchRecord>>, ApiError> {
    Ok(Json(db(&state, move |s| s.list_dispatch(&id)).await?))
}

/// Files the task changed: its working tree, including uncommitted and
/// untracked work, against where it branched from the default branch.
#[utoipa::path(
    get,
    path = "/v1/tasks/{id}/changes",
    tag = "tasks",
    params(("id" = String, Path, description = "Task id")),
    responses(
        (status = 200, body = TaskChanges),
        (status = 404, body = ErrorBody),
        (status = 409, description = "The task has no readable working tree", body = ErrorBody)
    )
)]
pub async fn get_task_changes(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<TaskChanges>, ApiError> {
    let dir = task_worktree(&state, &id).await?;
    let c = worktree::changes(&dir).await.map_err(worktree_error)?;
    Ok(Json(TaskChanges {
        task_id: id,
        base_ref: c.base_ref,
        base: c.base,
        head: c.head,
        files: c.files,
    }))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DiffQuery {
    /// One changed file, as listed by `/changes`; absent for the whole task.
    pub path: Option<String>,
}

/// Unified diff of the task's changes, or of one changed file.
#[utoipa::path(
    get,
    path = "/v1/tasks/{id}/diff",
    tag = "tasks",
    params(("id" = String, Path, description = "Task id"), DiffQuery),
    responses(
        (status = 200, body = TaskDiff),
        (status = 404, description = "Unknown task, or a path the task did not change", body = ErrorBody),
        (status = 409, description = "The task has no readable working tree", body = ErrorBody)
    )
)]
pub async fn get_task_diff(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<DiffQuery>,
) -> Result<Json<TaskDiff>, ApiError> {
    let dir = task_worktree(&state, &id).await?;
    let patch = worktree::diff(&dir, q.path.as_deref())
        .await
        .map_err(worktree_error)?;
    Ok(Json(TaskDiff {
        task_id: id,
        base: patch.base,
        path: q.path,
        patch: patch.text,
        truncated: patch.truncated,
    }))
}

async fn task_worktree(state: &AppState, id: &str) -> Result<std::path::PathBuf, ApiError> {
    let id = id.to_string();
    db(state, move |s| s.task_worktree(&id))
        .await?
        .map(Into::into)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "no_worktree",
                "the task has no working tree on this machine",
            )
        })
}

fn worktree_error(e: WorktreeError) -> ApiError {
    match e {
        WorktreeError::NotChanged(_) => {
            ApiError::new(StatusCode::NOT_FOUND, "not_found", e.to_string())
        }
        WorktreeError::Missing(_) | WorktreeError::NoBase => {
            ApiError::new(StatusCode::CONFLICT, "worktree_unavailable", e.to_string())
        }
        WorktreeError::Git { .. } | WorktreeError::Io(_) => {
            tracing::warn!(error = %e, "reading a task worktree failed");
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "worktree_read_failed",
                e.to_string(),
            )
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DecisionQuery {
    /// Only decisions in this state.
    pub state: Option<DecisionState>,
}

/// The decisions inbox across all Projects.
#[utoipa::path(
    get,
    path = "/v1/decisions",
    tag = "decisions",
    params(DecisionQuery),
    responses((status = 200, body = Vec<Decision>))
)]
pub async fn list_decisions(
    State(state): State<AppState>,
    Query(q): Query<DecisionQuery>,
) -> Result<Json<Vec<Decision>>, ApiError> {
    Ok(Json(db(&state, move |s| s.list_decisions(q.state)).await?))
}

/// Dispatches `POST /v1/decisions/{id}:<action>`.
pub async fn decision_action(
    state: State<AppState>,
    Path(target): Path<String>,
    body: Bytes,
) -> Result<Json<Decision>, ApiError> {
    let Some((id, action)) = target.rsplit_once(':') else {
        return Err(ApiError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "use POST /v1/decisions/{id}:answer",
        ));
    };
    match action {
        "answer" => {
            let input = serde_json::from_slice(&body)
                .map_err(|e| ApiError::invalid(format!("invalid answer body: {e}")))?;
            answer_decision(state, Path(id.to_string()), Json(input)).await
        }
        _ => Err(ApiError::not_found()),
    }
}

/// Longest `answered_by` accepted, in bytes.
pub(super) const MAX_ANSWERED_BY_BYTES: usize = 128;

/// Answer an open decision.
///
/// The engine records the answer with who gave it, which unblocks the task
/// that asked. A decision the daemon opened about a rate limit relaunches
/// the task's worker instead, with the answer as its note. The answered decision is returned and also arrives as a
/// `decision.answered` event.
#[utoipa::path(
    post,
    path = "/v1/decisions/{id}:answer",
    tag = "decisions",
    params(("id" = String, Path, description = "Decision id")),
    request_body = AnswerDecision,
    responses(
        (status = 200, body = Decision, description = "The answer is recorded"),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`already_answered`, or `workspace_missing`"),
        (status = 502, body = ErrorBody, description = "The engine refused or failed the call")
    )
)]
pub async fn answer_decision(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<AnswerDecision>,
) -> Result<Json<Decision>, ApiError> {
    if input.answer.trim().is_empty() {
        return Err(ApiError::invalid("answer is empty"));
    }
    let answered_by = match input.answered_by.as_deref().map(str::trim) {
        Some(user) if !user.is_empty() => user.to_string(),
        _ => daemon_user(),
    };
    if answered_by.is_empty()
        || answered_by.len() > MAX_ANSWERED_BY_BYTES
        || answered_by.chars().any(char::is_control)
    {
        return Err(ApiError::invalid(
            "answered_by must be one line of at most 128 bytes",
        ));
    }
    let target = {
        let id = id.clone();
        db(&state, move |s| s.decision_target(&id)).await?
    };
    if !target.open {
        return Err(already_answered());
    }
    let Some(root) = target.workspace_path else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "workspace_missing",
            "the decision's Project has no workspace attached",
        ));
    };
    let ws = WorkspaceRef {
        project_id: target.project_id,
        root: root.into(),
    };
    if crate::store::is_daemon_decision(&target.engine_id) {
        // The daemon's own question about a rate limit: the answer
        // relaunches the worker instead of going to the engine.
        let decision = {
            let id = id.clone();
            db(&state, move |s| s.get_decision(&id)).await?
        };
        Failover::new(
            state.store.clone(),
            state.engine.clone(),
            state.accounts.clone(),
            state.harnesses.clone(),
        )
        .resume(&ws, &decision, &input.answer)
        .await?;
    } else {
        state
            .engine
            .answer(&ws, &target.engine_id, &input.answer, &answered_by)
            .await?;
    }
    let decision = db(&state, move |s| {
        s.answer_decision(&id, &input.answer, &answered_by)
    })
    .await
    .map_err(|e| match e.code() {
        "conflict" => already_answered(),
        _ => e,
    })?;
    Ok(Json(decision))
}

fn already_answered() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "already_answered",
        "the decision is already answered",
    )
}

/// Who answers when the client names no one: the user running the daemon.
pub(super) fn daemon_user() -> String {
    ["USER", "USERNAME", "LOGNAME"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|u| !u.trim().is_empty()))
        .map(|u| u.trim().to_string())
        .unwrap_or_else(|| "local".into())
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TranscriptQuery {
    /// Only entries with `id` greater than this.
    pub after: Option<i64>,
    /// At most this many entries (default and maximum 1000).
    pub limit: Option<u32>,
}

impl TranscriptQuery {
    fn bounds(&self) -> (i64, u32) {
        (
            self.after.unwrap_or(0),
            self.limit.unwrap_or(1000).clamp(1, 1000),
        )
    }
}

/// A worker's transcript so far, read from its harness session log. New
/// entries arrive as `worker.transcript` events.
#[utoipa::path(
    get,
    path = "/v1/tasks/{id}/transcript",
    tag = "tasks",
    params(("id" = String, Path, description = "Task id"), TranscriptQuery),
    responses(
        (status = 200, body = Vec<TranscriptItem>),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn task_transcript(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<TranscriptQuery>,
) -> Result<Json<Vec<TranscriptItem>>, ApiError> {
    let (after, limit) = q.bounds();
    Ok(Json(
        db(&state, move |s| {
            s.get_task(&id)?;
            s.transcript(&TranscriptSource::Task { task_id: id }, after, limit)
        })
        .await?,
    ))
}

/// A Project coordinator's conversation so far. The coordinator id is the
/// Project id. New entries arrive as `coordinator.message` events.
#[utoipa::path(
    get,
    path = "/v1/coordinators/{id}/messages",
    tag = "coordinators",
    params(("id" = String, Path, description = "Coordinator id (the Project id)"), TranscriptQuery),
    responses(
        (status = 200, body = Vec<TranscriptItem>),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn coordinator_messages(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<TranscriptQuery>,
) -> Result<Json<Vec<TranscriptItem>>, ApiError> {
    let (after, limit) = q.bounds();
    Ok(Json(
        db(&state, move |s| {
            s.get_project(&id)?;
            s.transcript(
                &TranscriptSource::Coordinator { project_id: id },
                after,
                limit,
            )
        })
        .await?,
    ))
}

/// Send a message to a Project's coordinator.
///
/// The coordinator id is the Project id. The text is typed into the
/// coordinator's live session; its reply arrives on the event stream as
/// `coordinator.message` events, which also carry this message once the
/// session records it.
#[utoipa::path(
    post,
    path = "/v1/coordinators/{id}/messages",
    tag = "coordinators",
    params(("id" = String, Path, description = "Coordinator id (the Project id)")),
    request_body = CoordinatorMessage,
    responses(
        (status = 202, body = CoordinatorMessageAccepted),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "The Project has no workspace yet", body = ErrorBody),
        (status = 502, description = "Typing into the session failed", body = ErrorBody),
        (status = 503, description = "No live coordinator session", body = ErrorBody)
    )
)]
pub async fn send_coordinator_message(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<CoordinatorMessage>,
) -> Result<(StatusCode, Json<CoordinatorMessageAccepted>), ApiError> {
    chat::validate(&input.text).map_err(ApiError::invalid)?;
    let project_id = id.clone();
    let project = db(&state, move |s| s.get_project(&project_id)).await?;
    let Some(root) = project.workspace_path else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "no_workspace",
            "the Project has no workspace, so it has no coordinator yet",
        ));
    };
    let ws = WorkspaceRef {
        project_id: project.id.clone(),
        root: PathBuf::from(root),
    };
    let confirmed = match state.chat.send(&ws, &input.text).await {
        Ok(Delivery::Confirmed) => true,
        Ok(Delivery::Unconfirmed) => false,
        Err(ChatError::Unavailable(m)) => {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "session_unavailable",
                m,
            ))
        }
        Err(ChatError::Failed(m)) => {
            tracing::warn!(coordinator = %id, error = %m, "coordinator message not delivered");
            return Err(ApiError::new(StatusCode::BAD_GATEWAY, "delivery_failed", m));
        }
    };
    Ok((
        StatusCode::ACCEPTED,
        Json(CoordinatorMessageAccepted {
            coordinator_id: project.id,
            confirmed,
            accepted_at: crate::now_rfc3339(),
        }),
    ))
}

/// This module's endpoints, plus schemas the generator does not reach from
/// them (event payloads), merged into the served document by
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(
        health,
        list_projects,
        create_project,
        get_project,
        update_project,
        provision_project,
        list_tasks,
        get_task,
        send_task_message,
        cancel_task,
        relaunch_task,
        list_task_events,
        list_task_dispatch,
        get_task_changes,
        get_task_diff,
        list_decisions,
        answer_decision,
        task_transcript,
        coordinator_messages,
        send_coordinator_message
    ),
    components(schemas(
        quark_systems::TaskState,
        quark_systems::TaskKind,
        quark_systems::TaskEvent,
        quark_systems::FileChangeStatus,
        quark_systems::DecisionState,
        quark_systems::ProjectStatus,
        quark_systems::DeliveryPolicy,
        quark_systems::DispatchPreset,
        quark_systems::TranscriptEntry,
        quark_systems::TranscriptRole,
        quark_systems::ToolInfo,
        quark_systems::ToolKind,
        quark_systems::ToolDiffLine,
        quark_systems::ToolDiffLineKind,
    ))
)]
pub(super) struct Api;

/// This module's routes, merged into the `/v1` router.
pub(super) fn router() -> axum::Router<AppState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/v1/health", get(health))
        .route("/v1/projects", get(list_projects).post(create_project))
        // POST serves the custom method `/v1/projects/{id}:provision`.
        .route(
            "/v1/projects/{id}",
            get(get_project).patch(update_project).post(project_action),
        )
        .route("/v1/projects/{id}/tasks", get(list_tasks))
        // POST serves the custom methods `/v1/tasks/{id}:cancel` and
        // `:relaunch`; the router allows one parameter per segment.
        .route("/v1/tasks/{id}", get(get_task).post(task_action))
        .route("/v1/tasks/{id}/messages", post(send_task_message))
        .route("/v1/tasks/{id}/transcript", get(task_transcript))
        .route("/v1/tasks/{id}/events", get(list_task_events))
        .route("/v1/tasks/{id}/dispatch", get(list_task_dispatch))
        .route("/v1/tasks/{id}/changes", get(get_task_changes))
        .route("/v1/tasks/{id}/diff", get(get_task_diff))
        .route("/v1/decisions", get(list_decisions))
        // POST serves the custom method `/v1/decisions/{id}:answer`.
        .route("/v1/decisions/{id}", post(decision_action))
        .route(
            "/v1/coordinators/{id}/messages",
            get(coordinator_messages).post(send_coordinator_message),
        )
}
