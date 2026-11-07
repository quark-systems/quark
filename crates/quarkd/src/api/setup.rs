//! `GET /v1/setup` and `POST /v1/setup/{id}`: the first-run checks
//! (`crate::setup`, also `quarkd doctor`) and the fixes quarkd can run.

use axum::extract::{Path, State};
use axum::Json;

use super::{ApiError, AppState};
use crate::setup::{self, FixOutcome, SetupReport};

/// What this machine still needs before Quark can run a Project: tools
/// (git, tmux 3.2+, gh signed in, a coordinator agent signed in,
/// treehouse), optional ones (Beads, Dolt, quota-axi) and the home. Each
/// missing check may carry a fix.
#[utoipa::path(
    get,
    path = "/v1/setup",
    tag = "daemon",
    responses((status = 200, body = SetupReport))
)]
pub async fn report(State(state): State<AppState>) -> Json<SetupReport> {
    Json(setup::check(&state.layout.home, &state.harnesses).await)
}

/// Run check `id`'s fix, when quarkd can run it without a prompt (a
/// Homebrew or npm install, or the pinned treehouse download), and check
/// again. Sign-ins can't be run here; their fix names the command.
#[utoipa::path(
    post,
    path = "/v1/setup/{id}",
    tag = "daemon",
    params(("id" = String, Path, description = "The check's id, e.g. `tmux`")),
    responses(
        (status = 200, body = FixOutcome),
        (status = 400, description = "No such check, or no fix quarkd can run"),
    )
)]
pub async fn fix(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<FixOutcome>, ApiError> {
    setup::fix(&state.layout.home, &state.harnesses, &id)
        .await
        .map(Json)
        .map_err(ApiError::invalid)
}

/// This module's endpoints, merged into the served document by
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(report, fix))]
pub(super) struct Api;

/// This module's routes, merged into the `/v1` router.
pub(super) fn router() -> axum::Router<AppState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/v1/setup", get(report))
        .route("/v1/setup/{id}", post(fix))
}
