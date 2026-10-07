//! Project memory (journey J8): review proposals from finished tasks, read
//! the entries accepted into the Project repo's `memory/`, and promote one to
//! the user-level memory every Project's coordinator reads.

use std::path::PathBuf;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use quark_systems::{
    AcceptMemoryProposal, ErrorBody, MemoryCommit, MemoryEntry, MemoryProposal,
    MemoryProposalState, PromoteMemoryEntry, RejectMemoryProposal, UserMemoryEntry,
};
use serde::Deserialize;
use utoipa::IntoParams;

use super::routes::{daemon_user, MAX_ANSWERED_BY_BYTES};
use super::{db, ApiError, AppState};
use crate::engine::WorkspaceRef;
use crate::memory;
use crate::project_repo;

/// One commit to a Project repo at a time, so two entries never race for a
/// file name or for the Project repo's `main`.
pub(super) static COMMITTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// One promotion at a time, so an entry is copied to user-level memory once.
static PROMOTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ProposalQuery {
    /// Only proposals in this state.
    pub state: Option<MemoryProposalState>,
}

/// A Project's memory proposals, oldest first.
///
/// A learning a finished task reported becomes a proposal and also arrives
/// as a `memory.proposed` event.
#[utoipa::path(
    get,
    path = "/v1/projects/{id}/memory/proposals",
    tag = "memory",
    params(("id" = String, Path, description = "Project id"), ProposalQuery),
    responses(
        (status = 200, body = Vec<MemoryProposal>),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn list_proposals(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<ProposalQuery>,
) -> Result<Json<Vec<MemoryProposal>>, ApiError> {
    Ok(Json(
        db(&state, move |s| s.list_memory_proposals(&id, q.state)).await?,
    ))
}

/// Dispatches `POST /v1/projects/{id}/memory/proposals/{proposal_id}:<action>`.
pub async fn proposal_action(
    state: State<AppState>,
    Path((project_id, target)): Path<(String, String)>,
    body: Bytes,
) -> Result<Json<MemoryProposal>, ApiError> {
    let Some((id, action)) = target.rsplit_once(':') else {
        return Err(ApiError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "use POST .../memory/proposals/{proposal_id}:accept or :reject",
        ));
    };
    let path = Path((project_id, id.to_string()));
    match action {
        "accept" => accept(state, path, Json(parse_body(&body)?)).await,
        "reject" => reject(state, path, Json(parse_body(&body)?)).await,
        _ => Err(ApiError::not_found()),
    }
}

/// An optional JSON body: empty means every field is absent.
fn parse_body<T: serde::de::DeserializeOwned + Default>(body: &[u8]) -> Result<T, ApiError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(body).map_err(|e| ApiError::invalid(format!("invalid body: {e}")))
}

/// Who decides: the given name, or the daemon's user.
fn decided_by(given: Option<&str>) -> Result<String, ApiError> {
    let who = match given.map(str::trim) {
        Some(user) if !user.is_empty() => user.to_string(),
        _ => daemon_user(),
    };
    if who.is_empty() || who.len() > MAX_ANSWERED_BY_BYTES || who.chars().any(char::is_control) {
        return Err(ApiError::invalid(
            "the name must be one line of at most 128 bytes",
        ));
    }
    Ok(who)
}

fn already_decided(message: String) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, "already_decided", message)
}

/// Accept a memory proposal, optionally edited.
///
/// The entry is written as one new file under the Project repo's `memory/`,
/// in its own commit on `main`, and the Project's coordinator is told about
/// it so it reads it on its next turn. The accepted proposal, with its
/// `entry`, is returned and also arrives as a `memory.accepted` event.
#[utoipa::path(
    post,
    path = "/v1/projects/{id}/memory/proposals/{proposal_id}:accept",
    tag = "memory",
    params(
        ("id" = String, Path, description = "Project id"),
        ("proposal_id" = String, Path, description = "Memory proposal id")
    ),
    request_body = AcceptMemoryProposal,
    responses(
        (status = 200, body = MemoryProposal, description = "The entry is committed"),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`already_decided`, or `no_project_repo`"),
        (status = 500, body = ErrorBody, description = "`project_repo_failed`: the commit failed")
    )
)]
pub async fn accept(
    State(state): State<AppState>,
    Path((project_id, id)): Path<(String, String)>,
    Json(input): Json<AcceptMemoryProposal>,
) -> Result<Json<MemoryProposal>, ApiError> {
    let who = decided_by(input.decided_by.as_deref())?;
    let _one = COMMITTING.lock().await;
    let proposal = {
        let (p, i) = (project_id.clone(), id.clone());
        db(&state, move |s| s.get_memory_proposal(&p, &i)).await?
    };
    if proposal.state != MemoryProposalState::Proposed {
        return Err(already_decided("the proposal is already decided".into()));
    }
    let text = match input.text.as_deref().map(str::trim) {
        Some(t) if !t.is_empty() => t.to_string(),
        Some(_) => return Err(ApiError::invalid("text must not be empty")),
        None => proposal.text.clone(),
    };
    memory::validate_text(&text).map_err(ApiError::invalid)?;
    let project = {
        let p = project_id.clone();
        db(&state, move |s| s.get_project(&p)).await?
    };
    let (Some(bare), Some(root)) = (project.project_repo_path, project.workspace_path) else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "no_project_repo",
            "the Project has no Project repo to keep memory in",
        ));
    };

    let mut entry = MemoryEntry {
        id: String::new(),
        project_id: project_id.clone(),
        path: String::new(),
        text,
        evidence: proposal.evidence.clone(),
        source: Some(proposal.source),
        date: Some(proposal.proposed_at.clone()),
        accepted_at: Some(crate::now_rfc3339()),
        accepted_by: Some(who.clone()),
        proposal_id: Some(proposal.id.clone()),
        commit: None,
    };
    let body = memory::render(&entry);
    let message = format!("Remember: {}", headline(&entry.text));
    let (date, text) = (proposal.proposed_at.clone(), entry.text.clone());
    let checkout = PathBuf::from(&root).join("project");
    let committed = tokio::task::spawn_blocking(move || {
        project_repo::commit_new_file(
            std::path::Path::new(&bare),
            &checkout,
            |taken| {
                let name = memory::file_name(&date, &text, |n| {
                    taken(&format!("{}/{n}", memory::MEMORY_DIR))
                });
                format!("{}/{name}", memory::MEMORY_DIR)
            },
            &body,
            &message,
        )
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?;
    let (path, commit) = committed.map_err(|e| {
        tracing::warn!(project = %project_id, error = %e, "committing a memory entry failed");
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "project_repo_failed",
            e.to_string(),
        )
    })?;
    entry.id = std::path::Path::new(&path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    entry.path = path;
    entry.commit = Some(commit);

    let accepted = {
        let (p, i, e) = (project_id.clone(), id, entry.clone());
        db(&state, move |s| s.accept_memory_proposal(&p, &i, &who, e))
            .await
            .map_err(|e| match e.code() {
                "conflict" => already_decided("the proposal is already decided".into()),
                _ => e,
            })?
    };
    tell_coordinator(&state, project_id, PathBuf::from(root), &entry);
    Ok(Json(accepted))
}

/// The first line of `text`, cut to fit a commit subject.
fn headline(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() <= 60 {
        return line.to_string();
    }
    let cut: String = line.chars().take(59).collect();
    format!("{}…", cut.trim_end())
}

/// Tells the coordinator about a new entry, so it reads it on its next turn
/// even though its session started before the entry existed. In the
/// background: the commit is what counts, and a coordinator without a live
/// session reads `memory/` when it next starts.
fn tell_coordinator(state: &AppState, project_id: String, root: PathBuf, entry: &MemoryEntry) {
    let text = format!(
        "Project memory: a new entry was accepted and committed to the Project repo at project/{}. \
         Read it and apply it from your next plan on:\n\n{}",
        entry.path, entry.text
    );
    let chat = state.chat.clone();
    tokio::spawn(async move {
        let ws = WorkspaceRef { project_id, root };
        if let Err(e) = chat.send(&ws, &text).await {
            tracing::info!(project = %ws.project_id, error = %e, "coordinator not told about a memory entry");
        }
    });
}

/// Reject a memory proposal. The rejected proposal is returned and also
/// arrives as a `memory.rejected` event.
#[utoipa::path(
    post,
    path = "/v1/projects/{id}/memory/proposals/{proposal_id}:reject",
    tag = "memory",
    params(
        ("id" = String, Path, description = "Project id"),
        ("proposal_id" = String, Path, description = "Memory proposal id")
    ),
    request_body = RejectMemoryProposal,
    responses(
        (status = 200, body = MemoryProposal),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`already_decided`")
    )
)]
pub async fn reject(
    State(state): State<AppState>,
    Path((project_id, id)): Path<(String, String)>,
    Json(input): Json<RejectMemoryProposal>,
) -> Result<Json<MemoryProposal>, ApiError> {
    let who = decided_by(input.decided_by.as_deref())?;
    let _one = COMMITTING.lock().await;
    Ok(Json(
        db(&state, move |s| {
            s.reject_memory_proposal(&project_id, &id, &who)
        })
        .await
        .map_err(|e| match e.code() {
            "conflict" => already_decided("the proposal is already decided".into()),
            _ => e,
        })?,
    ))
}

/// The Project's memory: every entry on the Project repo's `main` under
/// `memory/`, by path, including files people added by hand. Empty for a
/// Project without a Project repo.
#[utoipa::path(
    get,
    path = "/v1/projects/{id}/memory",
    tag = "memory",
    params(("id" = String, Path, description = "Project id")),
    responses(
        (status = 200, body = Vec<MemoryEntry>),
        (status = 404, body = ErrorBody),
        (status = 500, body = ErrorBody, description = "`project_repo_failed`: the repo could not be read")
    )
)]
pub async fn list_entries(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<MemoryEntry>>, ApiError> {
    Ok(Json(entries_of(&state, &id).await?))
}

fn repo_failed(e: String) -> ApiError {
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "project_repo_failed", e)
}

/// The Project's entries, read from its Project repo off the async runtime.
async fn entries_of(state: &AppState, id: &str) -> Result<Vec<MemoryEntry>, ApiError> {
    let project = {
        let id = id.to_string();
        db(state, move |s| s.get_project(&id)).await?
    };
    let Some(bare) = project.project_repo_path else {
        return Ok(Vec::new());
    };
    let id = id.to_string();
    tokio::task::spawn_blocking(move || memory::list(std::path::Path::new(&bare), &id))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .map_err(repo_failed)
}

/// A Project repo commit that touched `memory/`, with its diff there: what
/// an entry's `commit` links to.
#[utoipa::path(
    get,
    path = "/v1/projects/{id}/memory/commits/{commit}",
    tag = "memory",
    params(
        ("id" = String, Path, description = "Project id"),
        ("commit" = String, Path, description = "Commit id on the Project repo's `main`")
    ),
    responses(
        (status = 200, body = MemoryCommit),
        (status = 404, body = ErrorBody),
        (status = 500, body = ErrorBody, description = "`project_repo_failed`: the repo could not be read")
    )
)]
pub async fn get_commit(
    State(state): State<AppState>,
    Path((id, commit)): Path<(String, String)>,
) -> Result<Json<MemoryCommit>, ApiError> {
    let project = db(&state, move |s| s.get_project(&id)).await?;
    let Some(bare) = project.project_repo_path else {
        return Err(ApiError::not_found());
    };
    tokio::task::spawn_blocking(move || memory::commit(std::path::Path::new(&bare), &commit))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .map_err(repo_failed)?
        .map(Json)
        .ok_or_else(ApiError::not_found)
}

/// Dispatches `POST /v1/projects/{id}/memory/{entry_id}:promote`.
pub async fn entry_action(
    state: State<AppState>,
    Path((project_id, target)): Path<(String, String)>,
    body: Bytes,
) -> Result<Json<UserMemoryEntry>, ApiError> {
    let Some((id, action)) = target.rsplit_once(':') else {
        return Err(ApiError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "use POST .../memory/{entry_id}:promote",
        ));
    };
    match action {
        "promote" => {
            let path = Path((project_id, id.to_string()));
            promote(state, path, Json(parse_body(&body)?)).await
        }
        _ => Err(ApiError::not_found()),
    }
}

/// Promote a Project memory entry to user-level memory.
///
/// The entry is copied as one new file into the user-level memory directory
/// (`~/.quark/memory/` unless the daemon was started with another), which
/// every Project's coordinator reads, and each coordinator is told about it.
/// The Project's own entry stays. Promoting an entry again returns the copy
/// made the first time.
#[utoipa::path(
    post,
    path = "/v1/projects/{id}/memory/{entry_id}:promote",
    tag = "memory",
    params(
        ("id" = String, Path, description = "Project id"),
        ("entry_id" = String, Path, description = "Memory entry id")
    ),
    request_body = PromoteMemoryEntry,
    responses(
        (status = 200, body = UserMemoryEntry),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 500, body = ErrorBody, description = "`project_repo_failed`, or `user_memory_failed`: the file could not be written")
    )
)]
pub async fn promote(
    State(state): State<AppState>,
    Path((project_id, id)): Path<(String, String)>,
    Json(input): Json<PromoteMemoryEntry>,
) -> Result<Json<UserMemoryEntry>, ApiError> {
    let who = decided_by(input.promoted_by.as_deref())?;
    let project = {
        let p = project_id.clone();
        db(&state, move |s| s.get_project(&p)).await?
    };
    let entry = entries_of(&state, &project_id)
        .await?
        .into_iter()
        .find(|e| e.id == id)
        .ok_or_else(ApiError::not_found)?;
    let dir = state.layout.user_memory();
    let _one = PROMOTING.lock().await;
    let (promoted, written) = tokio::task::spawn_blocking(move || {
        memory::promote(&dir, &entry, &project.name, &crate::now_rfc3339(), &who)
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .map_err(|e| {
        tracing::warn!(project = %project_id, error = %e, "promoting a memory entry failed");
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "user_memory_failed", e)
    })?;
    if written {
        tell_coordinators(&state, &promoted);
    }
    Ok(Json(promoted))
}

/// Tells every Project's coordinator about a new user-level entry, in the
/// background: the file is what counts, and a coordinator without a live
/// session is not told.
fn tell_coordinators(state: &AppState, entry: &UserMemoryEntry) {
    let text = format!(
        "Memory shared by every Project: a new entry landed at {}. \
         Read it and apply it from your next plan on:\n\n{}",
        entry.path, entry.text
    );
    let state = state.clone();
    tokio::spawn(async move {
        let Ok(projects) = db(&state, |s| s.list_projects()).await else {
            return;
        };
        for p in projects {
            let Some(root) = p.workspace_path else {
                continue;
            };
            let ws = WorkspaceRef {
                project_id: p.id,
                root: PathBuf::from(root),
            };
            if let Err(e) = state.chat.send(&ws, &text).await {
                tracing::info!(project = %ws.project_id, error = %e, "coordinator not told about a user-level memory entry");
            }
        }
    });
}

/// User-level memory: every entry in the user-level memory directory, by
/// file name, including files people added by hand.
#[utoipa::path(
    get,
    path = "/v1/memory",
    tag = "memory",
    responses(
        (status = 200, body = Vec<UserMemoryEntry>),
        (status = 500, body = ErrorBody, description = "`user_memory_failed`: the directory could not be read")
    )
)]
pub async fn list_user_entries(
    State(state): State<AppState>,
) -> Result<Json<Vec<UserMemoryEntry>>, ApiError> {
    let dir = state.layout.user_memory();
    let entries = tokio::task::spawn_blocking(move || memory::list_user(&dir))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "user_memory_failed", e))?;
    Ok(Json(entries))
}

/// This module's endpoints, plus schemas the generator does not reach from
/// them (event payloads), merged into the served document by
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(
        list_proposals,
        accept,
        reject,
        list_entries,
        get_commit,
        promote,
        list_user_entries
    ),
    components(schemas(quark_systems::MemorySource, quark_systems::MemoryProposalState,))
)]
pub(super) struct Api;

/// This module's routes, merged into the `/v1` router.
pub(super) fn router() -> axum::Router<AppState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/v1/projects/{id}/memory", get(list_entries))
        // POST serves the custom method `.../memory/{entry_id}:promote`.
        .route("/v1/projects/{id}/memory/{entry_id}", post(entry_action))
        .route("/v1/projects/{id}/memory/commits/{commit}", get(get_commit))
        .route("/v1/memory", get(list_user_entries))
        .route("/v1/projects/{id}/memory/proposals", get(list_proposals))
        // POST serves the custom methods `.../{proposal_id}:accept` and `:reject`.
        .route(
            "/v1/projects/{id}/memory/proposals/{proposal_id}",
            post(proposal_action),
        )
}
