//! The PR center: every Project's pull requests with their checks and
//! reviews, review comments routed to the owning worker, and merges through
//! the engine's guarded merge path.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use quark_systems::{
    ErrorBody, MergePullRequest, PullRequest, PullRequestComment, PullRequestDiff,
    PullRequestState,
};
use serde::Deserialize;
use utoipa::IntoParams;

use super::{ApiError, AppState};

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PullRequestQuery {
    /// Only pull requests in this state.
    pub state: Option<PullRequestState>,
    /// Only this Project's pull requests.
    pub project_id: Option<String>,
}

fn not_yet() -> ApiError {
    ApiError::new(
        StatusCode::NOT_IMPLEMENTED,
        "not_implemented",
        "the PR center is not wired up yet",
    )
}

/// Pull requests across all Projects, newest first, with their checks and
/// reviews. Changes arrive as `pr.updated`, `check.updated` and
/// `review.updated` events.
#[utoipa::path(
    get,
    path = "/v1/pull-requests",
    tag = "pull-requests",
    params(PullRequestQuery),
    responses((status = 200, body = Vec<PullRequest>))
)]
pub async fn list(
    State(_state): State<AppState>,
    Query(_q): Query<PullRequestQuery>,
) -> Result<Json<Vec<PullRequest>>, ApiError> {
    Err(not_yet())
}

/// One pull request.
#[utoipa::path(
    get,
    path = "/v1/pull-requests/{id}",
    tag = "pull-requests",
    params(("id" = String, Path, description = "Pull request id")),
    responses(
        (status = 200, body = PullRequest),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn get(
    State(_state): State<AppState>,
    Path(_id): Path<String>,
) -> Result<Json<PullRequest>, ApiError> {
    Err(not_yet())
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PullRequestDiffQuery {
    /// One file, as named in the diff; absent for the whole pull request.
    pub path: Option<String>,
}

/// The pull request's diff as the forge shows it, or one file of it.
#[utoipa::path(
    get,
    path = "/v1/pull-requests/{id}/diff",
    tag = "pull-requests",
    params(("id" = String, Path, description = "Pull request id"), PullRequestDiffQuery),
    responses(
        (status = 200, body = PullRequestDiff),
        (status = 404, description = "Unknown pull request, or a file the diff does not touch", body = ErrorBody),
        (status = 502, description = "The forge could not be read", body = ErrorBody)
    )
)]
pub async fn diff(
    State(_state): State<AppState>,
    Path(_id): Path<String>,
    Query(_q): Query<PullRequestDiffQuery>,
) -> Result<Json<PullRequestDiff>, ApiError> {
    Err(not_yet())
}

/// Send a review comment to the worker that owns the pull request.
///
/// The comment, with its file and line, is durably recorded for the worker
/// as a steering message; it is not posted on the forge.
#[utoipa::path(
    post,
    path = "/v1/pull-requests/{id}/comments",
    tag = "pull-requests",
    params(("id" = String, Path, description = "Pull request id")),
    request_body = PullRequestComment,
    responses(
        (status = 204, description = "Comment recorded for the worker"),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "No task owns the pull request, or its workspace is missing", body = ErrorBody),
        (status = 502, description = "The engine refused or failed the call", body = ErrorBody)
    )
)]
pub async fn comment(
    State(_state): State<AppState>,
    Path(_id): Path<String>,
    Json(_input): Json<PullRequestComment>,
) -> Result<StatusCode, ApiError> {
    Err(not_yet())
}

/// Dispatches `POST /v1/pull-requests/{id}:<action>`.
pub async fn action(
    state: State<AppState>,
    Path(target): Path<String>,
    body: Bytes,
) -> Result<Json<PullRequest>, ApiError> {
    let Some((id, action)) = target.rsplit_once(':') else {
        return Err(ApiError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "use POST /v1/pull-requests/{id}:merge",
        ));
    };
    match action {
        "merge" => {
            let input = if body.iter().all(u8::is_ascii_whitespace) {
                None
            } else {
                Some(Json(serde_json::from_slice(&body).map_err(|e| {
                    ApiError::invalid(format!("invalid merge body: {e}"))
                })?))
            };
            merge(state, Path(id.to_string()), input).await
        }
        _ => Err(ApiError::not_found()),
    }
}

/// Merge a pull request through the engine's guarded merge.
///
/// The engine re-reads the pull request live and refuses unless it is open,
/// not a draft, free of conflicts and green on its current head. Returns the
/// pull request as read from the forge after the merge.
#[utoipa::path(
    post,
    path = "/v1/pull-requests/{id}:merge",
    tag = "pull-requests",
    params(("id" = String, Path, description = "Pull request id")),
    request_body = MergePullRequest,
    responses(
        (status = 200, body = PullRequest),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "The merge was refused: not green, conflicting, already merged, or no owning task", body = ErrorBody),
        (status = 502, description = "The engine failed the call", body = ErrorBody)
    )
)]
pub async fn merge(
    State(_state): State<AppState>,
    Path(_id): Path<String>,
    _input: Option<Json<MergePullRequest>>,
) -> Result<Json<PullRequest>, ApiError> {
    Err(not_yet())
}
