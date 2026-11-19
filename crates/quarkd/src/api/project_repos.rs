//! A Project's repos after creation: add a repo or local folder to it, or
//! take one out.
//!
//! Adding clones the repo into the command center and the Project workspace
//! (`<workspace>/repos/<name>`), as provisioning does, then records it in
//! `project.yaml` on the Project repo's `main` and on the Project. A Project
//! created without repos has no workspace yet: its first repo provisions it.
//!
//! Removing refuses while the workspace clone, or a worktree of it, holds
//! work that is on no remote. Otherwise the clone moves out of the
//! workspace, to `<workspace>/.quark/removed-repos/`, so nothing is
//! deleted, and the repo leaves `project.yaml` and the Project.
//!
//! The running coordinator is told about either change in its inbox; its
//! prompt lists the new set of repos from its next start.

use std::path::{Path as FsPath, PathBuf};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use quark_systems::{DeliveryPolicy, ErrorBody, Project, ProjectStatus, RepoSource};

use super::{db, ApiError, AppState};
use crate::engine::native_project::{REPOS_DIR, STATE_DIR};
use crate::engine::{SourceRepo, WorkspacePlan, WorkspaceRef};
use crate::{project_repo, project_sources, provision, settings};

/// Repo changes take turns, so two never race on one Project's list.
static CHANGING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Where removed clones are kept, inside the workspace's [`STATE_DIR`].
const REMOVED_DIR: &str = "removed-repos";

/// Add a repo or local folder to a Project.
///
/// `url` is a clone URL or an absolute path to a local repo; `name`, unique
/// in the Project, is derived from it when absent. The repo is cloned into
/// the workspace before the call answers. A Project that has no workspace
/// yet (created without repos) is provisioned instead, as `POST
/// /v1/projects` does, and is answered `provisioning`.
#[utoipa::path(
    post,
    path = "/v1/projects/{id}/repos",
    tag = "projects",
    params(("id" = String, Path, description = "Project id")),
    request_body = RepoSource,
    responses(
        (status = 200, description = "Added; the Project as it is now", body = Project),
        (status = 400, description = "Not a clone URL or path, or a bad name", body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "The name is taken (`repo_exists`), the Project is archived or provisioning (`project_busy`), uses a workspace Quark did not create (`not_provisioned`), has no agent to provision with (`no_agent`), or `project.yaml` has an uncommitted edit (`uncommitted_changes`)", body = ErrorBody),
        (status = 502, description = "Cloning failed", body = ErrorBody)
    )
)]
pub async fn add(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<RepoSource>,
) -> Result<Json<Project>, ApiError> {
    let _turn = CHANGING.lock().await;
    let project = db(&state, move |s| s.get_project(&id)).await?;
    check_settled(&project)?;
    let taken: Vec<String> = project
        .repos
        .iter()
        .filter_map(|r| r.name.clone())
        .collect();
    let repo = provision::normalize_repo(input, &[]).map_err(ApiError::invalid)?;
    let name = repo.name.clone().unwrap_or_default();
    if taken.contains(&name) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "repo_exists",
            format!("the Project already has a repo named {name:?}; give this one another name"),
        ));
    }
    let mut repos = project.repos.clone();
    repos.push(repo.clone());

    // A Project created without repos was only recorded: provision it now.
    if project.workspace_path.is_none() && project.project_repo_path.is_none() {
        let Some(agent) = &project.agent_config else {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "no_agent",
                "the Project has no agent to run its coordinator, so its workspace cannot be set up",
            ));
        };
        super::routes::check_coordinator_agent(&state, agent).await?;
        let pid = project.id.clone();
        let project = db(&state, move |s| s.set_project_repos(&pid, repos, true)).await?;
        super::routes::start_provisioning(&state, &project.id);
        return Ok(Json(project));
    }
    if project.status == ProjectStatus::Failed {
        // Provisioning stopped: record the repo; its retry clones it.
        let pid = project.id.clone();
        return Ok(Json(
            db(&state, move |s| s.set_project_repos(&pid, repos, false)).await?,
        ));
    }
    let (Some(root), Some(_)) = (&project.workspace_path, &project.project_repo_path) else {
        return Err(not_provisioned());
    };
    let root = PathBuf::from(root);
    let source = SourceRepo {
        name: name.clone(),
        url: repo.url.clone(),
    };
    let command = state.layout.command_workspace();
    std::fs::create_dir_all(&command).map_err(|e| ApiError::internal(e.to_string()))?;
    let delivery = project.delivery.unwrap_or(DeliveryPolicy::Gated);
    state.engine.add_source(&command, &source, delivery).await?;
    let plan = WorkspacePlan {
        project_id: project.id.clone(),
        name: project.name.clone(),
        goal: project.goal.clone(),
        sources: vec![source],
        root: root.clone(),
        user_memory: Some(state.layout.user_memory()),
    };
    state.engine.seed_workspace(&command, &plan).await?;

    let (n, url) = (name.clone(), repo.url.clone());
    commit_yaml(&project, format!("Add repo {name}"), move |text| {
        project_sources::add(text, &n, &url)
    })
    .await?;
    if project.standing_approval {
        let ws = WorkspaceRef {
            project_id: project.id.clone(),
            root: root.clone(),
        };
        if let Err(e) = state
            .engine
            .set_standing_approval(&ws, std::slice::from_ref(&name), true)
            .await
        {
            tracing::warn!(project = %project.id, repo = %name, error = %e, "could not apply standing approval to the added repo");
        }
    }
    let pid = project.id.clone();
    let project = db(&state, move |s| s.set_project_repos(&pid, repos, false)).await?;
    tell_coordinator(
        &state,
        &project.id,
        format!(
            "The person added the repo `{name}` ({}) to this Project. It is cloned at `{REPOS_DIR}/{name}` in the workspace.",
            repo.url
        ),
    )
    .await;
    Ok(Json(project))
}

/// Take a repo or local folder out of a Project.
///
/// Refused while the workspace clone or one of its worktrees has
/// uncommitted changes or commits on no remote (`unlanded_work`). Otherwise
/// the clone is moved to `<workspace>/.quark/removed-repos/`, not deleted,
/// and the repo leaves `project.yaml` and the Project. The source it came
/// from is never touched.
#[utoipa::path(
    delete,
    path = "/v1/projects/{id}/repos/{name}",
    tag = "projects",
    params(
        ("id" = String, Path, description = "Project id"),
        ("name" = String, Path, description = "The repo's name in the Project")
    ),
    responses(
        (status = 200, description = "Removed; the Project as it is now", body = Project),
        (status = 404, description = "No such Project or repo", body = ErrorBody),
        (status = 409, description = "Work in the clone is on no remote (`unlanded_work`), the Project is archived or provisioning (`project_busy`), or `project.yaml` has an uncommitted edit (`uncommitted_changes`)", body = ErrorBody)
    )
)]
pub async fn remove(
    State(state): State<AppState>,
    Path((id, name)): Path<(String, String)>,
) -> Result<Json<Project>, ApiError> {
    let _turn = CHANGING.lock().await;
    let project = db(&state, move |s| s.get_project(&id)).await?;
    check_settled(&project)?;
    let Some(at) = project
        .repos
        .iter()
        .position(|r| r.name.as_deref() == Some(name.as_str()))
    else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("the Project has no repo named {name:?}"),
        ));
    };
    let mut repos = project.repos.clone();
    repos.remove(at);

    let clone = project
        .workspace_path
        .as_ref()
        .map(|root| PathBuf::from(root).join(REPOS_DIR).join(&name))
        .filter(|p| p.exists());
    if let Some(clone) = &clone {
        let unlanded = unlanded(clone).await?;
        if !unlanded.is_empty() {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "unlanded_work",
                format!(
                    "{name} has work that is on no remote, so it was kept: {}. Push or discard it, then remove the repo again.",
                    unlanded.join("; ")
                ),
            ));
        }
    }
    if project.project_repo_path.is_some() && project.workspace_path.is_some() {
        let n = name.clone();
        commit_yaml(&project, format!("Remove repo {name}"), move |text| {
            project_sources::remove(text, &n)
        })
        .await?;
    }
    if let (Some(clone), Some(root)) = (&clone, &project.workspace_path) {
        if project.standing_approval {
            let ws = WorkspaceRef {
                project_id: project.id.clone(),
                root: root.into(),
            };
            if let Err(e) = state
                .engine
                .set_standing_approval(&ws, std::slice::from_ref(&name), false)
                .await
            {
                tracing::warn!(project = %project.id, repo = %name, error = %e, "could not turn standing approval off for the removed repo");
            }
        }
        set_aside(clone, &PathBuf::from(root), &name)?;
    }
    let pid = project.id.clone();
    let project = db(&state, move |s| s.set_project_repos(&pid, repos, false)).await?;
    tell_coordinator(
        &state,
        &project.id,
        format!(
            "The person removed the repo `{name}` from this Project. Its clone is no longer in `{REPOS_DIR}/`; do not start work in it."
        ),
    )
    .await;
    Ok(Json(project))
}

fn check_settled(project: &Project) -> Result<(), ApiError> {
    let busy = |why: &str| ApiError::new(StatusCode::CONFLICT, "project_busy", why.to_string());
    if project.archived_at.is_some() {
        return Err(busy(
            "the Project is archived; unarchive it to change its repos",
        ));
    }
    if project.status == ProjectStatus::Provisioning {
        return Err(busy(
            "the Project is being set up; change its repos once it is ready",
        ));
    }
    Ok(())
}

fn not_provisioned() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "not_provisioned",
        "the Project uses a workspace Quark did not create, so Quark does not manage its repos",
    )
}

/// Rewrite `project.yaml` with `edit` in one commit on the Project repo's
/// `main`. A Project repo without the file is left alone.
async fn commit_yaml(
    project: &Project,
    message: String,
    edit: impl FnOnce(&str) -> Result<String, String> + Send + 'static,
) -> Result<(), ApiError> {
    let (Some(bare), Some(root)) = (&project.project_repo_path, &project.workspace_path) else {
        return Ok(());
    };
    // Memory entries, dispatch rules and settings commit through the same checkout.
    let _one = super::memory::COMMITTING.lock().await;
    let (bare, checkout) = (PathBuf::from(bare), PathBuf::from(root).join("project"));
    tokio::task::spawn_blocking(move || {
        project_repo::commit_file(
            &bare,
            &checkout,
            settings::FILE,
            |current| match current {
                Some((_, text)) => edit(text)
                    .map_err(|e| ApiError::new(StatusCode::CONFLICT, "settings_invalid", e)),
                None => Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "no_project_repo",
                    "the Project repo has no project.yaml",
                )),
            },
            &message,
        )
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .map(|_| ())
    .or_else(|e| match e.code() {
        // An older Project repo without project.yaml has nothing to keep in step.
        "no_project_repo" => Ok(()),
        _ => Err(e),
    })
}

/// Each checkout of `clone` (itself and its linked worktrees) with work on
/// no remote, described for the person.
async fn unlanded(clone: &FsPath) -> Result<Vec<String>, ApiError> {
    let mut paths = vec![clone.to_path_buf()];
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(clone)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .await
        .map_err(|e| ApiError::internal(format!("git worktree list: {e}")))?;
    if out.status.success() {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                let p = PathBuf::from(p);
                if p.is_dir() && !paths.iter().any(|q| same(q, &p)) {
                    paths.push(p);
                }
            }
        }
    }
    let mut found = Vec::new();
    for p in paths {
        let mut work = quark_worktree::checks::landed_work(&p)
            .await
            .map_err(|e| ApiError::internal(format!("checking {}: {e}", p.display())))?;
        if p == clone {
            // A local `main` ahead of `origin` is work the default-branch
            // rule would call landed.
            work.unpushed_commits = work
                .unpushed_commits
                .max(local_branch_commits(clone).await?);
        }
        if work.is_landed() {
            continue;
        }
        let mut what = Vec::new();
        if work.uncommitted {
            what.push("uncommitted changes".to_string());
        }
        if work.unpushed_commits > 0 {
            what.push(format!(
                "{} unpushed commit{}",
                work.unpushed_commits,
                if work.unpushed_commits == 1 { "" } else { "s" }
            ));
        }
        found.push(format!("{} in {}", what.join(" and "), p.display()));
    }
    Ok(found)
}

/// Commits on the clone's checked-out branch that no remote-tracking branch
/// has. Branches of worker worktrees are judged in their worktrees, where a
/// squash-merged branch counts as landed.
async fn local_branch_commits(clone: &FsPath) -> Result<u32, ApiError> {
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(clone)
        .args(["rev-list", "--count", "HEAD", "--not", "--remotes"])
        .output()
        .await
        .map_err(|e| ApiError::internal(format!("git rev-list: {e}")))?;
    if !out.status.success() {
        // No commit yet: nothing to lose.
        return Ok(0);
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or_default())
}

fn same(a: &FsPath, b: &FsPath) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Move the clone out of the workspace's repos, keeping it.
fn set_aside(clone: &FsPath, root: &FsPath, name: &str) -> Result<PathBuf, ApiError> {
    let dir = root.join(STATE_DIR).join(REMOVED_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| ApiError::internal(e.to_string()))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let dest = dir.join(format!("{name}-{stamp}"));
    std::fs::rename(clone, &dest).map_err(|e| {
        ApiError::internal(format!(
            "moving {} to {}: {e}",
            clone.display(),
            dest.display()
        ))
    })?;
    Ok(dest)
}

/// Leave the running coordinator a note in its inbox; best effort.
async fn tell_coordinator(state: &AppState, project_id: &str, body: String) {
    let Some(engine) = &state.triggers else {
        return;
    };
    let note = quark_triggers::Inbound::note("quark", body);
    if let Err(e) = engine
        .receive(&quark_core::ProjectId::new(project_id.to_string()), note)
        .await
    {
        tracing::warn!(project = %project_id, error = %e, "could not tell the coordinator about a repo change");
    }
}

#[derive(utoipa::OpenApi)]
#[openapi(paths(add, remove))]
pub(super) struct Api;

/// This module's routes, merged into the `/v1` router.
pub(super) fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/v1/projects/{id}/repos", axum::routing::post(add))
        .route(
            "/v1/projects/{id}/repos/{name}",
            axum::routing::delete(remove),
        )
}
