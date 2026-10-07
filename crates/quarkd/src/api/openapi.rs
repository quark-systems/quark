use axum::Json;
use utoipa::OpenApi;

use super::MODULES;

/// The document's info and tag order. Paths and schemas come from each API
/// module's own `Api`.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Quark daemon API",
        description = "Local control plane API served by quarkd on localhost.",
        license(name = "MIT")
    ),
    tags(
        (name = "daemon"),
        (name = "projects"),
        (name = "tasks"),
        (name = "decisions"),
        (name = "memory"),
        (name = "pull-requests"),
        (name = "harnesses"),
        (name = "accounts"),
        (name = "terminals"),
        (name = "coordinators"),
        (name = "events")
    )
)]
struct Root;

/// The served OpenAPI document: [`Root`] merged with every module's `Api`.
pub struct ApiDoc;

impl OpenApi for ApiDoc {
    fn openapi() -> utoipa::openapi::OpenApi {
        let mut doc = Root::openapi();
        for m in MODULES {
            doc.merge((m.openapi)());
        }
        doc
    }
}

impl ApiDoc {
    /// The OpenAPI document as pretty JSON, with a trailing newline.
    pub fn json() -> String {
        let mut s = ApiDoc::openapi()
            .to_pretty_json()
            .expect("OpenAPI document serializes");
        s.push('\n');
        s
    }
}

/// The OpenAPI document for this API.
pub async fn serve() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}
