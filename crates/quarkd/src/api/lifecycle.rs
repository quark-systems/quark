//! Archiving and deleting Projects.
//!
//! Archiving stops a Project's coordinator and workers and leaves it out of
//! `GET /v1/projects` and every background pass (coordinator recovery,
//! refreshes, triggers, Beads sync), keeping everything Quark has for it.
//! Unarchiving brings it back; coordinator recovery then starts its
//! coordinator again in its latest conversation. Stopped workers stay
//! stopped, with their worktrees, until someone relaunches them.
//!
//! Deleting archives the Project first, then gives every worker's worktree
//! back and removes what Quark keeps for it: its store rows, the workspace
//! and Project repo Quark made under its home, the coordinator's token and
//! record, and its Beads database. A worktree holding work that has not
//! landed stops the delete before anything is removed, and the Project stays
//! archived. A workspace Quark did not make (one attached at creation) and
//! the source repos are never touched.

use std::path::Path;

use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::Json;
use quark_systems::{DeleteProject, ErrorBody, Project, ProjectStatus, TaskState};

use super::{db, ApiError, AppState};
use crate::engine::native_project::WINDOW_PREFIX;
use crate::engine::{EngineError, EngineTask, TaskControl, WorkspaceRef, WorktreeRelease};

/// Archive a Project: stop its coordinator and workers and hide it.
///
/// Repeating it stops anything still running again.
#[utoipa::path(
    post,
    path = "/v1/projects/{id}:archive",
    tag = "projects",
    params(("id" = String, Path, description = "Project id")),
    responses(
        (status = 200, body = Project),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "The Project is still being provisioned"),
        (status = 502, body = ErrorBody, description = "The engine could not stop an agent; the Project is archived and archiving again retries")
    )
)]
pub async fn archive_project(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<(StatusCode, Json<Project>), ApiError> {
    let project = archive(&state, &id).await?;
    stop(&state, &project).await?;
    Ok((StatusCode::OK, Json(project)))
}

/// Bring an archived Project back. Its coordinator starts again in its
/// latest conversation; workers stopped by archiving stay stopped.
#[utoipa::path(
    post,
    path = "/v1/projects/{id}:unarchive",
    tag = "projects",
    params(("id" = String, Path, description = "Project id")),
    responses(
        (status = 200, body = Project),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn unarchive_project(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<(StatusCode, Json<Project>), ApiError> {
    let project = db(&state, move |s| s.set_project_archived(&id, false)).await?;
    state
        .beads
        .watch_project(state.store.clone(), state.layout.home.clone(), &project.id);
    Ok((StatusCode::OK, Json(project)))
}

/// Delete a Project and everything Quark keeps for it.
///
/// `confirm_name` must be the Project's name. The Project is archived and
/// its agents stopped first; then each worker's worktree is given back,
/// and a worktree with uncommitted changes or unpushed commits stops the
/// delete (409 `unlanded_work`) before anything is removed. Quark's own
/// workspace, Project repo and Beads database for the Project are removed;
/// a workspace that was attached rather than made by Quark is left as it
/// is. Emits `project.deleted`.
#[utoipa::path(
    delete,
    path = "/v1/projects/{id}",
    tag = "projects",
    params(("id" = String, Path, description = "Project id")),
    request_body = DeleteProject,
    responses(
        (status = 204, description = "The Project is deleted"),
        (status = 400, body = ErrorBody, description = "`confirm_name` is not the Project's name"),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "Still provisioning, or a worktree holds work that has not landed; the Project is archived"),
        (status = 502, body = ErrorBody, description = "The engine could not stop an agent or give a worktree back; the Project is archived")
    )
)]
pub async fn delete_project(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Json(input): Json<DeleteProject>,
) -> Result<StatusCode, ApiError> {
    let pid = id.clone();
    let project = db(&state, move |s| s.get_project(&pid)).await?;
    if input.confirm_name.trim() != project.name {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "confirm_mismatch",
            "type the Project's name exactly to delete it",
        ));
    }
    let project = archive(&state, &id).await?;
    let tasks = stop(&state, &project).await?;
    if let Some(ws) = workspace(&project) {
        release_worktrees(&state, &ws, &tasks).await?;
    }
    remove_files(&state, &project).await?;
    // Terminals are optional; without them there is nothing to forget.
    let _ = state.sessions.sync(&project.id, Vec::new()).await;
    let _ = state.sessions.set_coordinator(&project.id, None).await;
    db(&state, move |s| s.delete_project(&id)).await?;
    tracing::info!(project = %project.id, name = %project.name, "Project deleted");
    Ok(StatusCode::NO_CONTENT)
}

/// Marks the Project archived, refusing one still being provisioned (its
/// provisioning would start the coordinator again).
async fn archive(state: &AppState, id: &str) -> Result<Project, ApiError> {
    let pid = id.to_string();
    let project = db(state, move |s| s.get_project(&pid)).await?;
    if project.status == ProjectStatus::Provisioning {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "provisioning",
            "the Project is still being provisioned; try again once it is ready or failed",
        ));
    }
    let pid = id.to_string();
    let project = db(state, move |s| s.set_project_archived(&pid, true)).await?;
    state.beads.unwatch(&project.id);
    Ok(project)
}

fn workspace(project: &Project) -> Option<WorkspaceRef> {
    project.workspace_path.as_ref().map(|root| WorkspaceRef {
        project_id: project.id.clone(),
        root: root.into(),
    })
}

/// Stops every open task's worker and the coordinator, then records the
/// stopped board. Returns the engine's tasks as they were before stopping.
async fn stop(state: &AppState, project: &Project) -> Result<Vec<EngineTask>, ApiError> {
    let mut failures = Vec::new();
    let mut tasks = Vec::new();
    if let Some(ws) = workspace(project) {
        match state.engine.snapshot(&ws).await {
            Ok(snapshot) => tasks = snapshot.tasks,
            Err(EngineError::WorkspaceNotFound(_)) => {}
            Err(e) => failures.push(format!("reading its workers: {e}")),
        }
        for task in tasks.iter().filter(|t| open(t.state)) {
            match state
                .engine
                .control(&ws, &task.id, &TaskControl::Cancel)
                .await
            {
                Ok(()) | Err(EngineError::TaskNotFound(_)) => {}
                Err(e) => failures.push(format!("stopping {}: {e}", task.title)),
            }
        }
    }
    if let Err(e) = stop_coordinator(state, &project.id).await {
        failures.push(format!("stopping the coordinator: {e}"));
    }
    if let Some(ws) = workspace(project) {
        if let Ok(snapshot) = state.engine.snapshot(&ws).await {
            let pid = project.id.clone();
            if let Err(e) = db(state, move |s| s.apply_snapshot(&pid, &snapshot)).await {
                tracing::warn!(project = %project.id, error = %e.message(), "could not record the stopped board");
            }
        }
    }
    if failures.is_empty() {
        Ok(tasks)
    } else {
        Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "engine_failed",
            format!(
                "the Project is archived, but {}; try again",
                failures.join("; ")
            ),
        ))
    }
}

/// Whether the task may still have a worker to stop.
fn open(state: TaskState) -> bool {
    !matches!(state, TaskState::Done | TaskState::Failed)
}

/// Closes the coordinator's window: the one the engine recorded, and one
/// named for the Project the native engine would adopt. Its conversation is
/// kept, so it resumes when the Project is unarchived.
async fn stop_coordinator(state: &AppState, project_id: &str) -> Result<(), String> {
    let Ok(server) = state.sessions.server() else {
        return Ok(());
    };
    if !server.is_running().await {
        return Ok(());
    }
    let recorded = state
        .engine
        .coordinator_terminals(&state.layout.command_workspace())
        .await
        .map_err(|e| e.to_string())?
        .remove(project_id);
    let window = format!("{WINDOW_PREFIX}{project_id}");
    let panes = server.list_panes().await.map_err(|e| e.to_string())?;
    let mut targets: Vec<String> = panes
        .into_iter()
        .map(|p| p.target)
        .filter(|t| {
            Some(t) == recorded.as_ref() || t.split_once(':').is_some_and(|(_, w)| w == window)
        })
        .collect();
    targets.dedup();
    for target in targets {
        server
            .run(&["kill-window", "-t", &target])
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Gives back every worktree the Project's tasks hold. Any kept for work
/// that has not landed, or that the engine could not give back, stops the
/// delete.
async fn release_worktrees(
    state: &AppState,
    ws: &WorkspaceRef,
    tasks: &[EngineTask],
) -> Result<(), ApiError> {
    let mut kept = Vec::new();
    let mut failed = Vec::new();
    for task in tasks.iter().filter(|t| t.worktree.is_some()) {
        match state.engine.release_worktree(ws, &task.id).await {
            Ok(WorktreeRelease::Returned) | Err(EngineError::TaskNotFound(_)) => {}
            Ok(WorktreeRelease::Kept {
                uncommitted,
                unpushed_commits,
            }) => kept.push(format!(
                "{} ({})",
                task.title,
                crate::clear::unlanded(uncommitted, unpushed_commits)
            )),
            Err(e) => failed.push(format!("{}: {e}", task.title)),
        }
    }
    if !kept.is_empty() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "unlanded_work",
            format!(
                "Not deleted: work in these workers' worktrees has not landed: {}. \
                 The Project is archived; push or discard that work, then delete it again.",
                kept.join(", ")
            ),
        ));
    }
    if !failed.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "engine_failed",
            format!(
                "Not deleted: these workers' worktrees could not be given back: {}. \
                 The Project is archived.",
                failed.join("; ")
            ),
        ));
    }
    Ok(())
}

/// Removes what Quark made for the Project under its home. Never a path
/// outside it: an attached workspace stays.
async fn remove_files(state: &AppState, project: &Project) -> Result<(), ApiError> {
    let id = project.id.as_str();
    if !safe_id(id) {
        return Err(ApiError::internal(format!(
            "refusing to remove files for Project id {id:?}"
        )));
    }
    let layout = &state.layout;
    let command = layout.command_workspace();
    state.engine.forget_coordinator(&command, id).await?;
    state
        .beads
        .remove(&layout.home, id)
        .await
        .map_err(|e| ApiError::internal(format!("removing its Beads database: {e}")))?;
    let paths = [
        layout.workspace(id),
        layout.project_repo(id),
        layout.home.join("coordinators").join(format!("{id}.token")),
    ];
    for path in paths {
        if path == command {
            continue;
        }
        remove(&path)
            .await
            .map_err(|e| ApiError::internal(format!("removing {}: {e}", path.display())))?;
    }
    Ok(())
}

/// Project ids name directories under Quark's home; only ids that cannot
/// climb out of them are acted on.
fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id != "command"
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Removes a file or directory tree; a symlink is removed, not followed.
async fn remove(path: &Path) -> std::io::Result<()> {
    let meta = match tokio::fs::symlink_metadata(path).await {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if meta.is_dir() {
        tokio::fs::remove_dir_all(path).await
    } else {
        tokio::fs::remove_file(path).await
    }
}

/// This module's endpoints and the `project.deleted` payload.
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(archive_project, unarchive_project, delete_project),
    components(schemas(quark_systems::ProjectDeleted))
)]
pub(super) struct Api;

/// The routes live with the other Project routes in [`super::routes`],
/// which own `/v1/projects/{id}` and its custom methods.
pub(super) fn router() -> axum::Router<AppState> {
    axum::Router::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_path_safe_ids() {
        assert!(safe_id("prj_01a0fe6f61ca72a5803eeb6b29a2b60a"));
        for bad in ["", "command", "..", "a/b", "prj 1", "../x"] {
            assert!(!safe_id(bad), "{bad}");
        }
    }
}
