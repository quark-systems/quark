use axum::extract::{Query, State};
use axum::Json;
use quark_systems::{AgentConfigValidation, AgentRole, HarnessInfo, ValidateAgentConfig};
use serde::Deserialize;
use utoipa::IntoParams;

use super::AppState;

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct HarnessQuery {
    /// Probe the executables again instead of using the recent result.
    #[serde(default)]
    pub refresh: bool,
}

/// Every harness the daemon can run, with install state, accepted models
/// and effort, and credential health for the default account.
#[utoipa::path(
    get,
    path = "/v1/harnesses",
    tag = "harnesses",
    params(HarnessQuery),
    responses((status = 200, body = Vec<HarnessInfo>))
)]
pub async fn list(
    State(state): State<AppState>,
    Query(q): Query<HarnessQuery>,
) -> Json<Vec<HarnessInfo>> {
    Json(state.harnesses.list(q.refresh).await)
}

/// Check an agent config before it is saved. Problems come back in the body
/// with `valid: false`, not as an error status, so a picker can show them.
#[utoipa::path(
    post,
    path = "/v1/harnesses:validate",
    tag = "harnesses",
    request_body = ValidateAgentConfig,
    responses((status = 200, body = AgentConfigValidation))
)]
pub async fn validate(
    State(state): State<AppState>,
    Json(input): Json<ValidateAgentConfig>,
) -> Json<AgentConfigValidation> {
    let role = input.role.unwrap_or(AgentRole::Worker);
    Json(state.harnesses.validate(&input.config, role))
}

/// This module's endpoints, plus schemas the generator does not reach from
/// them (event payloads), merged into the served document by
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(list, validate))]
pub(super) struct Api;

/// This module's routes, merged into the `/v1` router.
pub(super) fn router() -> axum::Router<AppState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/v1/harnesses", get(list))
        .route("/v1/harnesses:validate", post(validate))
}
