//! `GET /v1/diagnostics`: what went wrong lately, folded by
//! `crate::diagnose` (also `quarkd diagnose`).

use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;
use time::{Duration, OffsetDateTime};

use super::{ApiError, AppState};
use crate::diagnose::{self, Diagnostics};

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct DiagnosticsQuery {
    /// Hours back the report covers; 24 when unset.
    pub hours: Option<u32>,
}

/// Trouble in this Quark home over the last `hours`: warnings and errors
/// the daemon logged, failed engine reads, supervisor and dispatch
/// trouble, quiet workers, waiting decisions and an oversized store. Each
/// finding's fingerprint stays the same while the trouble does.
#[utoipa::path(
    get,
    path = "/v1/diagnostics",
    tag = "daemon",
    params(DiagnosticsQuery),
    responses((status = 200, body = Diagnostics))
)]
pub async fn diagnostics(
    State(state): State<AppState>,
    Query(q): Query<DiagnosticsQuery>,
) -> Result<Json<Diagnostics>, ApiError> {
    let home = state.layout.home.clone();
    let hours = q
        .hours
        .map_or(diagnose::DEFAULT_WINDOW, |h| Duration::hours(h.into()));
    let report = tokio::task::spawn_blocking(move || {
        let now = OffsetDateTime::now_utc();
        diagnose::diagnose(&home, now - hours, now)
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(report))
}

/// This module's endpoints, merged into the served document by
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(diagnostics))]
pub(super) struct Api;

/// This module's routes, merged into the `/v1` router.
pub(super) fn router() -> axum::Router<AppState> {
    use axum::routing::get;
    axum::Router::new().route("/v1/diagnostics", get(diagnostics))
}
