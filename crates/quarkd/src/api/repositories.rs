//! Repositories the forge account can reach, for the repository picker on
//! New project.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use quark_systems::{ErrorBody, ForgeRepository};
use serde::Deserialize;
use utoipa::IntoParams;

use super::{ApiError, AppState};
use crate::forge::ForgeError;

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RepositoryQuery {
    /// Ask GitHub again instead of using the list from the last two minutes.
    #[serde(default)]
    pub refresh: bool,
}

/// GitHub repositories the account `gh` is logged in to owns, collaborates
/// on or reaches through its organizations, most recently pushed first (at
/// most 1000). 502 with `forge_unavailable` when gh is missing, and
/// `forge_failed` when it is not logged in or GitHub refused.
#[utoipa::path(
    get,
    path = "/v1/forge/repositories",
    tag = "pull-requests",
    params(RepositoryQuery),
    responses(
        (status = 200, body = Vec<ForgeRepository>),
        (status = 502, body = ErrorBody)
    )
)]
pub async fn list(
    State(state): State<AppState>,
    Query(q): Query<RepositoryQuery>,
) -> Result<Json<Vec<ForgeRepository>>, ApiError> {
    state
        .forge
        .repositories(q.refresh)
        .await
        .map(Json)
        .map_err(|e| {
            let code = match e {
                ForgeError::Missing => "forge_unavailable",
                _ => "forge_failed",
            };
            ApiError::new(StatusCode::BAD_GATEWAY, code, e.to_string())
        })
}

#[derive(utoipa::OpenApi)]
#[openapi(paths(list))]
pub(super) struct Api;

pub(super) fn router() -> axum::Router<AppState> {
    axum::Router::new().route("/v1/forge/repositories", axum::routing::get(list))
}
