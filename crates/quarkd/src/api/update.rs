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

/// Installs the downloaded update (Install and restart): backs up the home
/// and starts quark-updater, which swaps Quark.app, restarts quarkd and
/// rolls back if the new version doesn't come up. Answers before quarkd
/// restarts; `state` is `installing` until it has.
#[utoipa::path(post, path = "/v1/update/install", tag = "daemon",
    responses((status = 200, body = UpdateStatus),
              (status = 400, description = "Nothing to install, or not an installed app")))]
pub async fn install(State(state): State<AppState>) -> Result<Json<UpdateStatus>, ApiError> {
    let u = updater(&state);
    tokio::task::spawn_blocking(move || u.install())
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .map(Json)
        .map_err(|e| ApiError::invalid(format!("{e:#}")))
}

/// Goes back to the Quark the last update replaced (Roll back), restoring
/// the pre-update backup when that version can't read the newer data.
#[utoipa::path(post, path = "/v1/update/rollback", tag = "daemon",
    responses((status = 200, body = UpdateStatus),
              (status = 400, description = "No earlier Quark is kept")))]
pub async fn rollback(State(state): State<AppState>) -> Result<Json<UpdateStatus>, ApiError> {
    let u = updater(&state);
    tokio::task::spawn_blocking(move || u.roll_back())
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .map(Json)
        .map_err(|e| ApiError::invalid(format!("{e:#}")))
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
#[openapi(paths(get, check, download, install, rollback, put_settings))]
pub(super) struct Api;

/// This module's routes, merged into the `/v1` router.
pub(super) fn router() -> axum::Router<AppState> {
    use axum::routing::{get, post, put};
    axum::Router::new()
        .route("/v1/update", get(self::get))
        .route("/v1/update/check", post(check))
        .route("/v1/update/download", post(download))
        .route("/v1/update/install", post(install))
        .route("/v1/update/rollback", post(rollback))
        .route("/v1/update/settings", put(put_settings))
}
