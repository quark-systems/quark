//! Persona packs: list them, pick the global default, and pick or clear a
//! Project's own. Packs and choices live under the Quark home
//! ([`quark_persona::FilePersonas`]), read fresh on every call.

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::Json;
use quark_core::persona::PersonaPack;
use quark_core::CoreError;
use quark_persona::{role_key, FilePersonas, Origin};
use quark_systems::{
    ErrorBody, Persona, PersonaList, ProjectPersona, SetDefaultPersona, SetProjectPersona,
};

use super::{db, ApiError, AppState};
use crate::engine::WorkspaceRef;

fn source(state: &AppState) -> FilePersonas {
    FilePersonas::new(state.layout.home.clone())
}

fn api_error(e: CoreError) -> ApiError {
    match e {
        CoreError::Invalid(m) => ApiError::invalid(m),
        CoreError::NotFound(_) => ApiError::not_found(),
        e => ApiError::internal(e.to_string()),
    }
}

fn persona(pack: &PersonaPack, origin: &Origin) -> Persona {
    Persona {
        id: pack.id.clone(),
        name: pack.name.clone(),
        builtin: *origin == Origin::Builtin,
        address: pack.address.clone(),
        voice: pack.voice.clone(),
        vocabulary: pack.vocabulary.clone(),
        roles: pack
            .roles
            .iter()
            .map(|(r, l)| (role_key(*r).to_string(), l.clone()))
            .collect::<BTreeMap<_, _>>(),
        ui_labels: pack.ui_labels.clone(),
    }
}

/// Packs and the selection file are small local files; read them off the
/// async runtime anyway, like the store.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
}

fn list_packs(src: &FilePersonas) -> Result<PersonaList, ApiError> {
    let selection = src.selection().map_err(api_error)?;
    let (packs, errors) = src.load();
    Ok(PersonaList {
        default: selection.default_id().to_string(),
        packs: packs.iter().map(|(p, o)| persona(p, o)).collect(),
        errors,
    })
}

fn project_persona(src: &FilePersonas, project: String) -> Result<ProjectPersona, ApiError> {
    let r = src.resolve(&project).map_err(api_error)?;
    let (packs, _) = src.load();
    let origin = packs
        .iter()
        .find(|(p, _)| p.id == r.pack.id)
        .map_or(Origin::Builtin, |(_, o)| o.clone());
    Ok(ProjectPersona {
        project_id: project,
        persona: persona(&r.pack, &origin),
        project_override: r.project_override,
        default: r.default,
        fallback: r.fallback,
    })
}

/// Every installed persona pack, the built-in ones first, and the global
/// default.
#[utoipa::path(
    get,
    path = "/v1/personas",
    tag = "personas",
    responses((status = 200, body = PersonaList))
)]
pub async fn list(State(state): State<AppState>) -> Result<Json<PersonaList>, ApiError> {
    let src = source(&state);
    Ok(Json(blocking(move || list_packs(&src)).await?))
}

/// Choose the pack every Project uses unless it picks its own.
#[utoipa::path(
    put,
    path = "/v1/personas/default",
    tag = "personas",
    request_body = SetDefaultPersona,
    responses(
        (status = 200, body = PersonaList),
        (status = 400, body = ErrorBody)
    )
)]
pub async fn set_default(
    State(state): State<AppState>,
    Json(input): Json<SetDefaultPersona>,
) -> Result<Json<PersonaList>, ApiError> {
    let src = source(&state);
    Ok(Json(
        blocking(move || {
            src.set_default(&input.persona).map_err(api_error)?;
            list_packs(&src)
        })
        .await?,
    ))
}

/// The pack a Project reads with: its role names, form of address and app
/// labels.
#[utoipa::path(
    get,
    path = "/v1/projects/{id}/persona",
    tag = "personas",
    params(("id" = String, Path, description = "Project id")),
    responses(
        (status = 200, body = ProjectPersona),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn get_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ProjectPersona>, ApiError> {
    let pid = id.clone();
    db(&state, move |s| s.get_project(&pid)).await?;
    let src = source(&state);
    Ok(Json(blocking(move || project_persona(&src, id)).await?))
}

/// Pick a Project's own pack, or send `null` to follow the global default.
#[utoipa::path(
    put,
    path = "/v1/projects/{id}/persona",
    tag = "personas",
    params(("id" = String, Path, description = "Project id")),
    request_body = SetProjectPersona,
    responses(
        (status = 200, body = ProjectPersona),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn set_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<SetProjectPersona>,
) -> Result<Json<ProjectPersona>, ApiError> {
    let pid = id.clone();
    let project = db(&state, move |s| s.get_project(&pid)).await?;
    let src = source(&state);
    let (shown, note) = blocking(move || {
        let before = src.resolve(&id).map_err(api_error)?.pack;
        src.set_project(&id, input.persona.as_deref())
            .map_err(api_error)?;
        let after = src.resolve(&id).map_err(api_error)?.pack;
        Ok((project_persona(&src, id)?, switch_note(&before, &after)))
    })
    .await?;
    if let Some(root) = project.workspace_path {
        // In the background: the choice is what counts, and a coordinator
        // without a live session reads its persona when it next starts.
        let chat = state.chat.clone();
        let ws = WorkspaceRef {
            project_id: project.id,
            root: root.into(),
        };
        tokio::spawn(async move {
            if let Err(e) = chat.send(&ws, &note).await {
                tracing::info!(project = %ws.project_id, error = %e, "coordinator not told about its persona");
            }
        });
    }
    Ok(Json(shown))
}

/// What a coordinator is told when the user picks its Project's persona. Its
/// prompt carries the pack too, but a conversation written in another voice
/// outweighs the prompt, so the switch is also said where the coordinator
/// reads it last: in the conversation, naming what to stop.
fn switch_note(before: &PersonaPack, after: &PersonaPack) -> String {
    let mut what = format!(
        "the user set this Project's persona to {}. It applies from your next message on, \
         whatever voice your earlier messages used.",
        after.name
    );
    if !after.voice.trim().is_empty() {
        what.push(' ');
        what.push_str(after.voice.trim());
    }
    match (&after.address, &before.address) {
        (Some(a), _) => what.push_str(&format!(" Address the user as \"{a}\".")),
        (None, Some(b)) => what.push_str(&format!(
            " Stop addressing the user as \"{b}\": use no title or nickname at all."
        )),
        (None, None) => what.push_str(" Address the user with no title or nickname at all."),
    }
    crate::chat::engine_note(&what, "")
}

#[derive(utoipa::OpenApi)]
#[openapi(paths(list, set_default, get_project, set_project))]
pub(super) struct Api;

/// This module's routes, merged into the `/v1` router.
pub(super) fn router() -> axum::Router<AppState> {
    use axum::routing::{get, put};
    axum::Router::new()
        .route("/v1/personas", get(list))
        .route("/v1/personas/default", put(set_default))
        .route(
            "/v1/projects/{id}/persona",
            get(get_project).put(set_project),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(name: &str, address: Option<&str>, voice: &str) -> PersonaPack {
        PersonaPack {
            id: name.to_lowercase(),
            name: name.into(),
            address: address.map(str::to_string),
            voice: voice.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_switch_names_the_address_to_stop_or_start() {
        let nautical = pack("Nautical", Some("captain"), "Speak like a first mate.");
        let plain = pack("Plain", None, "Write plainly.");
        assert_eq!(
            switch_note(&nautical, &plain),
            "Engine note: the user set this Project's persona to Plain. It applies from your next message on, \
             whatever voice your earlier messages used. Write plainly. \
             Stop addressing the user as \"captain\": use no title or nickname at all."
        );
        assert!(switch_note(&plain, &nautical).ends_with("Address the user as \"captain\"."));
        assert!(switch_note(&plain, &plain)
            .ends_with("Address the user with no title or nickname at all."));
    }
}
