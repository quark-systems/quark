//! The `/v1` HTTP API and event stream.

mod error;
mod events;
mod openapi;
mod routes;

use std::sync::Arc;

use crate::engine::EngineAdapter;
use axum::http::{header, HeaderValue, Method};
use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::store::Store;

pub use error::ApiError;
pub use openapi::ApiDoc;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub engine: Arc<dyn EngineAdapter>,
    /// Where new Project workspaces and Project repos go.
    pub layout: crate::provision::Layout,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/health", get(routes::health))
        .route("/v1/openapi.json", get(openapi::serve))
        .route(
            "/v1/projects",
            get(routes::list_projects).post(routes::create_project),
        )
        // POST serves the custom method `/v1/projects/{id}:provision`.
        .route(
            "/v1/projects/{id}",
            get(routes::get_project)
                .patch(routes::update_project)
                .post(routes::project_action),
        )
        .route("/v1/projects/{id}/tasks", get(routes::list_tasks))
        // POST serves the custom methods `/v1/tasks/{id}:cancel` and
        // `:relaunch`; the router allows one parameter per segment.
        .route(
            "/v1/tasks/{id}",
            get(routes::get_task).post(routes::task_action),
        )
        .route("/v1/tasks/{id}/messages", post(routes::send_task_message))
        .route("/v1/decisions", get(routes::list_decisions))
        .route("/v1/events", get(events::stream))
        .layer(cors())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Origins of the desktop app: the Tauri webview (macOS/Linux and Windows) and the
/// Vite dev server. Browsers do not send `Origin` for WebSocket upgrades through CORS,
/// so this only governs REST calls.
pub const APP_ORIGINS: &[&str] = &[
    "tauri://localhost",
    "http://tauri.localhost",
    "http://localhost:1420",
    "http://127.0.0.1:1420",
];

fn cors() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(
            APP_ORIGINS
                .iter()
                .map(|o| HeaderValue::from_static(o))
                .collect::<Vec<_>>(),
        )
        .allow_methods([Method::GET, Method::POST, Method::PATCH])
        .allow_headers([header::CONTENT_TYPE])
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
