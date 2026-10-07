use axum::Json;
use utoipa::OpenApi;

use super::{accounts, dispatch, events, harnesses, memory, pull_requests, routes, terminals};

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
        dispatch::get,
        dispatch::put,
        dispatch::test,
        routes::list_tasks,
        routes::get_task,
        routes::send_task_message,
        routes::cancel_task,
        routes::relaunch_task,
        routes::list_task_events,
        routes::list_task_dispatch,
        routes::get_task_changes,
        routes::get_task_diff,
        routes::list_decisions,
        routes::answer_decision,
        memory::list_proposals,
        memory::accept,
        memory::reject,
        memory::list_entries,
        memory::get_commit,
        memory::promote,
        memory::list_user_entries,
        pull_requests::list,
        pull_requests::get,
        pull_requests::diff,
        pull_requests::comment,
        pull_requests::merge,
        pull_requests::artifact,
        harnesses::list,
        harnesses::validate,
        accounts::list,
        accounts::get,
        accounts::create,
        accounts::update,
        accounts::delete,
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
        quark_systems::TaskEvent,
        quark_systems::DispatchRecord,
        quark_systems::DispatchTrigger,
        quark_systems::DispatchDecider,
        quark_systems::DispatchStatus,
        quark_systems::FileChangeStatus,
        quark_systems::DecisionState,
        quark_systems::TerminalRole,
        quark_systems::TerminalChunkKind,
        quark_systems::TerminalOutput,
        quark_systems::ProjectStatus,
        quark_systems::DeliveryPolicy,
        quark_systems::DispatchPreset,
        quark_systems::TranscriptEntry,
        quark_systems::TranscriptRole,
        quark_systems::ToolInfo,
        quark_systems::ToolKind,
        quark_systems::ToolDiffLine,
        quark_systems::ToolDiffLineKind,
        quark_systems::PullRequestState,
        quark_systems::Mergeability,
        quark_systems::ChecksState,
        quark_systems::ReviewDecision,
        quark_systems::CheckStatus,
        quark_systems::ReviewState,
        quark_systems::CheckUpdated,
        quark_systems::ReviewUpdated,
        quark_systems::DiffSide,
        quark_systems::MergeMethod,
        quark_systems::GateState,
        quark_systems::GateKind,
        quark_systems::ArtifactKind,
        quark_systems::MemorySource,
        quark_systems::MemoryProposalState,
        quark_systems::QuotaState,
        quark_systems::AccountQuotaChanged,
        quark_systems::AccountFailover,
        quark_systems::FailoverOutcome,
    )),
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
