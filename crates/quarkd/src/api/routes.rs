use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use quark_systems::{
    CoordinatorMessage, CoordinatorMessageAccepted, CreateProject, Decision, DecisionState,
    ErrorBody, Health, Project, RelaunchTask, SendTaskMessage, Task, TranscriptItem, UpdateProject,
};
use std::path::PathBuf;

use crate::chat::{self, ChatError, Delivery};
use crate::store::TranscriptSource;
use serde::Deserialize;
use utoipa::IntoParams;

use super::{db, ApiError, AppState};
use crate::engine::{TaskControl, WorkspaceRef};

/// Note given to a relaunched worker when the client sends none.
const DEFAULT_RELAUNCH_NOTE: &str =
    "Relaunched from Quark. Pick up from the current state of the worktree and the task brief.";

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
/// Phase 0 records the Project in the daemon only. Seeding its engine
/// workspace arrives with the adapter write path.
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
    let project = db(&state, move |s| s.create_project(input)).await?;
    Ok((StatusCode::CREATED, Json(project)))
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
        (status = 404, body = ErrorBody)
    )
)]
pub async fn update_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<UpdateProject>,
) -> Result<Json<Project>, ApiError> {
    Ok(Json(
        db(&state, move |s| s.update_project(&id, input)).await?,
    ))
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
    let note = input
        .note
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_RELAUNCH_NOTE.into());
    let action = TaskControl::Relaunch {
        harness: input.harness,
        model: input.model,
        effort: input.effort,
        note,
    };
    let (ws, task) = engine_target(&state, id).await?;
    state.engine.control(&ws, &task, &action).await?;
    Ok(StatusCode::NO_CONTENT)
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
