//! `GET /v1/status`: what needs you and how every Project and its agents are
//! doing, folded by `crate::status`.

use axum::extract::State;
use axum::Json;
use quark_systems::{DecisionState, StatusView};
use time::OffsetDateTime;

use super::{db, ApiError, AppState};

/// What waits on you across every Project, oldest first: open decisions,
/// open pull requests with a failing check, and failed or blocked workers
/// not already waiting on a decision. With it, each unarchived Project's
/// listed workers (unfinished, or failed in the last three days) by tone,
/// its open decisions and pull requests, and its latest activity.
///
/// Changes arrive as `task.*`, `decision.*`, `pr.updated`, `check.updated`
/// and `project.*` events; read again on any of them.
#[utoipa::path(
    get,
    path = "/v1/status",
    tag = "daemon",
    responses((status = 200, body = StatusView))
)]
pub async fn status(State(state): State<AppState>) -> Result<Json<StatusView>, ApiError> {
    let view = db(&state, |s| {
        let projects = s.list_projects()?;
        let mut tasks = Vec::new();
        for p in &projects {
            tasks.extend(s.list_tasks(&p.id)?);
        }
        let mut decisions = s.list_decisions(Some(DecisionState::Open))?;
        decisions.extend(s.list_decisions(Some(DecisionState::Answered))?);
        let prs = s.list_pull_requests(None, None)?;
        Ok(crate::status::fold(
            &projects,
            &tasks,
            &decisions,
            &prs,
            OffsetDateTime::now_utc(),
        ))
    })
    .await?;
    Ok(Json(view))
}

/// This module's endpoints, merged into the served document by
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(status))]
pub(super) struct Api;

/// This module's routes, merged into the `/v1` router.
pub(super) fn router() -> axum::Router<AppState> {
    use axum::routing::get;
    axum::Router::new().route("/v1/status", get(status))
}
