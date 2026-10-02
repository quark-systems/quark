//! Project memory (journey J8): review proposals from finished tasks, and
//! read the entries accepted into the Project repo's `memory/`.

use std::path::PathBuf;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use quark_systems::{
    AcceptMemoryProposal, ErrorBody, MemoryEntry, MemoryProposal, MemoryProposalState,
    RejectMemoryProposal,
};
use serde::Deserialize;
use utoipa::IntoParams;

use super::routes::{daemon_user, MAX_ANSWERED_BY_BYTES};
use super::{db, ApiError, AppState};
use crate::engine::WorkspaceRef;
use crate::memory;
use crate::project_repo;

/// One accept at a time, so two entries never race for a file name or for
/// the Project repo's `main`.
static ACCEPTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
            "decided_by must be one line of at most 128 bytes",
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
    let _one = ACCEPTING.lock().await;
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
    let _one = ACCEPTING.lock().await;
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
    let project = {
        let id = id.clone();
        db(&state, move |s| s.get_project(&id)).await?
    };
    let Some(bare) = project.project_repo_path else {
        return Ok(Json(Vec::new()));
    };
    let entries =
        tokio::task::spawn_blocking(move || memory::list(std::path::Path::new(&bare), &id))
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?
            .map_err(|e| {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "project_repo_failed", e)
            })?;
    Ok(Json(entries))
}
