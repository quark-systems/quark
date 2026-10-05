use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use quark_systems::{DispatchTest, ErrorBody, TestDispatch};
use std::path::PathBuf;

use super::{db, ApiError, AppState};
use crate::crew_dispatch::{self, CrewDispatchConfig};
use crate::dispatch::Resolved;
use crate::dispatch_test::{self, Host};
use crate::engine::WorkspaceRef;

/// Largest task description accepted, in bytes.
const MAX_DESCRIPTION_BYTES: usize = 256 * 1024;

/// Test a task description against the Project's dispatch rules.
///
/// Runs the engine's dispatch resolution on the description, as the
/// coordinator does on a task's brief, against the rules `dispatch.yaml` on
/// the Project repo's `main` declares. Returns the rule the classifier
/// matched and the profile it would select, or, with no classifier
/// (`classifier.provider` is `none`), that the coordinator would pick. Every
/// candidate says whether a worker could start with it now and why: harness
/// installed, model accepted, account health and quota headroom. Nothing is
/// dispatched or recorded.
#[utoipa::path(
    post,
    path = "/v1/projects/{id}/dispatch:test",
    tag = "projects",
    params(("id" = String, Path, description = "Project id")),
    request_body = TestDispatch,
    responses(
        (status = 200, body = DispatchTest),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`dispatch.yaml` does not compile", body = ErrorBody)
    )
)]
pub async fn test(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<TestDispatch>,
) -> Result<Json<DispatchTest>, ApiError> {
    let description = input.description.trim().to_string();
    if description.is_empty() {
        return Err(ApiError::invalid("description is empty"));
    }
    if description.len() > MAX_DESCRIPTION_BYTES {
        return Err(ApiError::invalid(format!(
            "description is longer than {MAX_DESCRIPTION_BYTES} bytes"
        )));
    }
    let project = db(&state, move |s| s.get_project(&id)).await?;

    let config = match project.project_repo_path.clone() {
        Some(repo) => tokio::task::spawn_blocking(move || {
            match crew_dispatch::read_declared(&PathBuf::from(repo))? {
                Some(declared) => crew_dispatch::compile(&declared.dispatch_yaml),
                None => Ok(CrewDispatchConfig::default()),
            }
        })
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .map_err(|e| ApiError::new(StatusCode::CONFLICT, "dispatch_invalid", e))?,
        None => CrewDispatchConfig::default(),
    };

    let resolved = match project.workspace_path {
        None => Resolved::NotRun(
            "The Project has no workspace yet, so the dispatch resolution was not run".into(),
        ),
        Some(root) => {
            let ws = WorkspaceRef {
                project_id: project.id.clone(),
                root: PathBuf::from(root),
            };
            match state.engine.resolve_description(&ws, &description).await {
                Ok(Some(r)) => Resolved::Ran(Box::new(r)),
                Ok(None) => Resolved::NotRun("The engine has no dispatch resolution".into()),
                Err(e) => Resolved::Failed(e.to_string()),
            }
        }
    };

    let infos = state.harnesses.list(false).await;
    let accounts = state.accounts.list().await?;
    let host = Host {
        harnesses: &state.harnesses,
        infos: &infos,
        accounts: &accounts,
    };
    Ok(Json(dispatch_test::build(
        &project.id,
        &config,
        resolved,
        &host,
    )))
}
