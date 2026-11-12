//! `/v1/update`: quarkd's updater (`crate::update`), which the app and
//! QuarkBar show and drive.

use axum::extract::State;
use axum::Json;

use super::{ApiError, AppState};
use crate::update::{Settings, UpdateStatus, Updater};

fn updater(state: &AppState) -> std::sync::Arc<Updater> {
    Updater::for_home(&state.layout.home)
}

/// What the updater last found: the offered release, a download that is
/// ready to install, or why updates are off.
#[utoipa::path(get, path = "/v1/update", tag = "daemon",
    responses((status = 200, body = UpdateStatus)))]
pub async fn get(State(state): State<AppState>) -> Json<UpdateStatus> {
    Json(updater(&state).status())
}

/// Checks the update feed now (Check for Updates…), and downloads what it
/// offers when the settings say to.
#[utoipa::path(post, path = "/v1/update/check", tag = "daemon",
    responses((status = 200, body = UpdateStatus)))]
pub async fn check(State(state): State<AppState>) -> Json<UpdateStatus> {
    Json(updater(&state).check().await)
}

/// Downloads and verifies the offered update.
#[utoipa::path(post, path = "/v1/update/download", tag = "daemon",
    responses((status = 200, body = UpdateStatus)))]
pub async fn download(State(state): State<AppState>) -> Json<UpdateStatus> {
    Json(updater(&state).download().await)
}

/// Replaces the update settings.
#[utoipa::path(put, path = "/v1/update/settings", tag = "daemon",
    request_body = Settings,
    responses((status = 200, body = UpdateStatus)))]
pub async fn put_settings(
    State(state): State<AppState>,
    Json(settings): Json<Settings>,
) -> Result<Json<UpdateStatus>, ApiError> {
    let u = updater(&state);
    u.set_settings(&settings)
        .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    Ok(Json(u.status()))
}

/// This module's endpoints, merged into the served document by
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(get, check, download, put_settings))]
pub(super) struct Api;

/// This module's routes, merged into the `/v1` router.
pub(super) fn router() -> axum::Router<AppState> {
    use axum::routing::{get, post, put};
    axum::Router::new()
        .route("/v1/update", get(self::get))
        .route("/v1/update/check", post(check))
        .route("/v1/update/download", post(download))
        .route("/v1/update/settings", put(put_settings))
}
