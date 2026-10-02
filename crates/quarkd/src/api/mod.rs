//! The `/v1` HTTP API and event stream.

mod error;
mod events;
mod openapi;
mod routes;

use std::sync::Arc;

use crate::chat::CoordinatorInput;
use crate::engine::EngineAdapter;
use axum::routing::{get, post};
use axum::Router;
use tower_http::trace::TraceLayer;

use crate::store::Store;

pub use error::ApiError;
pub use openapi::ApiDoc;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub engine: Arc<dyn EngineAdapter>,
    /// Delivers chat input to coordinator sessions.
    pub chat: Arc<dyn CoordinatorInput>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/health", get(routes::health))
        .route("/v1/openapi.json", get(openapi::serve))
        .route(
            "/v1/projects",
            get(routes::list_projects).post(routes::create_project),
        )
        .route(
            "/v1/projects/{id}",
            get(routes::get_project).patch(routes::update_project),
        )
        .route("/v1/projects/{id}/tasks", get(routes::list_tasks))
        // POST serves the custom methods `/v1/tasks/{id}:cancel` and
        // `:relaunch`; the router allows one parameter per segment.
        .route(
            "/v1/tasks/{id}",
            get(routes::get_task).post(routes::task_action),
        )
        .route("/v1/tasks/{id}/messages", post(routes::send_task_message))
        .route("/v1/tasks/{id}/transcript", get(routes::task_transcript))
        .route("/v1/decisions", get(routes::list_decisions))
        .route(
            "/v1/coordinators/{id}/messages",
            get(routes::coordinator_messages).post(routes::send_coordinator_message),
        )
        .route("/v1/events", get(events::stream))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Runs a blocking store call off the async runtime.
pub(crate) async fn db<T, F>(state: &AppState, f: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce(&Store) -> crate::store::Result<T> + Send + 'static,
{
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || f(&store))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .map_err(ApiError::from)
}
