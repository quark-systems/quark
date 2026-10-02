use axum::Json;
use utoipa::OpenApi;

use super::{events, routes};

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Quark daemon API",
        description = "Local control plane API served by quarkd on localhost.",
        license(name = "MIT")
    ),
    paths(
        routes::health,
        routes::list_projects,
        routes::create_project,
        routes::get_project,
        routes::update_project,
        routes::provision_project,
        routes::list_tasks,
        routes::get_task,
        routes::send_task_message,
        routes::cancel_task,
        routes::relaunch_task,
        routes::list_decisions,
        events::stream,
    ),
    components(schemas(
        quark_systems::Event,
        quark_systems::EventType,
        quark_systems::TaskState,
        quark_systems::TaskKind,
        quark_systems::DecisionState,
        quark_systems::ProjectStatus,
        quark_systems::DeliveryPolicy,
        quark_systems::DispatchPreset,
    )),
    tags(
        (name = "daemon"),
        (name = "projects"),
        (name = "tasks"),
        (name = "decisions"),
        (name = "events")
    )
)]
pub struct ApiDoc;

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
