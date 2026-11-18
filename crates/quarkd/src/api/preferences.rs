//! App-wide preferences: how the app shows what is there. Kept on the
//! daemon so every client it serves opens the same way.

use axum::extract::State;
use axum::Json;
use quark_systems::{ErrorBody, Preferences, UpdatePreferences};

use super::{db, ApiError, AppState};

/// Every app-wide preference, with its default where none is set.
#[utoipa::path(
    get,
    path = "/v1/preferences",
    tag = "daemon",
    responses((status = 200, body = Preferences))
)]
pub async fn get(State(state): State<AppState>) -> Result<Json<Preferences>, ApiError> {
    Ok(Json(db(&state, move |s| s.preferences()).await?))
}

/// Change the preferences named, leaving the rest as they are.
#[utoipa::path(
    patch,
    path = "/v1/preferences",
    tag = "daemon",
    request_body = UpdatePreferences,
    responses(
        (status = 200, body = Preferences),
        (status = 400, body = ErrorBody)
    )
)]
pub async fn patch(
    State(state): State<AppState>,
    Json(input): Json<UpdatePreferences>,
) -> Result<Json<Preferences>, ApiError> {
    Ok(Json(db(&state, move |s| s.set_preferences(input)).await?))
}

#[derive(utoipa::OpenApi)]
#[openapi(paths(get, patch))]
pub(super) struct Api;

/// This module's routes, merged into the `/v1` router.
pub(super) fn router() -> axum::Router<AppState> {
    use axum::routing::get as get_route;
    axum::Router::new().route("/v1/preferences", get_route(get).patch(patch))
}
