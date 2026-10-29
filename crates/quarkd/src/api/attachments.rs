//! `POST /v1/projects/{id}/attachments`: a file pasted or dropped into a
//! prompt box. The daemon keeps it under `<home>/attachments/<project>/` and
//! answers with its path, which the app puts in the message so the agent
//! can read the file.

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use super::{db, ApiError, AppState};

/// Largest file accepted, in bytes.
pub const MAX_BYTES: usize = 25 * 1024 * 1024;
/// Longest file name kept, in bytes.
const NAME_MAX: usize = 120;

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct AttachmentQuery {
    /// The file's name, e.g. `screenshot.png`. Only its last path part is
    /// used; characters outside letters, digits, `.`, `-` and `_` become `_`.
    pub name: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct Attachment {
    /// Absolute path of the saved file on the daemon's machine.
    pub path: String,
    /// The file name as saved.
    pub name: String,
    pub bytes: u64,
}

/// Saves the request body as a file an agent of this Project can read.
/// Each upload gets its own folder, so two files with the same name never
/// clash.
#[utoipa::path(
    post,
    path = "/v1/projects/{id}/attachments",
    tag = "projects",
    params(("id" = String, Path, description = "Project id"), AttachmentQuery),
    request_body(content = Vec<u8>, content_type = "application/octet-stream"),
    responses(
        (status = 201, body = Attachment),
        (status = 400, description = "Empty body"),
        (status = 404, description = "No such Project"),
        (status = 413, description = "Larger than 25 MB")
    )
)]
pub async fn upload(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<AttachmentQuery>,
    body: Bytes,
) -> Result<(StatusCode, Json<Attachment>), ApiError> {
    if body.is_empty() {
        return Err(ApiError::invalid("the file is empty"));
    }
    let project_id = id.clone();
    let project = db(&state, move |s| s.get_project(&project_id)).await?;
    let name = file_name(q.name.as_deref().unwrap_or(""));
    let dir = state
        .layout
        .home
        .join("attachments")
        .join(&project.id)
        .join(uuid::Uuid::new_v4().simple().to_string());
    let path = dir.join(&name);
    let bytes = body.len() as u64;
    let saved = path.clone();
    tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&dir)?;
        std::fs::write(&saved, &body)
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .map_err(|e| ApiError::internal(format!("saving {}: {e}", path.display())))?;
    Ok((
        StatusCode::CREATED,
        Json(Attachment {
            path: path.to_string_lossy().into_owned(),
            name,
            bytes,
        }),
    ))
}

/// A safe file name: the last path part, with anything unusual replaced,
/// never empty, hidden or `..`.
pub fn file_name(raw: &str) -> String {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let mut name: String = last
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    name = name.trim_start_matches('.').to_string();
    if name.len() > NAME_MAX {
        // Keep the extension so the agent still knows the file's type.
        let ext = name
            .rfind('.')
            .map(|i| name[i..].to_string())
            .filter(|e| e.len() <= 16)
            .unwrap_or_default();
        name.truncate(NAME_MAX - ext.len());
        name.push_str(&ext);
    }
    if name.is_empty() {
        "attachment".into()
    } else {
        name
    }
}

/// This module's endpoints, merged into the served document by
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(upload), components(schemas(Attachment)))]
pub(super) struct Api;

/// This module's routes, merged into the `/v1` router.
pub(super) fn router() -> axum::Router<AppState> {
    use axum::routing::post;
    axum::Router::new().route(
        "/v1/projects/{id}/attachments",
        post(upload).layer(DefaultBodyLimit::max(MAX_BYTES)),
    )
}

#[cfg(test)]
mod tests {
    use super::file_name;

    #[test]
    fn names_are_made_safe() {
        assert_eq!(file_name("shot.png"), "shot.png");
        assert_eq!(file_name("/etc/../passwd"), "passwd");
        assert_eq!(file_name("..\\..\\x.txt"), "x.txt");
        assert_eq!(file_name(".."), "attachment");
        assert_eq!(file_name(".env"), "env");
        assert_eq!(file_name(""), "attachment");
        assert_eq!(file_name("Screen Shot 1.png"), "Screen_Shot_1.png");
        let long = format!("{}.png", "a".repeat(300));
        let n = file_name(&long);
        assert_eq!(n.len(), 120);
        assert!(n.ends_with(".png"));
    }
}
