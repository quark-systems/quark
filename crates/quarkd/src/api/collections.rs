//! Collections: named sets of Projects the left list groups by. A Project
//! may be in several, or none. Nothing here reaches the engine; a
//! collection is what the person sees, and the app can turn the grouping
//! off and show every Project flat.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use quark_systems::{Collection, CreateCollection, ErrorBody, UpdateCollection};

use super::{db, ApiError, AppState};

/// Every collection with the Projects in it, in the order they are shown.
#[utoipa::path(
    get,
    path = "/v1/collections",
    tag = "collections",
    responses((status = 200, body = Vec<Collection>))
)]
pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<Collection>>, ApiError> {
    Ok(Json(db(&state, move |s| s.list_collections()).await?))
}

/// Add an empty collection, after the ones that exist.
#[utoipa::path(
    post,
    path = "/v1/collections",
    tag = "collections",
    request_body = CreateCollection,
    responses(
        (status = 200, body = Collection),
        (status = 400, body = ErrorBody)
    )
)]
pub async fn create(
    State(state): State<AppState>,
    Json(input): Json<CreateCollection>,
) -> Result<Json<Collection>, ApiError> {
    Ok(Json(db(&state, move |s| s.create_collection(input)).await?))
}

/// Rename a collection, move it among the others, or replace what is in it.
/// `projects` is the whole membership, in the order the left list shows.
#[utoipa::path(
    patch,
    path = "/v1/collections/{id}",
    tag = "collections",
    params(("id" = String, Path, description = "Collection id")),
    request_body = UpdateCollection,
    responses(
        (status = 200, body = Collection),
        (status = 400, description = "An empty name, or a project id that names no Project", body = ErrorBody),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<UpdateCollection>,
) -> Result<Json<Collection>, ApiError> {
    Ok(Json(
        db(&state, move |s| s.update_collection(&id, input)).await?,
    ))
}

/// Remove a collection. The Projects in it are untouched.
#[utoipa::path(
    delete,
    path = "/v1/collections/{id}",
    tag = "collections",
    params(("id" = String, Path, description = "Collection id")),
    responses(
        (status = 204, description = "Removed"),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    db(&state, move |s| s.delete_collection(&id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(utoipa::OpenApi)]
#[openapi(paths(list, create, update, delete))]
pub(super) struct Api;

/// This module's routes, merged into the `/v1` router.
pub(super) fn router() -> axum::Router<AppState> {
    use axum::routing::{get, patch};
    axum::Router::new()
        .route("/v1/collections", get(list).post(create))
        .route("/v1/collections/{id}", patch(update).delete(delete))
}
