//! Beads: the Project's Beads database (`crate::beads`), its issues and
//! memories, and the New issue side chat in which the coordinator drafts
//! beads before any exist.

use std::path::PathBuf;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use quark_systems::{
    AcceptIssueDraft, BeadsMemory, BeadsState, BeadsStatus, DraftMessage, DraftRole, DraftText,
    ErrorBody, Issue, IssueDetail, IssueDraft, IssueDraftState, WriteIssueDraft,
};
use serde::Deserialize;
use utoipa::IntoParams;

use super::{db, ApiError, AppState};
use crate::beads::{validate_drafts, BeadsError};
use crate::chat::MAX_MESSAGE_BYTES;
use crate::engine::WorkspaceRef;

impl From<BeadsError> for ApiError {
    fn from(e: BeadsError) -> Self {
        let message = e.to_string();
        match e {
            BeadsError::Missing => ApiError::new(StatusCode::CONFLICT, "no_beads", message),
            BeadsError::Unavailable(_) => ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "beads_unavailable",
                message,
            ),
            BeadsError::Bd { .. } => {
                ApiError::new(StatusCode::BAD_GATEWAY, "beads_failed", message)
            }
            BeadsError::NotFound(_) => ApiError::new(StatusCode::NOT_FOUND, "not_found", message),
            BeadsError::Invalid(m) => ApiError::invalid(m),
        }
    }
}

/// The Project, or 404.
async fn project(state: &AppState, id: &str) -> Result<quark_systems::Project, ApiError> {
    let id = id.to_string();
    db(state, move |s| s.get_project(&id)).await
}

/// The Project's Beads database.
#[utoipa::path(
    get,
    path = "/v1/projects/{id}/beads",
    tag = "beads",
    params(("id" = String, Path, description = "Project id")),
    responses((status = 200, body = BeadsStatus), (status = 404, body = ErrorBody))
)]
pub async fn get_beads(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<BeadsStatus>, ApiError> {
    project(&state, &id).await?;
    Ok(Json(state.beads.status(&state.layout.home, &id).await))
}

/// Create the Project's Beads database in server mode.
///
/// When the Project's first repo tracks a `.beads` that syncs to a Dolt
/// remote, the database is cloned from it, so existing issues carry over.
/// Returns at once with `setting_up`; progress and the outcome arrive as
/// `beads.status` events. A Project that already has one gets its status.
#[utoipa::path(
    post,
    path = "/v1/projects/{id}/beads:setup",
    tag = "beads",
    params(("id" = String, Path, description = "Project id")),
    responses((status = 200, body = BeadsStatus), (status = 404, body = ErrorBody))
)]
pub async fn setup_beads(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<BeadsStatus>, ApiError> {
    let p = project(&state, &id).await?;
    Ok(Json(
        state
            .beads
            .start_setup(state.store.clone(), state.layout.home.clone(), p)
            .await,
    ))
}

/// Sync the Project's beads with GitHub Issues now, both ways.
///
/// Pulls every issue, then pushes every bead except decision beads. Uses
/// `GITHUB_TOKEN`, else the token `gh` is signed in with. The outcome is
/// kept as `last_sync`, failed or not.
#[utoipa::path(
    post,
    path = "/v1/projects/{id}/beads:sync",
    tag = "beads",
    params(("id" = String, Path, description = "Project id")),
    responses(
        (status = 200, body = BeadsStatus, description = "Synced, or `last_sync` says why not"),
        (status = 400, body = ErrorBody, description = "The Project's first repo is not on GitHub"),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`no_beads`")
    )
)]
pub async fn sync_beads(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<BeadsStatus>, ApiError> {
    project(&state, &id).await?;
    Ok(Json(
        state
            .beads
            .sync(&state.store, &state.layout.home, &id)
            .await?,
    ))
}

#[derive(Debug, Clone, Copy, Default, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IssueFilter {
    /// Open, with nothing it waits for still open.
    Ready,
    InProgress,
    /// Waiting for an open issue, or marked blocked.
    Blocked,
    Closed,
    /// Everything not closed.
    Open,
    #[default]
    All,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct IssueQuery {
    /// Default `all`.
    #[param(inline)]
    pub filter: Option<IssueFilter>,
}

fn keep(filter: IssueFilter, i: &Issue) -> bool {
    match filter {
        IssueFilter::Ready => i.ready,
        IssueFilter::InProgress => i.status == "in_progress",
        IssueFilter::Blocked => i.blocked && i.status != "closed",
        IssueFilter::Closed => i.status == "closed",
        IssueFilter::Open => i.status != "closed",
        IssueFilter::All => true,
    }
}

/// The Project's issues: open ones first, then by priority, newest first.
#[utoipa::path(
    get,
    path = "/v1/projects/{id}/issues",
    tag = "beads",
    params(("id" = String, Path, description = "Project id"), IssueQuery),
    responses(
        (status = 200, body = Vec<Issue>),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`no_beads`")
    )
)]
pub async fn list_issues(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<IssueQuery>,
) -> Result<Json<Vec<Issue>>, ApiError> {
    project(&state, &id).await?;
    let filter = q.filter.unwrap_or_default();
    let all = state.beads.issues(&state.layout.home, &id).await?;
    Ok(Json(all.into_iter().filter(|i| keep(filter, i)).collect()))
}

/// One issue with what blocks it, what it blocks, its links and comments.
#[utoipa::path(
    get,
    path = "/v1/projects/{id}/issues/{issue_id}",
    tag = "beads",
    params(
        ("id" = String, Path, description = "Project id"),
        ("issue_id" = String, Path, description = "Bead id, e.g. `qk-44`")
    ),
    responses(
        (status = 200, body = IssueDetail),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`no_beads`")
    )
)]
pub async fn get_issue(
    State(state): State<AppState>,
    Path((id, issue)): Path<(String, String)>,
) -> Result<Json<IssueDetail>, ApiError> {
    project(&state, &id).await?;
    Ok(Json(
        state.beads.issue(&state.layout.home, &id, &issue).await?,
    ))
}

/// The Project's Beads memories, which `bd prime` gives every agent.
#[utoipa::path(
    get,
    path = "/v1/projects/{id}/beads/memories",
    tag = "beads",
    params(("id" = String, Path, description = "Project id")),
    responses(
        (status = 200, body = Vec<BeadsMemory>),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`no_beads`")
    )
)]
pub async fn list_memories(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<BeadsMemory>>, ApiError> {
    project(&state, &id).await?;
    Ok(Json(
        state
            .beads
            .memories(&state.store, &state.layout.home, &id)
            .await?,
    ))
}

/// Forget one of the Project's Beads memories.
#[utoipa::path(
    delete,
    path = "/v1/projects/{id}/beads/memories/{key}",
    tag = "beads",
    params(
        ("id" = String, Path, description = "Project id"),
        ("key" = String, Path, description = "Memory key")
    ),
    responses(
        (status = 204),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`no_beads`")
    )
)]
pub async fn forget_memory(
    State(state): State<AppState>,
    Path((id, key)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    project(&state, &id).await?;
    state
        .beads
        .forget(&state.store, &state.layout.home, &id, &key)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The Project's open New issue drafts, oldest first.
#[utoipa::path(
    get,
    path = "/v1/projects/{id}/issue-drafts",
    tag = "beads",
    params(("id" = String, Path, description = "Project id")),
    responses((status = 200, body = Vec<IssueDraft>), (status = 404, body = ErrorBody))
)]
pub async fn list_drafts(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<IssueDraft>>, ApiError> {
    Ok(Json(
        db(&state, move |s| {
            s.list_issue_drafts(&id, IssueDraftState::Open)
        })
        .await?,
    ))
}

fn message_text(input: &DraftText) -> Result<String, ApiError> {
    let text = input.text.trim();
    if text.is_empty() {
        return Err(ApiError::invalid("text must not be empty"));
    }
    if text.len() > MAX_MESSAGE_BYTES {
        return Err(ApiError::invalid(format!(
            "text is longer than {MAX_MESSAGE_BYTES} bytes"
        )));
    }
    Ok(text.to_string())
}

/// Start a New issue draft with the user's description of the work.
///
/// The message goes to the Project's coordinator, which drafts beads and
/// sends them back with `PUT /v1/issue-drafts/{id}`; they arrive as
/// `issue_draft.updated` events. Nothing is created in Beads until the draft
/// is accepted. When the coordinator cannot be reached the draft says so.
#[utoipa::path(
    post,
    path = "/v1/projects/{id}/issue-drafts",
    tag = "beads",
    params(("id" = String, Path, description = "Project id")),
    request_body = DraftText,
    responses(
        (status = 200, body = IssueDraft),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn start_draft(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<DraftText>,
) -> Result<Json<IssueDraft>, ApiError> {
    let text = message_text(&input)?;
    let p = project(&state, &id).await?;
    let draft = {
        let (id, text) = (id.clone(), text.clone());
        db(&state, move |s| s.create_issue_draft(&id, &text)).await?
    };
    let ask = first_ask(state.beads.api_base(), &draft.id, &text);
    Ok(Json(send(&state, &p, draft, ask).await?))
}

/// Add a message to an open draft ("make the first one P0"); the
/// coordinator revises its drafts.
#[utoipa::path(
    post,
    path = "/v1/issue-drafts/{id}/messages",
    tag = "beads",
    params(("id" = String, Path, description = "Draft id")),
    request_body = DraftText,
    responses(
        (status = 200, body = IssueDraft),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "The draft is closed")
    )
)]
pub async fn refine_draft(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<DraftText>,
) -> Result<Json<IssueDraft>, ApiError> {
    let text = message_text(&input)?;
    let draft = {
        let (id, text) = (id.clone(), text.clone());
        db(&state, move |s| {
            s.update_issue_draft(&id, |d| {
                d.messages.push(DraftMessage {
                    role: DraftRole::User,
                    text,
                    at: crate::now_rfc3339(),
                });
                d.waiting = true;
                Ok(())
            })
        })
        .await?
    };
    let p = project(&state, &draft.project_id).await?;
    let current = serde_json::to_string(&draft.issues).unwrap_or_default();
    let ask = refine_ask(state.beads.api_base(), &draft.id, &text, &current);
    Ok(Json(send(&state, &p, draft, ask).await?))
}

/// Types `ask` into the coordinator; a draft it cannot reach stops waiting
/// and says why in the side chat.
async fn send(
    state: &AppState,
    p: &quark_systems::Project,
    draft: IssueDraft,
    ask: String,
) -> Result<IssueDraft, ApiError> {
    let failure = match &p.workspace_path {
        None => Some("the Project has no workspace, so it has no coordinator".to_string()),
        Some(root) => {
            let ws = WorkspaceRef {
                project_id: p.id.clone(),
                root: PathBuf::from(root),
            };
            state
                .chat
                .send(&ws, &ask)
                .await
                .err()
                .map(|e| e.to_string())
        }
    };
    let Some(why) = failure else {
        return Ok(draft);
    };
    let id = draft.id.clone();
    db(state, move |s| {
        s.update_issue_draft(&id, |d| {
            d.waiting = false;
            d.messages.push(DraftMessage {
                role: DraftRole::Coordinator,
                text: format!("I could not reach the coordinator, so nothing was drafted: {why}"),
                at: crate::now_rfc3339(),
            });
            Ok(())
        })
    })
    .await
}

/// How the coordinator sends its drafts back.
fn put_command(base: &str, draft_id: &str) -> String {
    format!(
        "curl -sS -X PUT {base}/v1/issue-drafts/{draft_id} -H 'content-type: application/json' --data-binary @- <<'JSON'\n\
         {{\"reply\": \"<one or two sentences for the side chat>\", \"issues\": [{{\"key\": \"1\", \"title\": \"...\", \
         \"issue_type\": \"bug|feature|task|chore|epic\", \"priority\": 2, \"labels\": [], \"description\": \"...\", \
         \"blocked_by\": []}}], \"related\": []}}\nJSON"
    )
}

const DRAFT_RULES: &str = "Do not create any beads yourself: nothing exists until the user accepts the drafts in Quark. \
Check the open beads first (`bd list`, `bd search`) so you neither duplicate one nor miss a related one. \
Split the work where it is really separate pieces. `blocked_by` names another draft's key or an existing issue id; \
`related` lists existing issue ids that are related but separate. Priority is 0 (highest) to 4. \
Send the whole set each time; it replaces the previous drafts.";

fn first_ask(base: &str, draft_id: &str, text: &str) -> String {
    format!(
        "New issue draft {draft_id}. The user describes work to file as issues:\n\n{text}\n\n\
         Draft beads for it. {DRAFT_RULES} Send your drafts with:\n\n{}",
        put_command(base, draft_id)
    )
}

fn refine_ask(base: &str, draft_id: &str, text: &str, current: &str) -> String {
    format!(
        "New issue draft {draft_id}: the user replied:\n\n{text}\n\n\
         The drafts are now: {current}\n\n\
         Revise them. {DRAFT_RULES} Send the revised drafts with:\n\n{}",
        put_command(base, draft_id)
    )
}

/// The coordinator's drafts for an open draft, replacing earlier ones.
#[utoipa::path(
    put,
    path = "/v1/issue-drafts/{id}",
    tag = "beads",
    params(("id" = String, Path, description = "Draft id")),
    request_body = WriteIssueDraft,
    responses(
        (status = 200, body = IssueDraft),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "The draft is closed")
    )
)]
pub async fn write_draft(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<WriteIssueDraft>,
) -> Result<Json<IssueDraft>, ApiError> {
    validate_drafts(&input.issues).map_err(ApiError::invalid)?;
    if input
        .reply
        .as_ref()
        .is_some_and(|r| r.len() > MAX_MESSAGE_BYTES)
    {
        return Err(ApiError::invalid("reply is too long"));
    }
    Ok(Json(
        db(&state, move |s| {
            s.update_issue_draft(&id, |d| {
                if let Some(reply) = input
                    .reply
                    .map(|r| r.trim().to_string())
                    .filter(|r| !r.is_empty())
                {
                    d.messages.push(DraftMessage {
                        role: DraftRole::Coordinator,
                        text: reply,
                        at: crate::now_rfc3339(),
                    });
                }
                d.issues = input.issues;
                d.related = input.related;
                d.waiting = false;
                Ok(())
            })
        })
        .await?,
    ))
}

/// Dispatches `POST /v1/issue-drafts/{id}:accept` and `:discard`.
pub async fn draft_action(
    state: State<AppState>,
    Path(target): Path<String>,
    body: Bytes,
) -> Result<Json<IssueDraft>, ApiError> {
    let Some((id, action)) = target.rsplit_once(':') else {
        return Err(ApiError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "use POST /v1/issue-drafts/{id}:accept or :discard",
        ));
    };
    match action {
        "accept" => {
            let input = if body.iter().all(u8::is_ascii_whitespace) {
                AcceptIssueDraft::default()
            } else {
                serde_json::from_slice(&body)
                    .map_err(|e| ApiError::invalid(format!("invalid body: {e}")))?
            };
            accept_draft(state, Path(id.to_string()), Json(input)).await
        }
        "discard" => discard_draft(state, Path(id.to_string())).await,
        _ => Err(ApiError::not_found()),
    }
}

/// One accept at a time, so a double click never creates the beads twice.
static ACCEPTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Create the drafted beads, as edited, in one Beads transaction.
///
/// The draft closes with the new ids by draft key. With `start_worker`, the
/// coordinator is asked to start a worker on the first one.
#[utoipa::path(
    post,
    path = "/v1/issue-drafts/{id}:accept",
    tag = "beads",
    params(("id" = String, Path, description = "Draft id")),
    request_body = AcceptIssueDraft,
    responses(
        (status = 200, body = IssueDraft),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`no_beads`, or the draft is closed"),
        (status = 502, body = ErrorBody, description = "`beads_failed`: nothing was created")
    )
)]
pub async fn accept_draft(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<AcceptIssueDraft>,
) -> Result<Json<IssueDraft>, ApiError> {
    let _one = ACCEPTING.lock().await;
    let draft = {
        let id = id.clone();
        db(&state, move |s| s.get_issue_draft(&id)).await?
    };
    if draft.state != IssueDraftState::Open {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "conflict",
            "the draft is already closed",
        ));
    }
    let issues = input.issues.unwrap_or(draft.issues);
    let created = state
        .beads
        .create(&state.layout.home, &draft.project_id, &issues)
        .await?;
    let first = issues
        .first()
        .and_then(|i| Some((created.get(&i.key)?.clone(), i.title.clone())));
    let accepted = db(&state, move |s| {
        s.update_issue_draft(&id, |d| {
            d.issues = issues;
            d.created = created;
            d.state = IssueDraftState::Accepted;
            d.waiting = false;
            Ok(())
        })
    })
    .await?;
    if input.start_worker {
        if let Some((bead, title)) = first {
            let p = project(&state, &accepted.project_id).await?;
            if let Some(root) = p.workspace_path {
                let ws = WorkspaceRef {
                    project_id: p.id,
                    root: PathBuf::from(root),
                };
                let text = format!("Start a worker on {bead}: {title}");
                if let Err(e) = state.chat.send(&ws, &text).await {
                    tracing::info!(project = %ws.project_id, error = %e, "coordinator not asked to start a worker");
                }
            }
        }
    }
    Ok(Json(accepted))
}

/// Close a draft without creating anything.
#[utoipa::path(
    post,
    path = "/v1/issue-drafts/{id}:discard",
    tag = "beads",
    params(("id" = String, Path, description = "Draft id")),
    responses(
        (status = 200, body = IssueDraft),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "The draft is closed")
    )
)]
pub async fn discard_draft(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<IssueDraft>, ApiError> {
    Ok(Json(
        db(&state, move |s| {
            s.update_issue_draft(&id, |d| {
                d.state = IssueDraftState::Discarded;
                d.waiting = false;
                Ok(())
            })
        })
        .await?,
    ))
}

/// Whether the Project's Beads database is ready; Memory uses it to decide
/// where an accepted learning goes.
pub(super) async fn ready(state: &AppState, project_id: &str) -> bool {
    state
        .beads
        .status(&state.layout.home, project_id)
        .await
        .state
        == BeadsState::Ready
}

/// This module's endpoints, plus schemas the generator does not reach from
/// them (event payloads), merged into the served document by
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(
        get_beads,
        setup_beads,
        sync_beads,
        list_issues,
        get_issue,
        list_memories,
        forget_memory,
        list_drafts,
        start_draft,
        refine_draft,
        write_draft,
        accept_draft,
        discard_draft
    ),
    components(schemas(
        quark_systems::BeadsChanged,
        quark_systems::BeadsState,
        quark_systems::IssueDraftState,
        quark_systems::DraftRole,
    ))
)]
pub(super) struct Api;

/// This module's routes, merged into the `/v1` router.
pub(super) fn router() -> axum::Router<AppState> {
    use axum::routing::{delete, get, post};
    axum::Router::new()
        .route("/v1/projects/{id}/beads", get(get_beads))
        .route("/v1/projects/{id}/beads:setup", post(setup_beads))
        .route("/v1/projects/{id}/beads:sync", post(sync_beads))
        .route("/v1/projects/{id}/issues", get(list_issues))
        .route("/v1/projects/{id}/issues/{issue_id}", get(get_issue))
        .route("/v1/projects/{id}/beads/memories", get(list_memories))
        .route(
            "/v1/projects/{id}/beads/memories/{key}",
            delete(forget_memory),
        )
        .route(
            "/v1/projects/{id}/issue-drafts",
            get(list_drafts).post(start_draft),
        )
        // POST serves the custom methods `/v1/issue-drafts/{id}:accept` and
        // `:discard`; PUT is the coordinator writing its drafts.
        .route("/v1/issue-drafts/{id}", post(draft_action).put(write_draft))
        .route("/v1/issue-drafts/{id}/messages", post(refine_draft))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ask_carries_the_text_and_how_to_answer() {
        let a = first_ask("http://127.0.0.1:7380", "draft_1", "PRs that go red");
        assert!(a.contains("PRs that go red"));
        assert!(a.contains("curl -sS -X PUT http://127.0.0.1:7380/v1/issue-drafts/draft_1"));
        assert!(a.contains("Do not create any beads yourself"));
        let r = refine_ask("http://h", "draft_1", "make it P0", "[]");
        assert!(r.contains("make it P0") && r.contains("The drafts are now: []"));
    }
}
