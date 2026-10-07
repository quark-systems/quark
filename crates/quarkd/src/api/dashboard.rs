//! The Project dashboard: its Settings tab reads every per-Project switch in
//! one call and changes the ones the daemon owns.

use std::path::PathBuf;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use quark_systems::{
    DeliveryPolicy, DispatchSummary, ErrorBody, MemoryProposalState, MemorySummary, Project,
    ProjectSettings, UpdateProject, UpdateProjectSettings, VerificationSettings,
};

use super::{db, ApiError, AppState};
use crate::{gates, memory, project_repo, settings};

/// Every per-Project switch.
///
/// Standing approval, delivery and the agent config come from the Project;
/// the verification gates, with each source's holdout tests, from
/// `project.yaml` on the Project repo's `main`; dispatch rules and memory are
/// summarized from theirs. A part that cannot be read says why in its
/// `error` rather than failing the call.
#[utoipa::path(
    get,
    path = "/v1/projects/{id}/settings",
    tag = "projects",
    params(("id" = String, Path, description = "Project id")),
    responses(
        (status = 200, body = ProjectSettings),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ProjectSettings>, ApiError> {
    let project = db(&state, move |s| s.get_project(&id)).await?;
    Ok(Json(read(&state, project).await?))
}

/// Change Project settings.
///
/// `holdout` turns a source's holdout tests on or off in `project.yaml`, in
/// one commit on the Project repo's `main` (comments in the file are not
/// kept); the engine takes the new gates on the daemon's next refresh.
/// `standing_approval` is applied to the engine as `PATCH /v1/projects/{id}`
/// applies it. Holdout changes are made first, so a refused one changes
/// nothing. Answers the settings as they are now.
#[utoipa::path(
    patch,
    path = "/v1/projects/{id}/settings",
    tag = "projects",
    params(("id" = String, Path, description = "Project id")),
    request_body = UpdateProjectSettings,
    responses(
        (status = 200, body = ProjectSettings),
        (status = 400, description = "A holdout change names no source of `project.yaml`, or leaves a file that does not compile (`settings_invalid`)", body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`main` has another `project.yaml` revision (`settings_changed`), the checkout has an uncommitted edit of it (`uncommitted_changes`), the Project has no Project repo or `project.yaml` (`no_project_repo`), or standing approval needs the workspace (`workspace_missing`)", body = ErrorBody),
        (status = 502, description = "The engine refused or failed the call", body = ErrorBody)
    )
)]
pub async fn patch(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<UpdateProjectSettings>,
) -> Result<Json<ProjectSettings>, ApiError> {
    let pid = id.clone();
    let project = db(&state, move |s| s.get_project(&pid)).await?;
    if !input.holdout.is_empty() {
        set_holdout(&project, &input).await?;
    }
    let project = match input.standing_approval {
        Some(on) => {
            super::routes::set_standing_approval(&state, &project, on).await?;
            let update = UpdateProject {
                standing_approval: Some(on),
                ..Default::default()
            };
            db(&state, move |s| s.update_project(&id, update)).await?
        }
        None => project,
    };
    Ok(Json(read(&state, project).await?))
}

fn no_project_repo() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "no_project_repo",
        "the Project has no Project repo with a project.yaml to keep its gates in",
    )
}

async fn set_holdout(project: &Project, input: &UpdateProjectSettings) -> Result<(), ApiError> {
    let (Some(bare), Some(root)) = (&project.project_repo_path, &project.workspace_path) else {
        return Err(no_project_repo());
    };
    // Memory entries and dispatch rules are committed through the same checkout.
    let _one = super::memory::COMMITTING.lock().await;
    let (bare, checkout) = (PathBuf::from(bare), PathBuf::from(root).join("project"));
    let (changes, revision) = (input.holdout.clone(), input.revision.clone());
    let invalid = |e: String| ApiError::new(StatusCode::BAD_REQUEST, "settings_invalid", e);
    tokio::task::spawn_blocking(move || {
        project_repo::commit_file(
            &bare,
            &checkout,
            settings::FILE,
            |current| {
                let Some((blob, text)) = current else {
                    return Err(no_project_repo());
                };
                if revision.as_deref().is_some_and(|r| r != blob) {
                    return Err(ApiError::new(
                        StatusCode::CONFLICT,
                        "settings_changed",
                        "project.yaml changed on main since these settings were loaded; \
                         load them again and repeat the change",
                    ));
                }
                let body = settings::set_holdout(text, &changes).map_err(invalid)?;
                gates::compile(&body, &bare, &[]).map_err(invalid)?;
                Ok(body)
            },
            "Change holdout tests",
        )
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))??;
    Ok(())
}

/// The settings of `project`, reading its Project repo off the async runtime.
async fn read(state: &AppState, project: Project) -> Result<ProjectSettings, ApiError> {
    let pid = project.id.clone();
    let to_review = db(state, move |s| {
        s.list_memory_proposals(&pid, Some(MemoryProposalState::Proposed))
    })
    .await?
    .len();
    let user_config = state.layout.user_config();
    let bare = project.project_repo_path.clone().map(PathBuf::from);
    let pid = project.id.clone();
    let (verification, dispatch, entries) = tokio::task::spawn_blocking(move || {
        let Some(bare) = bare else {
            return (
                VerificationSettings::default(),
                DispatchSummary::default(),
                0,
            );
        };
        let dispatch = match super::dispatch::read_rules(&pid, &bare, &user_config) {
            Ok(r) => DispatchSummary {
                rules: r.rules.len() as u32,
                default_candidates: r.default.len() as u32,
                classifier: r
                    .classifier
                    .as_ref()
                    .and_then(|c| c.get("provider"))
                    .and_then(|p| p.as_str())
                    .map(str::to_string),
                error: None,
            },
            Err(e) => DispatchSummary {
                error: Some(e.message().to_string()),
                ..Default::default()
            },
        };
        let entries = memory::list(&bare, &pid).map(|e| e.len()).unwrap_or(0);
        (settings::verification(&bare), dispatch, entries)
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(ProjectSettings {
        project_id: project.id,
        standing_approval: project.standing_approval,
        delivery: project.delivery.unwrap_or(DeliveryPolicy::Gated),
        agent_config: project.agent_config,
        verification,
        dispatch,
        memory: MemorySummary {
            entries: entries as u32,
            proposals_to_review: to_review as u32,
        },
    })
}

/// This module's endpoints, merged into the served document by
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(get, patch))]
pub(super) struct Api;

/// This module's routes, merged into the `/v1` router.
pub(super) fn router() -> axum::Router<AppState> {
    use axum::routing::get;
    axum::Router::new().route("/v1/projects/{id}/settings", get(self::get).patch(patch))
}
