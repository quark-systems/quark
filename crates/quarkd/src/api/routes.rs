use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use quark_systems::{
    CreateProject, Decision, DecisionState, ErrorBody, Health, Project, Task, UpdateProject,
};
use serde::Deserialize;
use utoipa::IntoParams;

use super::{ApiError, AppState, db};

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
