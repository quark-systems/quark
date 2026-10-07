//! The Project dashboard's Metrics tab: how the Project's work has gone,
//! computed from the event log.

use std::collections::BTreeMap;

use axum::extract::{Path, Query, State};
use axum::Json;
use quark_core::{EventLog, Seq};
use quark_systems::{AccountUse, ErrorBody, ProjectMetrics};
use serde::Deserialize;
use time::OffsetDateTime;
use utoipa::IntoParams;

use super::{db, ApiError, AppState};
use crate::metrics;

/// Events read from the log per page.
const PAGE: usize = 2000;

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct MetricsQuery {
    /// Days covered, ending now: 1 to 90, default 7.
    pub days: Option<u32>,
}

/// Throughput, lead time, gate pass and first-time-green, interventions,
/// failovers and the quota of the accounts the Project's tasks ran under.
///
/// Computed from the event log (`events.db`) on each call; failovers come
/// from the tasks that recorded them and quota from the latest reading.
/// Metrics the daemon cannot compute yet are listed in `unavailable`.
#[utoipa::path(
    get,
    path = "/v1/projects/{id}/metrics",
    tag = "projects",
    params(("id" = String, Path, description = "Project id"), MetricsQuery),
    responses(
        (status = 200, body = ProjectMetrics),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<MetricsQuery>,
) -> Result<Json<ProjectMetrics>, ApiError> {
    let pid = id.clone();
    let project = db(&state, move |s| s.get_project(&pid)).await?;
    let pid = project.id.clone();
    let tasks = db(&state, move |s| s.list_tasks(&pid)).await?;

    let mut events = Vec::new();
    let mut after = Seq::ZERO;
    loop {
        let page = state
            .events
            .read(after, PAGE)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;
        let Some(last) = page.last() else { break };
        after = last.seq;
        let full = page.len() == PAGE;
        events.extend(
            page.into_iter()
                .filter(|e| e.project.as_str() == project.id),
        );
        if !full {
            break;
        }
    }

    let failovers: Vec<_> = tasks.iter().flat_map(|t| t.failovers.clone()).collect();
    let mut m = metrics::compute(
        &project.id,
        &events,
        &failovers,
        OffsetDateTime::now_utc(),
        q.days.unwrap_or(metrics::DEFAULT_DAYS),
    );

    // Accounts the Project's tasks ran under, or moved to on a failover.
    let mut used: BTreeMap<String, u32> = BTreeMap::new();
    for t in &tasks {
        if let Some(a) = &t.account_id {
            *used.entry(a.clone()).or_default() += 1;
        }
        for f in &t.failovers {
            if let Some(a) = &f.to_account_id {
                used.entry(a.clone()).or_default();
            }
        }
    }
    if !used.is_empty() {
        for a in state.accounts.list().await? {
            if let Some(&tasks) = used.get(&a.id) {
                m.accounts.push(AccountUse {
                    account_id: a.id,
                    harness: a.harness,
                    label: a.label,
                    tasks,
                    quota: a.quota,
                });
            }
        }
    }
    Ok(Json(m))
}

/// This module's endpoints, merged into the served document by
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(get))]
pub(super) struct Api;

/// This module's routes, merged into the `/v1` router.
pub(super) fn router() -> axum::Router<AppState> {
    use axum::routing::get;
    axum::Router::new().route("/v1/projects/{id}/metrics", get(self::get))
}
