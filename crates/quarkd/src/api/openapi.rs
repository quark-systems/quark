use axum::Json;
use utoipa::OpenApi;

use super::{events, harnesses, routes, terminals};

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
        harnesses::list,
        harnesses::validate,
        terminals::list_terminals,
        terminals::get_terminal,
        terminals::input,
        terminals::resize,
        terminals::snapshot,
        routes::task_transcript,
        routes::coordinator_messages,
        routes::send_coordinator_message,
        events::stream,
    ),
    components(schemas(
        quark_systems::Event,
        quark_systems::EventType,
        quark_systems::TaskState,
        quark_systems::TaskKind,
        quark_systems::DecisionState,
        quark_systems::TerminalRole,
        quark_systems::TerminalChunkKind,
        quark_systems::TerminalOutput,
        quark_systems::ProjectStatus,
        quark_systems::DeliveryPolicy,
        quark_systems::DispatchPreset,
        quark_systems::TranscriptEntry,
        quark_systems::TranscriptRole,
    )),
    tags(
        (name = "daemon"),
        (name = "projects"),
        (name = "tasks"),
        (name = "decisions"),
        (name = "harnesses"),
        (name = "terminals"),
        (name = "coordinators"),
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
