//! Clearing finished workers (see [`crate::clear`]).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use quark_systems::{ClearTask, ClearTasks, ClearedTasks, ErrorBody, KeptTask, TaskState};

use super::{db, ApiError, AppState};
use crate::clear::{self, ClearError, Cleared};

impl From<ClearError> for ApiError {
    fn from(e: ClearError) -> Self {
        match e {
            ClearError::Store(e) => e.into(),
            ClearError::Engine(e) => e.into(),
            ClearError::Join(m) => ApiError::internal(m),
        }
    }
}

fn kept_error(uncommitted: bool, unpushed_commits: u32) -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "unlanded_work",
        format!(
            "Not cleared: the worker's worktree has {}. Push that work, or clear it with discard.",
            clear::unlanded(uncommitted, unpushed_commits)
        ),
    )
}

/// Clear a finished task's worker.
///
/// Stops its session if its agent never exited, gives its worktree back and
/// takes it off the board: `GET /v1/projects/{id}/tasks` leaves it out
/// unless `include_cleared`. Its activity log and events are kept. Clearing
/// a cleared task again does nothing. With `discard`, work that has not
/// landed is thrown away instead of keeping the worker; commits stay on its
/// branch.
#[utoipa::path(
    post,
    path = "/v1/tasks/{id}:clear",
    tag = "tasks",
    params(("id" = String, Path, description = "Task id")),
    request_body = ClearTask,
    responses(
        (status = 204, description = "The task is cleared"),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "The task is still working (`conflict`), or its worktree holds work that has not landed and `discard` is not set (`unlanded_work`)"),
        (status = 502, body = ErrorBody, description = "The engine could not stop its agent or give its worktree back")
    )
)]
pub async fn clear_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
    input: Option<Json<ClearTask>>,
) -> Result<StatusCode, ApiError> {
    let discard = input.is_some_and(|Json(i)| i.discard);
    match clear::clear(&state.store, &state.engine, &id, discard).await? {
        Cleared::Cleared(_) => Ok(StatusCode::NO_CONTENT),
        Cleared::Kept {
            uncommitted,
            unpushed_commits,
            ..
        } => Err(kept_error(uncommitted, unpushed_commits)),
    }
}

/// Clear a Project's finished workers, as `POST /v1/tasks/{id}:clear` does
/// for each one in `states` (done and failed by default). Tasks it cannot
/// clear are listed under `kept` with why; the rest are still cleared.
#[utoipa::path(
    post,
    path = "/v1/projects/{id}/tasks:clear",
    tag = "tasks",
    params(("id" = String, Path, description = "Project id")),
    request_body = ClearTasks,
    responses(
        (status = 200, body = ClearedTasks),
        (status = 400, body = ErrorBody, description = "`states` names a state that is not finished"),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn clear_project_tasks(
    State(state): State<AppState>,
    Path(id): Path<String>,
    input: Option<Json<ClearTasks>>,
) -> Result<Json<ClearedTasks>, ApiError> {
    let input = input.map(|Json(i)| i).unwrap_or_default();
    let discard = input.discard;
    let states = input
        .states
        .unwrap_or_else(|| vec![TaskState::Done, TaskState::Failed]);
    if let Some(s) = states.iter().find(|s| !clear::clearable(**s)) {
        return Err(ApiError::invalid(format!(
            "only done and failed workers can be cleared, not {}",
            s.as_str()
        )));
    }
    let tasks = db(&state, move |s| s.list_tasks(&id)).await?;
    let mut out = ClearedTasks::default();
    for t in tasks
        .into_iter()
        .filter(|t| t.cleared_at.is_none() && states.contains(&t.state))
    {
        let kept = |code: &str, reason: String| KeptTask {
            task_id: t.id.clone(),
            title: t.title.clone(),
            code: code.into(),
            reason,
        };
        match clear::clear(&state.store, &state.engine, &t.id, discard).await {
            Ok(Cleared::Cleared(_)) => out.cleared.push(t.id.clone()),
            Ok(Cleared::Kept {
                uncommitted,
                unpushed_commits,
                ..
            }) => out.kept.push(kept(
                "unlanded_work",
                format!(
                    "its worktree has {}",
                    clear::unlanded(uncommitted, unpushed_commits)
                ),
            )),
            Err(e) => {
                tracing::warn!(task = %t.id, error = %e, "could not clear a worker");
                out.kept.push(kept("engine_failed", e.to_string()));
            }
        }
    }
    Ok(Json(out))
}

#[derive(utoipa::OpenApi)]
#[openapi(
    paths(clear_task, clear_project_tasks),
    components(schemas(ClearTask, ClearTasks, ClearedTasks, KeptTask))
)]
pub(super) struct Api;

/// `:clear` on a task is served by [`super::routes::task_action`], which
/// owns `/v1/tasks/{id}` and its custom methods.
pub(super) fn router() -> axum::Router<AppState> {
    axum::Router::new().route(
        "/v1/projects/{id}/tasks:clear",
        axum::routing::post(clear_project_tasks),
    )
}
