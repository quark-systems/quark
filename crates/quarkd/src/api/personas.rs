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
    db(&state, move |s| s.get_project(&pid)).await?;
    let src = source(&state);
    Ok(Json(
        blocking(move || {
            src.set_project(&id, input.persona.as_deref())
                .map_err(api_error)?;
            project_persona(&src, id)
        })
        .await?,
    ))
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
