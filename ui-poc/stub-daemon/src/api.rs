//! HTTP + WebSocket routes (see `ui-poc/CONTRACT.md`).

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;
use tower_http::cors::CorsLayer;

use crate::app::{AnswerError, App};
use crate::store::StoredEvent;

type AppState = State<Arc<App>>;

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/v1/projects", get(projects))
        .route("/v1/projects/{id}/tasks", get(tasks))
        .route("/v1/decisions", get(decisions))
        // `{id}:answer` is a single path segment; parsed in the handler.
        .route("/v1/decisions/{id_action}", post(answer_decision))
        .route("/v1/pull-requests", get(pull_requests))
        .route("/v1/pull-requests/{id}/diff", get(pr_diff))
        .route(
            "/v1/pull-requests/{id}/comments",
            get(comments).post(add_comment),
        )
        .route(
            "/v1/coordinators/{id}/messages",
            get(chat_history).post(post_chat),
        )
        .route("/v1/workers", get(workers))
        .route("/v1/workers/{id}/input", post(worker_input))
        .route("/v1/workers/{id}/resize", post(worker_resize))
        .route("/v1/workers/{id}/stress", post(worker_stress))
        .route("/v1/events", get(events))
        .layer(CorsLayer::very_permissive())
        .with_state(app)
}

fn error(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

fn not_found(what: &str) -> Response {
    error(StatusCode::NOT_FOUND, format!("{what} not found"))
}

async fn projects(State(app): AppState) -> Response {
    Json(app.data.lock().unwrap().project_list()).into_response()
}

async fn tasks(State(app): AppState, Path(id): Path<String>) -> Response {
    let data = app.data.lock().unwrap();
    if !data.projects.iter().any(|(p, _, _)| *p == id) {
        return not_found("project");
    }
    let tasks: Vec<_> = data
        .tasks
        .iter()
        .filter(|t| t.project_id == id)
        .cloned()
        .collect();
    Json(tasks).into_response()
}

async fn decisions(State(app): AppState) -> Response {
    Json(app.data.lock().unwrap().decisions.clone()).into_response()
}

#[derive(Deserialize)]
struct AnswerBody {
    option: usize,
}

async fn answer_decision(
    State(app): AppState,
    Path(id_action): Path<String>,
    Json(body): Json<AnswerBody>,
) -> Response {
    let Some(id) = id_action.strip_suffix(":answer") else {
        return not_found("route");
    };
    match app.answer_decision(id, body.option) {
        Ok(d) => Json(d).into_response(),
        Err(AnswerError::NotFound) => not_found("decision"),
        Err(AnswerError::AlreadyAnswered) => {
            error(StatusCode::CONFLICT, "decision already answered")
        }
        Err(AnswerError::OutOfRange) => {
            error(StatusCode::UNPROCESSABLE_ENTITY, "option out of range")
        }
    }
}

async fn pull_requests(State(app): AppState) -> Response {
    Json(app.data.lock().unwrap().prs.clone()).into_response()
}

async fn pr_diff(State(app): AppState, Path(id): Path<String>) -> Response {
    match app.data.lock().unwrap().diffs.get(&id) {
        Some(diff) => {
            ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], *diff).into_response()
        }
        None => not_found("pull request"),
    }
}

async fn comments(State(app): AppState, Path(id): Path<String>) -> Response {
    match app.data.lock().unwrap().comments.get(&id) {
        Some(c) => Json(c.clone()).into_response(),
        None => not_found("pull request"),
    }
}

#[derive(Deserialize)]
struct CommentBody {
    path: String,
    line: u32,
    body: String,
}

async fn add_comment(
    State(app): AppState,
    Path(id): Path<String>,
    Json(b): Json<CommentBody>,
) -> Response {
    match app.add_comment(&id, b.path, b.line, b.body) {
        Some(c) => Json(c).into_response(),
        None => not_found("pull request"),
    }
}

async fn chat_history(State(app): AppState, Path(id): Path<String>) -> Response {
    match app.data.lock().unwrap().chats.get(&id) {
        Some(m) => Json(m.clone()).into_response(),
        None => not_found("coordinator"),
    }
}

#[derive(Deserialize)]
struct ChatBody {
    text: String,
}

async fn post_chat(
    State(app): AppState,
    Path(id): Path<String>,
    Json(b): Json<ChatBody>,
) -> Response {
    if app.post_chat(&id, b.text) {
        StatusCode::ACCEPTED.into_response()
    } else {
        not_found("coordinator")
    }
}

async fn workers(State(app): AppState) -> Response {
    let workers = app.tmux.as_ref().map(|t| t.workers()).unwrap_or_default();
    Json(workers).into_response()
}

fn io_result(r: Option<std::io::Result<()>>) -> Response {
    match r {
        Some(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Some(Err(e)) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        None => not_found("worker"),
    }
}

#[derive(Deserialize)]
struct InputBody {
    data_b64: String,
}

async fn worker_input(
    State(app): AppState,
    Path(id): Path<String>,
    Json(b): Json<InputBody>,
) -> Response {
    let Some(tmux) = &app.tmux else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "tmux not running");
    };
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b.data_b64.as_bytes()) else {
        return error(StatusCode::BAD_REQUEST, "data_b64 is not valid base64");
    };
    io_result(tmux.input(&id, &bytes))
}

#[derive(Deserialize)]
struct ResizeBody {
    cols: u16,
    rows: u16,
}

async fn worker_resize(
    State(app): AppState,
    Path(id): Path<String>,
    Json(b): Json<ResizeBody>,
) -> Response {
    let Some(tmux) = &app.tmux else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "tmux not running");
    };
    match tmux.resize(&id, b.cols, b.rows) {
        Some(Ok(worker)) => Json(worker).into_response(),
        Some(Err(e)) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        None => not_found("worker"),
    }
}

#[derive(Deserialize)]
struct StressBody {
    on: bool,
}

async fn worker_stress(
    State(app): AppState,
    Path(id): Path<String>,
    Json(b): Json<StressBody>,
) -> Response {
    let Some(tmux) = &app.tmux else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "tmux not running");
    };
    io_result(tmux.stress(&id, b.on))
}

#[derive(Deserialize)]
struct EventsQuery {
    cursor: Option<u64>,
}

async fn events(
    State(app): AppState,
    Query(q): Query<EventsQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| ws_session(app, socket, q.cursor.unwrap_or(0)))
}

/// Replay retained events after `cursor`, then stream live. A client that
/// lags the broadcast channel is resynced from the store, so publishers
/// never wait on it.
async fn ws_session(app: Arc<App>, mut socket: WebSocket, cursor: u64) {
    // Subscribe before reading the store so nothing falls in between.
    let mut rx = app.hub.subscribe();
    // A cursor beyond the head (e.g. from before a daemon restart) means
    // "live only" rather than "skip everything until seq catches up".
    let mut last = cursor.min(app.hub.head());

    async fn send_all(socket: &mut WebSocket, events: Vec<StoredEvent>, last: &mut u64) -> bool {
        for ev in events {
            if ev.seq <= *last {
                continue;
            }
            if socket
                .send(Message::Text(ev.json.as_ref().into()))
                .await
                .is_err()
            {
                return false;
            }
            *last = ev.seq;
        }
        true
    }

    if !send_all(&mut socket, app.hub.since(last), &mut last).await {
        return;
    }
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(ev) => {
                    if !send_all(&mut socket, vec![ev], &mut last).await {
                        return;
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    if !send_all(&mut socket, app.hub.since(last), &mut last).await {
                        return;
                    }
                }
                Err(RecvError::Closed) => return,
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                Some(Ok(_)) => {} // clients have nothing to say; pings are answered by axum
            },
        }
    }
}
