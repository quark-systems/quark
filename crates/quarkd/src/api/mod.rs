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
use axum::routing::get;
use axum::Router;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use utoipa::OpenApi;

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

/// One API module: its routes and its part of the OpenAPI document.
struct Module {
    router: fn() -> Router<AppState>,
    openapi: fn() -> utoipa::openapi::OpenApi,
}

/// Every API module. A new group of endpoints gets its own module with an
/// `Api` and a `router()`, and one line here.
const MODULES: &[Module] = &[
    Module {
        router: routes::router,
        openapi: <routes::Api as OpenApi>::openapi,
    },
    Module {
        router: dispatch::router,
        openapi: <dispatch::Api as OpenApi>::openapi,
    },
    Module {
        router: memory::router,
        openapi: <memory::Api as OpenApi>::openapi,
    },
    Module {
        router: pull_requests::router,
        openapi: <pull_requests::Api as OpenApi>::openapi,
    },
    Module {
        router: harnesses::router,
        openapi: <harnesses::Api as OpenApi>::openapi,
    },
    Module {
        router: accounts::router,
        openapi: <accounts::Api as OpenApi>::openapi,
    },
    Module {
        router: terminals::router,
        openapi: <terminals::Api as OpenApi>::openapi,
    },
    Module {
        router: events::router,
        openapi: <events::Api as OpenApi>::openapi,
    },
];

pub fn router(state: AppState) -> Router {
    let mut router = Router::new().route("/v1/openapi.json", get(openapi::serve));
    for m in MODULES {
        router = router.merge((m.router)());
    }
    router
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
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ])
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
