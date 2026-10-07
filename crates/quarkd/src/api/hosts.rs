//! The Hosts view and each Project's slice of its hosts, read from the
//! native event log (`crate::hosts`).

use axum::extract::{Path, Query, State};
use axum::Json;
use quark_systems::{ErrorBody, HostsView, ProjectHosts};
use serde::Deserialize;
use time::OffsetDateTime;
use utoipa::IntoParams;

use super::{db, ApiError, AppState};
use crate::hosts::Names;

/// Hours of history when a read names none.
const DEFAULT_HOURS: u32 = 6;

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct HostsQuery {
    /// Hours of history in each series, ending now: 1 to 48, default 6.
    pub hours: Option<u32>,
}

/// Project names and Quark task ids and titles, for every Project or one.
async fn names(state: &AppState, only: Option<String>) -> Result<Names, ApiError> {
    db(state, move |s| {
        let mut names = Names::default();
        for p in s.list_projects()? {
            if only.as_ref().is_some_and(|o| o != &p.id) {
                continue;
            }
            let titles: std::collections::HashMap<String, String> = s
                .list_tasks(&p.id)?
                .into_iter()
                .map(|t| (t.id, t.title))
                .collect();
            for (engine, id) in s.task_ids_by_engine(&p.id)? {
                let title = titles.get(&id).cloned().unwrap_or_default();
                names.tasks.insert((p.id.clone(), engine), (id, title));
            }
            names.projects.insert(p.id, p.name);
        }
        Ok(names)
    })
    .await
}

/// Every host Quark knows: identity, platform, capacity and health, the
/// newest telemetry sample and a series over `hours`, each Project's share
/// of the newest sample, and the host's worktree pools.
///
/// Hosts register and are sampled once a minute while host telemetry is on
/// (`QUARK_HOST_TELEMETRY`, on unless `0`). An unreadable event log is
/// reported in `error` rather than failing the call.
#[utoipa::path(
    get,
    path = "/v1/hosts",
    tag = "hosts",
    params(HostsQuery),
    responses((status = 200, body = HostsView))
)]
pub async fn list(
    State(state): State<AppState>,
    Query(q): Query<HostsQuery>,
) -> Result<Json<HostsView>, ApiError> {
    let hours = q.hours.unwrap_or(DEFAULT_HOURS);
    let names = names(&state, None).await?;
    let model = crate::hosts::shared(&state.events);
    let mut model = model.lock().await;
    if let Err(e) = model.catch_up().await {
        return Ok(Json(HostsView {
            hours,
            hosts: Vec::new(),
            error: Some(e.to_string()),
        }));
    }
    Ok(Json(model.view(hours, OffsetDateTime::now_utc(), &names)))
}

/// One Project's slice of every host it used in the last `hours` or has
/// worktrees on: its CPU, memory and worktree disk now and over the window,
/// broken down by coordinator and task, beside the host's own readings.
#[utoipa::path(
    get,
    path = "/v1/projects/{id}/hosts",
    tag = "projects",
    params(("id" = String, Path, description = "Project id"), HostsQuery),
    responses(
        (status = 200, body = ProjectHosts),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn project(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<HostsQuery>,
) -> Result<Json<ProjectHosts>, ApiError> {
    let hours = q.hours.unwrap_or(DEFAULT_HOURS);
    let pid = id.clone();
    db(&state, move |s| s.get_project(&pid).map(drop)).await?;
    let names = names(&state, Some(id.clone())).await?;
    let model = crate::hosts::shared(&state.events);
    let mut model = model.lock().await;
    if let Err(e) = model.catch_up().await {
        return Ok(Json(ProjectHosts {
            project_id: id,
            hours,
            hosts: Vec::new(),
            error: Some(e.to_string()),
        }));
    }
    Ok(Json(model.project(
        &id,
        hours,
        OffsetDateTime::now_utc(),
        &names,
    )))
}

/// This module's endpoints, merged into the served document by
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(list, project))]
pub(super) struct Api;

/// This module's routes, merged into the `/v1` router.
pub(super) fn router() -> axum::Router<AppState> {
    use axum::routing::get;
    axum::Router::new()
        .route("/v1/hosts", get(list))
        .route("/v1/projects/{id}/hosts", get(project))
}
