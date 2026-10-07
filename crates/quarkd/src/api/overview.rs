//! The Project dashboard's Overview tab: live status and "since you last
//! looked", read from the native event log (`crate::overview`).

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::Json;
use quark_systems::{ErrorBody, LiveStatus, ProjectOverview};
use serde::Deserialize;
use time::OffsetDateTime;
use utoipa::IntoParams;

use super::{db, ApiError, AppState};

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct OverviewQuery {
    /// The `head` an earlier read returned: adds a digest of everything the
    /// Project logged after it. `0` digests the whole log.
    pub since: Option<u64>,
}

/// What is happening in a Project now and, with `since`, what happened
/// after that point in its event log.
///
/// Live status folds every task's status lines into its latest state, open
/// decisions and pull request; finished tasks stay listed for a day. The
/// digest counts spawns, results and decisions after `since` and lists the
/// notable ones, newest first. An unreadable event log is reported in
/// `error` rather than failing the call.
#[utoipa::path(
    get,
    path = "/v1/projects/{id}/overview",
    tag = "projects",
    params(("id" = String, Path, description = "Project id"), OverviewQuery),
    responses(
        (status = 200, body = ProjectOverview),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<OverviewQuery>,
) -> Result<Json<ProjectOverview>, ApiError> {
    let pid = id.clone();
    let (tasks, by_engine) = db(&state, move |s| {
        s.get_project(&pid)?;
        Ok((s.list_tasks(&pid)?, s.task_ids_by_engine(&pid)?))
    })
    .await?;
    let failed = |e: quark_core::CoreError| ProjectOverview {
        project_id: id.clone(),
        head: 0,
        live: LiveStatus::default(),
        digest: None,
        error: Some(e.to_string()),
    };
    let model = crate::overview::shared(&state.events);
    let mut model = model.lock().await;
    if let Err(e) = model.catch_up().await {
        return Ok(Json(failed(e)));
    }
    let mut overview = model.overview(&id, q.since, OffsetDateTime::now_utc());
    drop(model);
    // Engine ids to Quark tasks, so the app can link to the worker view.
    let titles: HashMap<String, String> = tasks.into_iter().map(|t| (t.id, t.title)).collect();
    let name = |engine: &str| {
        let id = by_engine.get(engine)?;
        Some((id.clone(), titles.get(id).cloned()))
    };
    for t in &mut overview.live.tasks {
        if let Some((id, title)) = name(&t.engine_task) {
            (t.task_id, t.title) = (Some(id), title);
        }
    }
    for h in overview
        .digest
        .iter_mut()
        .flat_map(|d| d.highlights.iter_mut())
    {
        if let Some((id, title)) = h.engine_task.as_deref().and_then(name) {
            (h.task_id, h.title) = (Some(id), title);
        }
    }
    Ok(Json(overview))
}

/// This module's endpoints, merged into the served document by
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(get))]
pub(super) struct Api;

/// This module's routes, merged into the `/v1` router.
pub(super) fn router() -> axum::Router<AppState> {
    use axum::routing::get;
    axum::Router::new().route("/v1/projects/{id}/overview", get(self::get))
}
