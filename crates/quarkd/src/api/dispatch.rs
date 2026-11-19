use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use quark_systems::{
    DispatchDefaults, DispatchRules, DispatchRulesSource, DispatchTest, ErrorBody,
    PutDispatchDefaults, PutDispatchRules, TestDispatch,
};
use std::path::PathBuf;

use super::{db, ApiError, AppState};
use crate::classifier;
use crate::crew_dispatch::{self, CrewDispatchConfig};
use crate::dispatch::Resolved;
use crate::dispatch_defaults::{self, Defaults};
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

/// Where the daemon reads the default rules from.
pub(super) fn defaults_source(state: &AppState) -> dispatch_defaults::Source {
    dispatch_defaults::Source {
        home: state.layout.home.clone(),
        harnesses: state.harnesses.clone(),
        accounts: state.accounts.clone(),
    }
}

/// The default rules in effect.
pub(super) async fn load_defaults(state: &AppState) -> Result<Defaults, ApiError> {
    defaults_source(state)
        .load()
        .await
        .map_err(|e| dispatch_invalid(StatusCode::CONFLICT, e))
}

/// The rules in effect for the Project whose bare repo is `bare`: what
/// `dispatch.yaml` on `main` declares, or `defaults` when it lists no rules
/// of its own.
pub(super) fn read_rules(
    project_id: &str,
    bare: &std::path::Path,
    user_config: &std::path::Path,
    defaults: &Defaults,
) -> Result<DispatchRules, ApiError> {
    let failed =
        |e: String| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "project_repo_failed", e);
    let declared = crew_dispatch::read_declared(bare).map_err(failed)?;
    let (text, owned) = crew_dispatch::effective(
        declared.as_ref().map(|d| d.dispatch_yaml.as_str()),
        &defaults.yaml,
    )
    .map_err(|e| dispatch_invalid(StatusCode::CONFLICT, e))?;
    let written: CrewDispatchConfig =
        crew_dispatch::as_written(&text).map_err(|e| dispatch_invalid(StatusCode::CONFLICT, e))?;
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
        source: if owned {
            DispatchRulesSource::Project
        } else {
            DispatchRulesSource::Defaults
        },
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
    let defaults = load_defaults(&state).await?;
    tokio::task::spawn_blocking(move || {
        read_rules(&project.id, &PathBuf::from(bare), &user_config, &defaults)
    })
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
    let defaults = load_defaults(&state).await?;
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
        read_rules(&project.id, &bare, &user_config, &defaults)
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
    let defaults = load_defaults(&state).await?;
    let (config, unsaved) = tokio::task::spawn_blocking(move || {
        let user = classifier::user_default_at(&user_config)
            .map_err(|e| dispatch_invalid(StatusCode::CONFLICT, e))?;
        let declared = match repo {
            Some(repo) => crew_dispatch::read_declared(&PathBuf::from(repo))
                .map_err(|e| dispatch_invalid(StatusCode::CONFLICT, e))?,
            None => None,
        };
        let saved = || {
            let (text, _) = crew_dispatch::effective(
                declared.as_ref().map(|d| d.dispatch_yaml.as_str()),
                &defaults.yaml,
            )
            .map_err(|e| dispatch_invalid(StatusCode::CONFLICT, e))?;
            crew_dispatch::compile(&text, user.as_ref())
                .map_err(|e| dispatch_invalid(StatusCode::CONFLICT, e))
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

/// Reset the Project's dispatch rules to the defaults.
///
/// Rewrites `dispatch.yaml` on the Project repo's `main` without rules of its
/// own, keeping its `classifier` block, and commits it, so the Project
/// follows the default rules (`GET /v1/dispatch/defaults`) again. The rules
/// it had stay in the repo's history. A Project already following the
/// defaults makes no commit.
#[utoipa::path(
    post,
    path = "/v1/projects/{id}/dispatch:reset",
    tag = "projects",
    params(("id" = String, Path, description = "Project id")),
    responses(
        (status = 200, description = "The rules now in effect: the defaults", body = DispatchRules),
        (status = 404, body = ErrorBody),
        (status = 409, description = "The file on `main` does not compile (`dispatch_invalid`), the checkout has an uncommitted edit of it (`uncommitted_changes`), or the Project has no Project repo (`no_project_repo`)", body = ErrorBody)
    )
)]
pub async fn reset(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<DispatchRules>, ApiError> {
    let _one = super::memory::COMMITTING.lock().await;
    let project = db(&state, move |s| s.get_project(&id)).await?;
    let (Some(bare), Some(root)) = (project.project_repo_path, project.workspace_path) else {
        return Err(no_project_repo());
    };
    let (bare, checkout) = (PathBuf::from(bare), PathBuf::from(root).join("project"));
    let user_config = state.layout.user_config();
    let defaults = load_defaults(&state).await?;
    tokio::task::spawn_blocking(move || {
        project_repo::commit_file(
            &bare,
            &checkout,
            crew_dispatch::FILE,
            |current| {
                let text = current.map(|(_, text)| text);
                if text.is_some_and(|t| crew_dispatch::owns_rules(t).ok() == Some(false)) {
                    return Ok(text.unwrap_or_default().to_string());
                }
                crew_dispatch::reset(text).map_err(|e| dispatch_invalid(StatusCode::CONFLICT, e))
            },
            "Reset dispatch rules to the defaults",
        )?;
        read_rules(&project.id, &bare, &user_config, &defaults)
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .map(Json)
}

/// The Projects that follow the default rules, by id.
async fn following(state: &AppState) -> Result<Vec<String>, ApiError> {
    let projects = db(state, |s| s.list_projects()).await?;
    tokio::task::spawn_blocking(move || {
        projects
            .into_iter()
            .filter(|p| {
                p.project_repo_path
                    .as_ref()
                    .is_some_and(
                        |repo| match crew_dispatch::read_declared(&PathBuf::from(repo)) {
                            Ok(Some(d)) => {
                                crew_dispatch::owns_rules(&d.dispatch_yaml).ok() == Some(false)
                            }
                            Ok(None) => true,
                            Err(_) => false,
                        },
                    )
            })
            .map(|p| p.id)
            .collect()
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))
}

async fn defaults_body(state: &AppState, d: Defaults) -> Result<DispatchDefaults, ApiError> {
    let draft = d
        .draft()
        .map_err(|e| dispatch_invalid(StatusCode::CONFLICT, e))?;
    Ok(DispatchDefaults {
        revision: d.revision(),
        shipped: d.shipped,
        default_select: draft.default_select,
        rules: draft.rules,
        default: draft.default,
        projects: following(state).await?,
    })
}

/// The default dispatch rules.
///
/// Every Project whose `dispatch.yaml` lists no rules of its own follows
/// these; `projects` names them. They are kept in `~/.quark/dispatch.yaml`.
/// Until that is saved (`shipped` is true), they are Quark's shipped tiers:
/// Trivial, Standard, Complex and Scout, and a default for a task none
/// matches, with candidates from the harnesses installed here that have an
/// enabled, signed-in account.
#[utoipa::path(
    get,
    path = "/v1/dispatch/defaults",
    tag = "dispatch",
    responses(
        (status = 200, body = DispatchDefaults),
        (status = 409, description = "`~/.quark/dispatch.yaml` does not compile (`dispatch_invalid`)", body = ErrorBody)
    )
)]
pub async fn get_defaults(
    State(state): State<AppState>,
) -> Result<Json<DispatchDefaults>, ApiError> {
    let d = load_defaults(&state).await?;
    defaults_body(&state, d).await.map(Json)
}

/// Save the default dispatch rules.
///
/// Writes `~/.quark/dispatch.yaml`. The rules are compiled first, as a
/// Project's are, and refused with the compile error when they do not
/// compile. Every Project that follows the defaults takes them on the
/// daemon's next refresh.
#[utoipa::path(
    put,
    path = "/v1/dispatch/defaults",
    tag = "dispatch",
    request_body = PutDispatchDefaults,
    responses(
        (status = 200, body = DispatchDefaults),
        (status = 400, description = "The rules do not compile (`dispatch_invalid`)", body = ErrorBody),
        (status = 409, description = "The defaults have another `revision` by now (`dispatch_changed`)", body = ErrorBody)
    )
)]
pub async fn put_defaults(
    State(state): State<AppState>,
    Json(input): Json<PutDispatchDefaults>,
) -> Result<Json<DispatchDefaults>, ApiError> {
    let current = load_defaults(&state).await?;
    if input
        .revision
        .as_deref()
        .is_some_and(|r| r != current.revision())
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "dispatch_changed",
            "the default dispatch rules changed since they were loaded; \
             load them again and repeat the edit",
        ));
    }
    let home = state.layout.home.clone();
    let saved = tokio::task::spawn_blocking(move || dispatch_defaults::save(&home, &input.rules))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .map_err(|e| dispatch_invalid(StatusCode::BAD_REQUEST, e))?;
    defaults_body(&state, saved).await.map(Json)
}

/// Go back to the shipped default dispatch rules.
///
/// Removes `~/.quark/dispatch.yaml`, so the shipped tiers apply again to
/// every Project that follows the defaults.
#[utoipa::path(
    delete,
    path = "/v1/dispatch/defaults",
    tag = "dispatch",
    responses((status = 200, description = "The shipped defaults", body = DispatchDefaults))
)]
pub async fn delete_defaults(
    State(state): State<AppState>,
) -> Result<Json<DispatchDefaults>, ApiError> {
    let home = state.layout.home.clone();
    tokio::task::spawn_blocking(move || dispatch_defaults::forget(&home))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .map_err(ApiError::internal)?;
    let d = load_defaults(&state).await?;
    defaults_body(&state, d).await.map(Json)
}

/// This module's endpoints, plus schemas the generator does not reach from
/// them (event payloads), merged into the served document by
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(get, put, test, reset, get_defaults, put_defaults, delete_defaults),
    components(schemas(
        quark_systems::DispatchRecord,
        quark_systems::DispatchTrigger,
        quark_systems::DispatchDecider,
        quark_systems::DispatchStatus,
    ))
)]
pub(super) struct Api;

/// This module's routes, merged into the `/v1` router.
pub(super) fn router() -> axum::Router<AppState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/v1/projects/{id}/dispatch", get(self::get).put(put))
        .route("/v1/projects/{id}/dispatch:test", post(test))
        .route("/v1/projects/{id}/dispatch:reset", post(reset))
        .route(
            "/v1/dispatch/defaults",
            get(get_defaults).put(put_defaults).delete(delete_defaults),
        )
}
