//! Terminal routes: list, input, resize and snapshot for session panes.
//! Output arrives on the event stream as `worker.output`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use base64::Engine as _;
use quark_systems::{ErrorBody, Event, Terminal, TerminalInput, TerminalResize};

use super::{db, ApiError, AppState};
use crate::sessions::{ControlError, SessionError};

impl From<SessionError> for ApiError {
    fn from(e: SessionError) -> Self {
        match e {
            SessionError::NotFound => ApiError::not_found(),
            SessionError::Invalid(m) => ApiError::invalid(m),
            e @ SessionError::StaleInput { .. } => {
                ApiError::new(StatusCode::CONFLICT, "stale_input", e.to_string())
            }
            e @ (SessionError::Unavailable(_) | SessionError::Control(ControlError::Closed)) => {
                ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "unavailable",
                    e.to_string(),
                )
            }
            SessionError::Store(e) => e.into(),
            e => ApiError::new(StatusCode::BAD_GATEWAY, "session_error", e.to_string()),
        }
    }
}

/// Live terminals of one Project: its coordinator and its workers.
#[utoipa::path(
    get,
    path = "/v1/projects/{id}/terminals",
    tag = "terminals",
    params(("id" = String, Path, description = "Project id")),
    responses(
        (status = 200, body = Vec<Terminal>),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn list_terminals(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<Terminal>>, ApiError> {
    let project = id.clone();
    db(&state, move |s| s.get_project(&project)).await?;
    Ok(Json(state.sessions.list(&id)))
}

/// One terminal. Its id is the task id for a worker and the Project id for
/// a coordinator.
#[utoipa::path(
    get,
    path = "/v1/terminals/{id}",
    tag = "terminals",
    params(("id" = String, Path, description = "Terminal id")),
    responses(
        (status = 200, body = Terminal),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn get_terminal(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Terminal>, ApiError> {
    Ok(Json(state.sessions.get(&id)?))
}

/// Type raw bytes into a terminal.
///
/// Requests for one terminal are applied in the order they arrive, and HTTP
/// requests can overtake each other, so keep one request in flight per
/// terminal and coalesce keys typed meanwhile, or send `seq`.
#[utoipa::path(
    post,
    path = "/v1/terminals/{id}/input",
    tag = "terminals",
    params(("id" = String, Path, description = "Terminal id")),
    request_body = TerminalInput,
    responses(
        (status = 204, description = "Typed"),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`seq` is not after the last applied one"),
        (status = 503, body = ErrorBody)
    )
)]
pub async fn input(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<TerminalInput>,
) -> Result<StatusCode, ApiError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(input.data_b64.as_bytes())
        .map_err(|e| ApiError::invalid(format!("data_b64 is not base64: {e}")))?;
    state.sessions.input(&id, &bytes, input.seq).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Resize a terminal. The last size set wins for every viewer.
#[utoipa::path(
    post,
    path = "/v1/terminals/{id}/resize",
    tag = "terminals",
    params(("id" = String, Path, description = "Terminal id")),
    request_body = TerminalResize,
    responses(
        (status = 200, body = Terminal),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn resize(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(size): Json<TerminalResize>,
) -> Result<Json<Terminal>, ApiError> {
    Ok(Json(
        state.sessions.resize(&id, size.cols, size.rows).await?,
    ))
}

/// Snapshot a terminal's screen.
///
/// Appends a `worker.output` snapshot event and returns it. To open a
/// terminal, subscribe to the event stream, call this, apply the returned
/// snapshot, then apply the terminal's events with a greater `seq`.
#[utoipa::path(
    post,
    path = "/v1/terminals/{id}/snapshot",
    tag = "terminals",
    params(("id" = String, Path, description = "Terminal id")),
    responses(
        (status = 200, body = Event),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn snapshot(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Event>, ApiError> {
    Ok(Json(state.sessions.snapshot(&id).await?))
}

/// This module's endpoints, plus schemas the generator does not reach from
/// them (event payloads), merged into the served document by
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(list_terminals, get_terminal, input, resize, snapshot),
    components(schemas(
        quark_systems::TerminalRole,
        quark_systems::TerminalChunkKind,
        quark_systems::TerminalOutput,
    ))
)]
pub(super) struct Api;

/// This module's routes, merged into the `/v1` router.
pub(super) fn router() -> axum::Router<AppState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/v1/projects/{id}/terminals", get(list_terminals))
        .route("/v1/terminals/{id}", get(get_terminal))
        .route("/v1/terminals/{id}/input", post(input))
        .route("/v1/terminals/{id}/resize", post(resize))
        .route("/v1/terminals/{id}/snapshot", post(snapshot))
}
