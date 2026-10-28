use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use quark_systems::{
    ActOnDecision, AgentRole, AnswerDecision, CoordinatorCommand, CoordinatorCommandRequest,
    CoordinatorMessage, CoordinatorMessageAccepted, CreateProject, Decision, DecisionState,
    DispatchRecord, ErrorBody, Health, Project, ProjectStatus, RelaunchTask, SendTaskMessage, Task,
    TaskBrief, TaskChanges, TaskDiff, TaskEvent, TranscriptItem, UpdateProject,
};
use std::path::PathBuf;

use crate::chat::{self, ChatError, Delivery};
use crate::store::{AnswerNote, TranscriptSource};
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

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ProjectQuery {
    /// Also list archived Projects (those with `archived_at`).
    #[serde(default)]
    pub include_archived: bool,
}

/// List Projects. Archived ones are left out unless `include_archived`.
#[utoipa::path(
    get,
    path = "/v1/projects",
    tag = "projects",
    params(ProjectQuery),
    responses((status = 200, body = Vec<Project>))
)]
pub async fn list_projects(
    State(state): State<AppState>,
    Query(q): Query<ProjectQuery>,
) -> Result<Json<Vec<Project>>, ApiError> {
    Ok(Json(
        db(&state, move |s| {
            if q.include_archived {
                s.list_all_projects()
            } else {
                s.list_projects()
            }
        })
        .await?,
    ))
}

/// Create a Project.
///
/// With `repos`, returns at once with status `provisioning` and provisions in
/// the background: clones the repos into a new Project workspace, writes the
/// Project repo and starts the coordinator. `project.updated` events report
/// each step and the final `ready` or `failed` status. The Project's Beads
/// database is set up beside it; `beads.status` events report that.
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
    Ok((
        StatusCode::CREATED,
        Json(create_and_provision(&state, input).await?),
    ))
}

/// Records the Project, starts provisioning it and sets up its Beads
/// database; what `POST /v1/projects` does.
pub(super) async fn create_and_provision(
    state: &AppState,
    input: CreateProject,
) -> Result<Project, ApiError> {
    let input = provision::normalize(input).map_err(ApiError::invalid)?;
    if let Some(agent) = &input.agent_config {
        check_coordinator_agent(state, agent).await?;
    }
    let project = db(state, move |s| s.create_project(input)).await?;
    if project.status == ProjectStatus::Provisioning {
        start_provisioning(state, &project.id);
    }
    // Every Project has its Beads database from the start.
    if state.beads.auto_setup() {
        state
            .beads
            .start_setup(
                state.store.clone(),
                state.layout.home.clone(),
                project.clone(),
            )
            .await;
    }
    Ok(project)
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
        Some((id, "archive")) => {
            super::lifecycle::archive_project(state, Path(id.to_string())).await
        }
        Some((id, "unarchive")) => {
            super::lifecycle::unarchive_project(state, Path(id.to_string())).await
        }
        Some(_) => Err(ApiError::not_found()),
        None => Err(ApiError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "use POST /v1/projects/{id}:provision, :archive or :unarchive",
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
pub(super) async fn set_standing_approval(
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

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TaskListQuery {
    /// Also list cleared tasks (those with `cleared_at`).
    #[serde(default)]
    pub include_cleared: bool,
}

/// The task board for one Project. Cleared tasks are left out unless
/// `include_cleared`.
#[utoipa::path(
    get,
    path = "/v1/projects/{id}/tasks",
    tag = "tasks",
    params(("id" = String, Path, description = "Project id"), TaskListQuery),
    responses(
        (status = 200, body = Vec<Task>),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn list_tasks(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<TaskListQuery>,
) -> Result<Json<Vec<Task>>, ApiError> {
    let mut tasks = db(&state, move |s| s.list_tasks(&id)).await?;
    if !q.include_cleared {
        tasks.retain(|t| t.cleared_at.is_none());
    }
    Ok(Json(tasks))
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
            "use POST /v1/tasks/{id}:cancel, :relaunch or :clear",
        ));
    };
    let id = Path(id.to_string());
    match action {
        "cancel" => cancel_task(state, id).await,
        "clear" => {
            let input = if body.iter().all(u8::is_ascii_whitespace) {
                None
            } else {
                Some(Json(serde_json::from_slice(&body).map_err(|e| {
                    ApiError::invalid(format!("invalid clear body: {e}"))
                })?))
            };
            super::clear::clear_task(state, id, input).await
        }
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

/// How and why the task's worker was dispatched: the issue it was started
/// for, the brief it received, and its delivery contract. It answers for
/// finished tasks too; parts the daemon has no record of are absent.
#[utoipa::path(
    get,
    path = "/v1/tasks/{id}/brief",
    tag = "tasks",
    params(("id" = String, Path, description = "Task id")),
    responses(
        (status = 200, body = TaskBrief),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn get_task_brief(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<TaskBrief>, ApiError> {
    Ok(Json(crate::task_brief::read(&state, id).await?))
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
    /// Only this Project's decisions.
    pub project_id: Option<String>,
}

/// The decision log: open, answered and acted-on decisions, oldest first,
/// across all Projects or one.
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
    Ok(Json(
        db(&state, move |s| {
            s.list_project_decisions(q.project_id.as_deref(), q.state)
        })
        .await?,
    ))
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
            "use POST /v1/decisions/{id}:answer or :act",
        ));
    };
    match action {
        "answer" => {
            let input = serde_json::from_slice(&body)
                .map_err(|e| ApiError::invalid(format!("invalid answer body: {e}")))?;
            answer_decision(state, Path(id.to_string()), Json(input)).await
        }
        "act" => {
            let input = serde_json::from_slice(&body)
                .map_err(|e| ApiError::invalid(format!("invalid act body: {e}")))?;
            act_on_decision(state, Path(id.to_string()), Json(input)).await
        }
        _ => Err(ApiError::not_found()),
    }
}

/// Longest `answered_by` accepted, in bytes.
pub(super) const MAX_ANSWERED_BY_BYTES: usize = 128;

/// Answer an open decision.
///
/// The engine records the answer with who gave it, which unblocks the task
/// that asked; a `why` goes with the answer as its reason. A decision the
/// daemon opened about a rate limit relaunches the task's worker instead,
/// with the answer as its note. The answered decision is returned and also
/// arrives as a `decision.answered` event. With `make_rule`, the answer also
/// becomes a standing rule (`rule.updated`).
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
    let why = trimmed(input.why.as_deref());
    let rule = trimmed(input.make_rule.as_deref());
    if why.as_ref().is_some_and(|w| w.len() > MAX_WHY_BYTES)
        || rule.as_ref().is_some_and(|r| r.len() > MAX_WHY_BYTES)
    {
        return Err(ApiError::invalid(
            "why and make_rule must be at most 2048 bytes",
        ));
    }
    let via = trimmed(input.via.as_deref()).unwrap_or_else(|| "app".into());
    if !ANSWER_CHANNELS.contains(&via.as_str()) {
        return Err(ApiError::invalid("via must be app, phone or chat"));
    }
    let text = input.answer.clone();
    answer(&state, id.clone(), input, via)
        .await
        .map(Json)
        .inspect_err(|e| {
            // The typed answer is in the log, so a refused one is not lost.
            tracing::warn!(decision = %id, code = e.code(), error = e.message(), answer = %text, "answering a decision failed");
        })
}

/// Answers an open decision through the engine and records it, from any
/// channel; `answer_decision` checks the request and the Beads mirror
/// answers with `via` "beads".
pub(crate) async fn answer(
    state: &AppState,
    id: String,
    input: AnswerDecision,
    via: String,
) -> Result<Decision, ApiError> {
    if input.answer.trim().is_empty() {
        return Err(ApiError::invalid("answer is empty"));
    }
    let answered_by = match input.answered_by.as_deref().map(str::trim) {
        Some(user) if !user.is_empty() => user.to_string(),
        _ => daemon_user(),
    };
    let why = trimmed(input.why.as_deref());
    let rule = trimmed(input.make_rule.as_deref());
    // The asker reads the reason with the answer.
    let engine_answer = match &why {
        Some(w) => format!("{}\n\nWhy: {w}", input.answer.trim_end()),
        None => input.answer.clone(),
    };
    let target = {
        let id = id.clone();
        db(state, move |s| s.decision_target(&id)).await?
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
    if let Some(key) = crate::store::coordinator_decision_key(&target.engine_id) {
        // The native coordinator's own question: the answer wakes it
        // through the event log.
        quark_coordinator::events::record_answer(
            &state.events,
            crate::host(),
            &quark_core::ProjectId::new(ws.project_id.clone()),
            None,
            key,
            &id,
            &engine_answer,
            &answered_by,
        )
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, "engine_failed", e.to_string()))?;
    } else if crate::store::is_daemon_decision(&target.engine_id) {
        // The daemon's own question about a rate limit: the answer
        // relaunches the worker instead of going to the engine.
        let decision = {
            let id = id.clone();
            db(state, move |s| s.get_decision(&id)).await?
        };
        Failover::new(
            state.store.clone(),
            state.engine.clone(),
            state.accounts.clone(),
            state.harnesses.clone(),
        )
        .resume(&ws, &decision, &engine_answer)
        .await?;
    } else {
        state
            .engine
            .answer(&ws, &target.engine_id, &engine_answer, &answered_by)
            .await?;
    }
    let note = AnswerNote {
        why,
        via: Some(via),
    };
    let decision = db(state, move |s| {
        let d = s.answer_decision_with(&id, &input.answer, &answered_by, &note)?;
        match rule {
            Some(text) => {
                s.make_rule(&d.id, &text, &answered_by)?;
                s.get_decision(&d.id)
            }
            None => Ok(d),
        }
    })
    .await
    .map_err(|e| match e.code() {
        "conflict" => already_answered(),
        _ => e,
    })?;
    Ok(decision)
}

/// Where an answer can come from.
const ANSWER_CHANNELS: &[&str] = &["app", "phone", "chat"];

/// Longest `why`, `make_rule` or `outcome` accepted, in bytes.
pub(super) const MAX_WHY_BYTES: usize = 2048;

/// `s` trimmed, or `None` when blank.
pub(super) fn trimmed(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Record what was done with an answer.
///
/// The asker (a coordinator, or a client acting for one) says what
/// happened next; the decision moves to `acted` and also arrives as a
/// `decision.acted` event. Recording again replaces the outcome.
#[utoipa::path(
    post,
    path = "/v1/decisions/{id}:act",
    tag = "decisions",
    params(("id" = String, Path, description = "Decision id")),
    request_body = ActOnDecision,
    responses(
        (status = 200, body = Decision, description = "The outcome is recorded"),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`conflict`: the decision has no answer yet")
    )
)]
pub async fn act_on_decision(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<ActOnDecision>,
) -> Result<Json<Decision>, ApiError> {
    let Some(outcome) = trimmed(Some(&input.outcome)) else {
        return Err(ApiError::invalid("outcome is empty"));
    };
    if outcome.len() > MAX_WHY_BYTES {
        return Err(ApiError::invalid("outcome must be at most 2048 bytes"));
    }
    Ok(Json(
        db(&state, move |s| s.act_on_decision(&id, &outcome)).await?,
    ))
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
    if let Some(command) = CoordinatorCommand::from_slash(&input.text) {
        return run_command(&state, &id, command).await;
    }
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
            command: None,
        }),
    ))
}

/// Ask a Project's coordinator for a catch up, a status report or a wrap up.
///
/// The same as sending the command's slash command (`/catchup`, `/status`,
/// `/wrapup`, or firstmate's `/ahoy`, `/bearings`, `/stow`) as a message.
/// The engine types the request into the coordinator's session with the
/// facts it needs: for a catch up, everything notable logged since the
/// user's last message and the open decisions; for a status report, every
/// task's live status and the open decisions. A catch up with no earlier
/// message runs as a status report, and `command` says which ran.
#[utoipa::path(
    post,
    path = "/v1/coordinators/{id}:run",
    tag = "coordinators",
    params(("id" = String, Path, description = "Coordinator id (the Project id)")),
    request_body = CoordinatorCommandRequest,
    responses(
        (status = 202, body = CoordinatorMessageAccepted),
        (status = 404, body = ErrorBody),
        (status = 409, description = "The Project has no workspace yet", body = ErrorBody),
        (status = 502, description = "Typing into the session failed", body = ErrorBody),
        (status = 503, description = "No live coordinator session", body = ErrorBody)
    )
)]
pub async fn run_coordinator_command(
    State(state): State<AppState>,
    Path(target): Path<String>,
    body: Bytes,
) -> Result<(StatusCode, Json<CoordinatorMessageAccepted>), ApiError> {
    let id = match target.rsplit_once(':') {
        Some((id, "run")) => id,
        Some((id, "compact")) => {
            return compact_coordinator(State(state), Path(id.to_string())).await
        }
        _ => {
            return Err(ApiError::new(
                StatusCode::METHOD_NOT_ALLOWED,
                "method_not_allowed",
                "use POST /v1/coordinators/{id}:run or :compact",
            ))
        }
    };
    let input: CoordinatorCommandRequest = serde_json::from_slice(&body)
        .map_err(|e| ApiError::invalid(format!("invalid command: {e}")))?;
    run_command(&state, id, input.command).await
}

/// A Project coordinator's harness, model and effort, how full its context
/// is, and when its conversation is compacted.
///
/// The context is the input of its last turn's last model call, read from
/// its session log; it is unknown before the first turn and after a
/// compaction until the next turn ends. `window_tokens` is absent when
/// Quark does not know the model's window.
#[utoipa::path(
    get,
    path = "/v1/coordinators/{id}/context",
    tag = "coordinators",
    params(("id" = String, Path, description = "Coordinator id (the Project id)")),
    responses(
        (status = 200, body = quark_systems::CoordinatorContext),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn coordinator_context(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<quark_systems::CoordinatorContext>, ApiError> {
    let compactor = state.compaction.clone();
    Ok(Json(
        db(&state, move |s| {
            let project = s.get_project(&id)?;
            compactor.context(s, &project)
        })
        .await?,
    ))
}

/// Whether `agent` can run a coordinator, and its pool has accounts.
async fn check_coordinator_agent(
    state: &AppState,
    agent: &quark_systems::AgentConfig,
) -> Result<(), ApiError> {
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
    Ok(())
}

/// Change a Project coordinator's harness, model, effort or pool.
///
/// Starts a switch and answers at once; `GET /v1/coordinators/{id}/context`
/// shows how far it got under `switch`. On the same harness the coordinator
/// continues its conversation with the new agent (`resume`). On another
/// harness the old coordinator first writes a handoff brief, then the new
/// one starts fresh with it (`handoff`). A new agent that does not start
/// puts the old one back. See `docs/engine/coordinator-switch.md`.
#[utoipa::path(
    put,
    path = "/v1/coordinators/{id}/agent",
    tag = "coordinators",
    params(("id" = String, Path, description = "Coordinator id (the Project id)")),
    request_body = quark_systems::AgentConfig,
    responses(
        (status = 202, body = quark_systems::CoordinatorSwitch),
        (status = 400, description = "The agent cannot run a coordinator, or is the one it runs", body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "A switch is under way, or the Project has no running coordinator", body = ErrorBody)
    )
)]
pub async fn switch_coordinator_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(mut to): Json<quark_systems::AgentConfig>,
) -> Result<(StatusCode, Json<quark_systems::CoordinatorSwitch>), ApiError> {
    use crate::engine::native_project::COORDINATOR_HARNESS;
    if !state.engine.switches_coordinators() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "unsupported",
            "only the native engine can change a coordinator's agent",
        ));
    }
    // Blank model or effort means the harness's default.
    to.model = to.model.filter(|m| !m.trim().is_empty());
    to.effort = to.effort.filter(|e| !e.trim().is_empty());
    to.pool = to.pool.filter(|p| !p.trim().is_empty());
    check_coordinator_agent(&state, &to).await?;
    let harness = |name: &str| state.harnesses.resolve(name).map(|h| h.id().to_string());
    let to_harness = harness(&to.harness);
    if to_harness.as_deref() != Some(COORDINATOR_HARNESS) {
        return Err(ApiError::invalid(format!(
            "the native coordinator runs on {COORDINATOR_HARNESS} only so far \
             (its tools and turn-end hook are wired for it alone), not {}",
            to.harness
        )));
    }
    let pid = id.clone();
    let project = db(&state, move |s| s.get_project(&pid)).await?;
    if project.status != ProjectStatus::Ready || project.workspace_path.is_none() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "not_ready",
            "the Project has no running coordinator to switch",
        ));
    }
    let same = project
        .agent_config
        .as_ref()
        .is_some_and(|a| harness(&a.harness) == to_harness);
    let kind = if same {
        quark_systems::SwitchKind::Resume
    } else {
        quark_systems::SwitchKind::Handoff
    };
    let switch = db(&state, move |s| s.start_switch(&id, &to, kind)).await?;
    Ok((StatusCode::ACCEPTED, Json(switch)))
}

/// Compact a Project coordinator's conversation now.
///
/// Served as `POST /v1/coordinators/{id}:compact`, with no body. Types the
/// harness's `/compact` into the coordinator's session, with what the
/// summary must keep, and answers at once: the compaction shows in the
/// conversation when the session log records it. No wrap up comes first;
/// ask for one before when the conversation holds something unsaved.
#[utoipa::path(
    post,
    path = "/v1/coordinators/{id}:compact",
    tag = "coordinators",
    params(("id" = String, Path, description = "Coordinator id (the Project id)")),
    responses(
        (status = 202, body = CoordinatorMessageAccepted),
        (status = 404, body = ErrorBody),
        (status = 409, description = "The Project has no workspace yet", body = ErrorBody)
    )
)]
pub async fn compact_coordinator(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<CoordinatorMessageAccepted>), ApiError> {
    use crate::coordinator_commands::CommandError;
    match crate::compaction::compact_now(
        state.compaction.clone(),
        state.store.clone(),
        state.chat.clone(),
        id.clone(),
    )
    .await
    {
        Ok(()) => {}
        Err(CommandError::Store(e)) => return Err(e.into()),
        Err(CommandError::NoWorkspace) => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "no_workspace",
                CommandError::NoWorkspace.to_string(),
            ))
        }
        Err(CommandError::Chat(e)) => return Err(ApiError::internal(e.to_string())),
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(CoordinatorMessageAccepted {
            coordinator_id: id,
            confirmed: false,
            accepted_at: crate::now_rfc3339(),
            command: None,
        }),
    ))
}

async fn run_command(
    state: &AppState,
    id: &str,
    command: CoordinatorCommand,
) -> Result<(StatusCode, Json<CoordinatorMessageAccepted>), ApiError> {
    use crate::coordinator_commands::{run, Asked, CommandError};
    let ran = run(
        &state.store,
        &state.events,
        state.chat.as_ref(),
        id,
        command,
        Asked::User,
    )
    .await;
    let (command, delivery) = match ran {
        Ok(r) => r,
        Err(CommandError::Store(e)) => return Err(e.into()),
        Err(CommandError::NoWorkspace) => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "no_workspace",
                CommandError::NoWorkspace.to_string(),
            ))
        }
        Err(CommandError::Chat(ChatError::Unavailable(m))) => {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "session_unavailable",
                m,
            ))
        }
        Err(CommandError::Chat(ChatError::Failed(m))) => {
            tracing::warn!(coordinator = %id, error = %m, "coordinator command not delivered");
            return Err(ApiError::new(StatusCode::BAD_GATEWAY, "delivery_failed", m));
        }
    };
    Ok((
        StatusCode::ACCEPTED,
        Json(CoordinatorMessageAccepted {
            coordinator_id: id.to_string(),
            confirmed: delivery == Delivery::Confirmed,
            accepted_at: crate::now_rfc3339(),
            command: Some(command),
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
        get_task_brief,
        get_task_changes,
        get_task_diff,
        list_decisions,
        answer_decision,
        act_on_decision,
        task_transcript,
        coordinator_messages,
        send_coordinator_message,
        run_coordinator_command,
        coordinator_context,
        compact_coordinator,
        switch_coordinator_agent
    ),
    components(schemas(
        quark_systems::TaskState,
        quark_systems::TaskKind,
        quark_systems::TaskDelivery,
        quark_systems::DispatchBrief,
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
    use axum::routing::{get, post, put};
    axum::Router::new()
        .route("/v1/health", get(health))
        .route("/v1/projects", get(list_projects).post(create_project))
        // POST serves the custom methods `/v1/projects/{id}:provision`,
        // `:archive` and `:unarchive`.
        .route(
            "/v1/projects/{id}",
            get(get_project)
                .patch(update_project)
                .post(project_action)
                .delete(super::lifecycle::delete_project),
        )
        .route("/v1/projects/{id}/tasks", get(list_tasks))
        // POST serves the custom methods `/v1/tasks/{id}:cancel`,
        // `:relaunch` and `:clear`; the router allows one parameter per
        // segment.
        .route("/v1/tasks/{id}", get(get_task).post(task_action))
        .route("/v1/tasks/{id}/messages", post(send_task_message))
        .route("/v1/tasks/{id}/transcript", get(task_transcript))
        .route("/v1/tasks/{id}/events", get(list_task_events))
        .route("/v1/tasks/{id}/dispatch", get(list_task_dispatch))
        .route("/v1/tasks/{id}/brief", get(get_task_brief))
        .route("/v1/tasks/{id}/changes", get(get_task_changes))
        .route("/v1/tasks/{id}/diff", get(get_task_diff))
        .route("/v1/decisions", get(list_decisions))
        // POST serves the custom methods `/v1/decisions/{id}:answer` and `:act`.
        .route(
            "/v1/decisions/{id}",
            get(super::decisions::get_decision).post(decision_action),
        )
        .route(
            "/v1/coordinators/{id}/messages",
            get(coordinator_messages).post(send_coordinator_message),
        )
        .route("/v1/coordinators/{id}/context", get(coordinator_context))
        .route("/v1/coordinators/{id}/agent", put(switch_coordinator_agent))
        // POST serves the custom methods `/v1/coordinators/{id}:run` and
        // `:compact`.
        .route("/v1/coordinators/{id}", post(run_coordinator_command))
}
