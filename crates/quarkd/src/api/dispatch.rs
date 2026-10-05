use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use quark_systems::{DispatchRules, DispatchTest, ErrorBody, PutDispatchRules, TestDispatch};
use std::path::PathBuf;

use super::{db, ApiError, AppState};
use crate::classifier;
use crate::crew_dispatch::{self, CrewDispatchConfig};
use crate::dispatch::Resolved;
use crate::dispatch_test::{self, Host};
use crate::engine::WorkspaceRef;
use crate::project_repo;

/// Largest task description accepted, in bytes.
const MAX_DESCRIPTION_BYTES: usize = 256 * 1024;

fn dispatch_invalid(status: StatusCode, message: String) -> ApiError {
    ApiError::new(status, "dispatch_invalid", message)
}

fn no_project_repo() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "no_project_repo",
        "the Project has no Project repo to keep dispatch rules in",
    )
}

/// The rules `dispatch.yaml` on `main` of the bare repo declares.
fn read_rules(
    project_id: &str,
    bare: &std::path::Path,
    user_config: &std::path::Path,
) -> Result<DispatchRules, ApiError> {
    let failed =
        |e: String| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "project_repo_failed", e);
    let declared = crew_dispatch::read_declared(bare).map_err(failed)?;
    let written = match &declared {
        Some(d) => crew_dispatch::as_written(&d.dispatch_yaml)
            .map_err(|e| dispatch_invalid(StatusCode::CONFLICT, e))?,
        None => CrewDispatchConfig::default(),
    };
    let commit = crate::gates::git(
        bare,
        &[
            "log",
            "-1",
            "--format=%H",
            "main",
            "--",
            crew_dispatch::FILE,
        ],
    )
    .ok()
    .filter(|c| !c.is_empty());
    let draft = crew_dispatch::draft(&written);
    let effective = classifier::user_default_at(user_config)
        .and_then(|user| {
            let block = classifier::block(written.classifier.as_ref())?;
            classifier::effective(block.as_ref(), user.as_ref())
        })
        .and_then(|c| serde_json::to_value(c).map_err(|e| e.to_string()))
        .map_err(|e| dispatch_invalid(StatusCode::CONFLICT, e))?;
    Ok(DispatchRules {
        project_id: project_id.to_string(),
        revision: declared.map(|d| d.blob),
        commit,
        classifier: Some(effective),
        default_select: draft.default_select,
        rules: draft.rules,
        default: draft.default,
    })
}

/// The Project's dispatch rules.
///
/// What `dispatch.yaml` on the Project repo's `main` declares: the rules in
/// order, each with its name, condition and ordered candidates, the default
/// candidates and `default_select`. Harness ids are Quark's. A Project repo
/// without the file answers no rules and no `revision`. `classifier` is the
/// classifier in effect: the file's block over the user-level default in
/// `~/.quark/config.yaml`, `provider: none` when neither names one.
#[utoipa::path(
    get,
    path = "/v1/projects/{id}/dispatch",
    tag = "projects",
    params(("id" = String, Path, description = "Project id")),
    responses(
        (status = 200, body = DispatchRules),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`dispatch.yaml` does not compile (`dispatch_invalid`), or the Project has no Project repo (`no_project_repo`)", body = ErrorBody)
    )
)]
pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<DispatchRules>, ApiError> {
    let project = db(&state, move |s| s.get_project(&id)).await?;
    let bare = project.project_repo_path.ok_or_else(no_project_repo)?;
    let user_config = state.layout.user_config();
    tokio::task::spawn_blocking(move || read_rules(&project.id, &PathBuf::from(bare), &user_config))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .map(Json)
}

/// Save the Project's dispatch rules.
///
/// Writes the rules as `dispatch.yaml` and commits it to the Project repo's
/// `main` in a commit of its own, so the change can be reviewed. The file is
/// compiled first, as it is before it reaches the engine, and one that does
/// not compile is refused with the compile error. The file's `classifier`
/// block and the comment lines that open it are kept; other comments are
/// not. The engine takes the saved rules on the daemon's next refresh.
/// Rules that are already what `main` has make no commit.
#[utoipa::path(
    put,
    path = "/v1/projects/{id}/dispatch",
    tag = "projects",
    params(("id" = String, Path, description = "Project id")),
    request_body = PutDispatchRules,
    responses(
        (status = 200, description = "The rules as saved, with their new `revision` and `commit`", body = DispatchRules),
        (status = 400, description = "The rules do not compile (`dispatch_invalid`)", body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`main` has another `revision` (`dispatch_changed`), the file there does not compile (`dispatch_invalid`), the checkout has an uncommitted edit of it (`uncommitted_changes`), or the Project has no Project repo (`no_project_repo`)", body = ErrorBody)
    )
)]
pub async fn put(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<PutDispatchRules>,
) -> Result<Json<DispatchRules>, ApiError> {
    // Memory entries are committed through the same checkout.
    let _one = super::memory::COMMITTING.lock().await;
    let project = db(&state, move |s| s.get_project(&id)).await?;
    let (Some(bare), Some(root)) = (project.project_repo_path, project.workspace_path) else {
        return Err(no_project_repo());
    };
    let (bare, checkout) = (PathBuf::from(bare), PathBuf::from(root).join("project"));
    let user_config = state.layout.user_config();
    tokio::task::spawn_blocking(move || {
        project_repo::commit_file(
            &bare,
            &checkout,
            crew_dispatch::FILE,
            |current| {
                if input
                    .revision
                    .as_deref()
                    .is_some_and(|r| Some(r) != current.map(|(blob, _)| blob))
                {
                    return Err(ApiError::new(
                        StatusCode::CONFLICT,
                        "dispatch_changed",
                        "dispatch.yaml changed on main since these rules were loaded; \
                         load them again and repeat the edit",
                    ));
                }
                let body = crew_dispatch::render(&input.rules, current.map(|(_, text)| text))
                    .map_err(|e| dispatch_invalid(StatusCode::CONFLICT, e))?;
                let user = classifier::user_default_at(&user_config)
                    .map_err(|e| dispatch_invalid(StatusCode::CONFLICT, e))?;
                crew_dispatch::compile(&body, user.as_ref())
                    .map_err(|e| dispatch_invalid(StatusCode::BAD_REQUEST, e))?;
                Ok(body)
            },
            "Update dispatch rules",
        )?;
        read_rules(&project.id, &bare, &user_config)
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .map(Json)
}

/// Test a task description against the Project's dispatch rules.
///
/// Runs the engine's dispatch resolution on the description, as the
/// coordinator does on a task's brief, against the rules `dispatch.yaml` on
/// the Project repo's `main` declares. With a `draft`, the candidates are
/// those of the draft, which is compiled and never written; the resolution
/// reads the saved rules, so it is not run for a draft that differs from
/// them, and the outcome says so. Returns the rule the classifier
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
        (status = 400, description = "No description, or the `draft` does not compile (`dispatch_invalid`)", body = ErrorBody),
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

    let (repo, draft) = (project.project_repo_path.clone(), input.draft);
    let user_config = state.layout.user_config();
    let (config, unsaved) = tokio::task::spawn_blocking(move || {
        let user = classifier::user_default_at(&user_config)
            .map_err(|e| dispatch_invalid(StatusCode::CONFLICT, e))?;
        let declared = match repo {
            Some(repo) => crew_dispatch::read_declared(&PathBuf::from(repo))
                .map_err(|e| dispatch_invalid(StatusCode::CONFLICT, e))?,
            None => None,
        };
        let saved = || match &declared {
            Some(d) => crew_dispatch::compile(&d.dispatch_yaml, user.as_ref())
                .map_err(|e| dispatch_invalid(StatusCode::CONFLICT, e)),
            None => crew_dispatch::compile("", user.as_ref())
                .map_err(|e| dispatch_invalid(StatusCode::CONFLICT, e)),
        };
        let Some(draft) = draft else {
            return Ok((saved()?, false));
        };
        let body = crew_dispatch::render(&draft, declared.as_ref().map(|d| &*d.dispatch_yaml))
            .map_err(|e| dispatch_invalid(StatusCode::CONFLICT, e))?;
        let config = crew_dispatch::compile(&body, user.as_ref())
            .map_err(|e| dispatch_invalid(StatusCode::BAD_REQUEST, e))?;
        let unsaved = config != saved()?;
        Ok::<_, ApiError>((config, unsaved))
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))??;

    let resolved = match project.workspace_path {
        _ if unsaved => Resolved::NotRun(
            "These rules are not saved, and the dispatch resolution reads the saved ones, \
             so it was not run"
                .into(),
        ),
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
