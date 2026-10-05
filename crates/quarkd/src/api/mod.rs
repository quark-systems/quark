//! The `/v1` HTTP API and event stream.

mod accounts;
mod dispatch;
mod error;
mod events;
mod harnesses;
mod memory;
mod openapi;
mod pull_requests;
mod routes;
mod terminals;

use std::sync::Arc;

use crate::chat::CoordinatorInput;
use crate::engine::EngineAdapter;
use crate::harness::HarnessRegistry;
use crate::sessions::Sessions;
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
    pub harnesses: Arc<HarnessRegistry>,
    /// Accounts, pools and quota per harness.
    pub accounts: Arc<crate::accounts::Accounts>,
    pub sessions: Sessions,
    /// Where new Project workspaces and Project repos go.
    pub layout: crate::provision::Layout,
    /// Delivers chat input to coordinator sessions.
    pub chat: Arc<dyn CoordinatorInput>,
    /// Reads pull requests from their forge.
    pub forge: Arc<dyn crate::forge::Forge>,
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
        .route("/v1/projects/{id}/dispatch:test", post(dispatch::test))
        .route("/v1/projects/{id}/memory", get(memory::list_entries))
        // POST serves the custom method `.../memory/{entry_id}:promote`.
        .route(
            "/v1/projects/{id}/memory/{entry_id}",
            post(memory::entry_action),
        )
        .route(
            "/v1/projects/{id}/memory/commits/{commit}",
            get(memory::get_commit),
        )
        .route("/v1/memory", get(memory::list_user_entries))
        .route(
            "/v1/projects/{id}/memory/proposals",
            get(memory::list_proposals),
        )
        // POST serves the custom methods `.../{proposal_id}:accept` and `:reject`.
        .route(
            "/v1/projects/{id}/memory/proposals/{proposal_id}",
            post(memory::proposal_action),
        )
        // POST serves the custom methods `/v1/tasks/{id}:cancel` and
        // `:relaunch`; the router allows one parameter per segment.
        .route(
            "/v1/tasks/{id}",
            get(routes::get_task).post(routes::task_action),
        )
        .route("/v1/tasks/{id}/messages", post(routes::send_task_message))
        .route(
            "/v1/projects/{id}/terminals",
            get(terminals::list_terminals),
        )
        .route("/v1/terminals/{id}", get(terminals::get_terminal))
        .route("/v1/terminals/{id}/input", post(terminals::input))
        .route("/v1/terminals/{id}/resize", post(terminals::resize))
        .route("/v1/terminals/{id}/snapshot", post(terminals::snapshot))
        .route("/v1/tasks/{id}/transcript", get(routes::task_transcript))
        .route("/v1/tasks/{id}/events", get(routes::list_task_events))
        .route("/v1/tasks/{id}/dispatch", get(routes::list_task_dispatch))
        .route("/v1/tasks/{id}/changes", get(routes::get_task_changes))
        .route("/v1/tasks/{id}/diff", get(routes::get_task_diff))
        .route("/v1/decisions", get(routes::list_decisions))
        // POST serves the custom method `/v1/decisions/{id}:answer`.
        .route("/v1/decisions/{id}", post(routes::decision_action))
        .route("/v1/pull-requests", get(pull_requests::list))
        // POST serves the custom method `/v1/pull-requests/{id}:merge`.
        .route(
            "/v1/pull-requests/{id}",
            get(pull_requests::get).post(pull_requests::action),
        )
        .route("/v1/pull-requests/{id}/diff", get(pull_requests::diff))
        .route(
            "/v1/pull-requests/{id}/evidence/artifacts/{artifact_id}",
            get(pull_requests::artifact),
        )
        .route(
            "/v1/pull-requests/{id}/comments",
            post(pull_requests::comment),
        )
        .route("/v1/accounts", get(accounts::list).post(accounts::create))
        .route(
            "/v1/accounts/{id}",
            get(accounts::get)
                .patch(accounts::update)
                .delete(accounts::delete),
        )
        .route("/v1/harnesses", get(harnesses::list))
        .route("/v1/harnesses:validate", post(harnesses::validate))
        .route(
            "/v1/coordinators/{id}/messages",
            get(routes::coordinator_messages).post(routes::send_coordinator_message),
        )
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
        .allow_methods([Method::GET, Method::POST, Method::PATCH, Method::DELETE])
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
