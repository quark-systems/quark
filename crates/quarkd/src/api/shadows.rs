//! How ready each native slice is to switch on, from its shadow's
//! divergences in the native event log (`crate::shadows`).

use axum::extract::{Query, State};
use axum::Json;
use quark_systems::ShadowReadiness;
use serde::Deserialize;
use time::OffsetDateTime;
use utoipa::IntoParams;

use super::AppState;
use crate::shadows::DEFAULT_DAYS;

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ShadowsQuery {
    /// Days of history to count, ending now: 1 to 90, default 7.
    pub days: Option<u32>,
}

/// Each slice's shadow: whether it compares with bash, whether it ran on
/// the daemon's latest start and since when, its divergences over `days`
/// per operation, and the newest few with both sides.
///
/// `QUARK_SHADOWS=all` turns on every shadow. A slice reads `agreeing`
/// once its shadow ran for the whole window with no divergences; it still
/// needs the verify-quark durability journeys before it switches native.
/// An unreadable event log is reported in `error` rather than failing the
/// call.
#[utoipa::path(
    get,
    path = "/v1/shadows",
    tag = "daemon",
    params(ShadowsQuery),
    responses((status = 200, body = ShadowReadiness))
)]
pub async fn get(
    State(state): State<AppState>,
    Query(q): Query<ShadowsQuery>,
) -> Json<ShadowReadiness> {
    let days = q.days.unwrap_or(DEFAULT_DAYS).clamp(1, 90);
    let model = crate::shadows::shared(&state.events);
    let mut model = model.lock().await;
    if let Err(e) = model.catch_up().await {
        return Json(ShadowReadiness {
            days,
            latest_start: None,
            slices: Vec::new(),
            error: Some(e.to_string()),
        });
    }
    Json(model.report(days, OffsetDateTime::now_utc()))
}

/// This module's endpoints, merged into the served document by
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(get))]
pub(super) struct Api;

/// This module's routes, merged into the `/v1` router.
pub(super) fn router() -> axum::Router<AppState> {
    use axum::routing::get;
    axum::Router::new().route("/v1/shadows", get(self::get))
}
