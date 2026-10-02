//! The PR center: every Project's pull requests with their checks and
//! reviews, review comments routed to the owning worker, and merges through
//! the engine's guarded merge path.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use quark_systems::{
    ErrorBody, MergePullRequest, PullRequest, PullRequestComment, PullRequestDiff, PullRequestState,
};
use serde::Deserialize;
use utoipa::IntoParams;

use super::{db, ApiError, AppState};
use crate::forge::{self, ForgeError};
use crate::pr_center::{self, PrError};

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PullRequestQuery {
    /// Only pull requests in this state.
    pub state: Option<PullRequestState>,
    /// Only this Project's pull requests.
    pub project_id: Option<String>,
}

/// Largest review comment accepted, in bytes.
const MAX_COMMENT_BYTES: usize = 32 * 1024;

fn pr_error(e: PrError) -> ApiError {
    match e {
        PrError::Store(e) => e.into(),
        PrError::Engine(e) => e.into(),
        PrError::Forge(e) => forge_error(e),
        PrError::NoOwner(m) => ApiError::new(StatusCode::CONFLICT, "no_owner", m),
    }
}

fn forge_error(e: ForgeError) -> ApiError {
    let code = match e {
        ForgeError::Unsupported(_) => "forge_unsupported",
        ForgeError::Missing => "forge_unavailable",
        _ => "forge_failed",
    };
    ApiError::new(StatusCode::BAD_GATEWAY, code, e.to_string())
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
    State(state): State<AppState>,
    Query(q): Query<PullRequestQuery>,
) -> Result<Json<Vec<PullRequest>>, ApiError> {
    Ok(Json(
        db(&state, move |s| {
            s.list_pull_requests(q.state, q.project_id.as_deref())
        })
        .await?,
    ))
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
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<PullRequest>, ApiError> {
    Ok(Json(db(&state, move |s| s.get_pull_request(&id)).await?))
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
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<PullRequestDiffQuery>,
) -> Result<Json<PullRequestDiff>, ApiError> {
    let pr = db(&state, move |s| s.get_pull_request(&id)).await?;
    let (patch, truncated) = state.forge.diff(&pr.url).await.map_err(forge_error)?;
    let patch = match &q.path {
        Some(path) => forge::file_patch(&patch, path).ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                format!("the pull request does not change {path}"),
            )
        })?,
        None => patch,
    };
    Ok(Json(PullRequestDiff {
        pull_request_id: pr.id,
        path: q.path,
        patch,
        truncated,
    }))
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
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<PullRequestComment>,
) -> Result<StatusCode, ApiError> {
    if input.text.trim().is_empty() {
        return Err(ApiError::invalid("text is empty"));
    }
    if input.text.len() > MAX_COMMENT_BYTES {
        return Err(ApiError::invalid(format!(
            "text is longer than {MAX_COMMENT_BYTES} bytes"
        )));
    }
    if input.line.is_some() && input.path.is_none() {
        return Err(ApiError::invalid("line needs a path"));
    }
    if input.line == Some(0) {
        return Err(ApiError::invalid("line starts at 1"));
    }
    if let Some(p) = &input.path {
        if p.trim().is_empty() || p.contains(['\n', '\r']) {
            return Err(ApiError::invalid("path is not a file path"));
        }
    }
    let owner = db(&state, move |s| s.pull_request_owner(&id)).await?;
    let (ws, task) = pr_center::owner_target(&owner).map_err(pr_error)?;
    let message = worker_message(&owner.pull_request.url, &input);
    state.engine.send_message(&ws, &task, &message).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The steering message a review comment becomes. It names the pull request
/// and, for a line comment, where in the diff the comment points.
pub fn worker_message(url: &str, c: &PullRequestComment) -> String {
    let place = match (&c.path, c.line) {
        (Some(path), Some(line)) => {
            let side = match c.side {
                Some(quark_systems::DiffSide::Old) => " (the removed version)",
                _ => "",
            };
            format!(" on {path} line {line}{side}")
        }
        (Some(path), None) => format!(" on {path}"),
        _ => String::new(),
    };
    format!(
        "Review comment on your pull request {url}{place}:\n\n{}\n\nAddress it on the same branch and push.",
        c.text.trim()
    )
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
    State(state): State<AppState>,
    Path(id): Path<String>,
    input: Option<Json<MergePullRequest>>,
) -> Result<Json<PullRequest>, ApiError> {
    let method = input.and_then(|Json(i)| i.method);
    let owner = db(&state, move |s| s.pull_request_owner(&id)).await?;
    if owner.pull_request.state.is_final() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "merge_refused",
            format!(
                "the pull request is already {}",
                owner.pull_request.state.as_str()
            ),
        ));
    }
    let merged = pr_center::merge(
        &state.store,
        state.engine.as_ref(),
        state.forge.as_ref(),
        &owner,
        method,
    )
    .await
    .map_err(|e| match e {
        // The engine's guarded merge refuses with its reasons on stderr.
        PrError::Engine(crate::engine::EngineError::Command(m)) => {
            ApiError::new(StatusCode::CONFLICT, "merge_refused", m)
        }
        e => pr_error(e),
    })?;
    Ok(Json(merged))
}

/// One file a verification gate produced: a Playwright trace, a screenshot,
/// a video, a log or a report, as listed in the pull request's `evidence`.
///
/// Served with its content type, `nosniff` and a sandboxing CSP, so an HTML
/// report cannot run script against the daemon.
#[utoipa::path(
    get,
    path = "/v1/pull-requests/{id}/evidence/artifacts/{artifact_id}",
    tag = "pull-requests",
    params(
        ("id" = String, Path, description = "Pull request id"),
        ("artifact_id" = String, Path, description = "Artifact id from the evidence")
    ),
    responses(
        (status = 200, description = "The artifact's bytes", content_type = "application/octet-stream"),
        (status = 404, description = "Unknown pull request or artifact, or the file is gone", body = ErrorBody),
        (status = 409, description = "No task owns the pull request, or its workspace is missing", body = ErrorBody)
    )
)]
pub async fn artifact(
    State(state): State<AppState>,
    Path((id, artifact_id)): Path<(String, String)>,
) -> Result<axum::response::Response, ApiError> {
    use axum::http::header;
    use axum::response::IntoResponse;

    let owner = db(&state, move |s| s.pull_request_owner(&id)).await?;
    let listed = owner
        .pull_request
        .evidence
        .iter()
        .flat_map(|e| &e.gates)
        .flat_map(|g| &g.cases)
        .flat_map(|c| &c.artifacts)
        .find(|a| a.id == artifact_id)
        .cloned()
        .ok_or_else(ApiError::not_found)?;
    let rel = crate::store::artifact_path(&artifact_id).ok_or_else(ApiError::not_found)?;
    let (ws, task) = pr_center::owner_target(&owner).map_err(pr_error)?;
    let path = state
        .engine
        .gate_artifact(&ws, &task, &rel)
        .map_err(|_| ApiError::not_found())?;
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| ApiError::not_found())?;
    let content_type = axum::http::HeaderValue::from_str(&listed.content_type).unwrap_or(
        axum::http::HeaderValue::from_static("application/octet-stream"),
    );
    let disposition = format!(
        "inline; filename=\"{}\"",
        listed.name.replace(['"', '\\', '\r', '\n'], "_")
    );
    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (
                header::CONTENT_DISPOSITION,
                axum::http::HeaderValue::from_str(&disposition)
                    .unwrap_or(axum::http::HeaderValue::from_static("inline")),
            ),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                axum::http::HeaderValue::from_static("nosniff"),
            ),
            (
                header::CONTENT_SECURITY_POLICY,
                axum::http::HeaderValue::from_static("sandbox"),
            ),
        ],
        bytes,
    )
        .into_response())
}
