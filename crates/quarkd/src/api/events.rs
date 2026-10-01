//! `GET /v1/events?cursor=<seq>`: the typed event stream over WebSocket.
//!
//! On connect the server subscribes to live events first, then replays every
//! stored event with `seq > cursor`, then forwards live events, skipping any it
//! already replayed. A subscriber that falls behind the live channel catches
//! up from the store, so a client never misses or repeats a `seq`.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use quark_systems::Event;
use serde::Deserialize;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use utoipa::IntoParams;

use super::{db, ApiError, AppState};

const REPLAY_PAGE: u32 = 500;

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EventsQuery {
    /// Last `seq` the client has seen. Every later event is replayed before
    /// live events. Omit to receive only events after connecting; pass 0 to
    /// replay the whole log.
    pub cursor: Option<i64>,
}

/// Typed event stream (WebSocket).
///
/// Each text frame is one JSON `Event`. Upgrade with a standard WebSocket
/// handshake; the server ignores client frames other than close.
#[utoipa::path(
    get,
    path = "/v1/events",
    tag = "events",
    params(EventsQuery),
    responses(
        (status = 101, description = "Switching to WebSocket; each frame is an Event",
            body = quark_systems::Event)
    )
)]
pub async fn stream(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(q): Query<EventsQuery>,
) -> Result<Response, ApiError> {
    // Subscribe and fix the starting point before completing the handshake,
    // so a client that has connected cannot miss an event committed after.
    let live = state.store.subscribe();
    let last = match q.cursor {
        Some(c) => c.max(0),
        None => db(&state, |s| s.last_seq()).await?,
    };
    Ok(ws.on_upgrade(move |socket| async move {
        if let Err(e) = run(socket, state, live, last).await {
            tracing::debug!(error = %e, "event stream closed");
        }
    }))
}

async fn run(
    mut socket: WebSocket,
    state: AppState,
    mut live: broadcast::Receiver<Event>,
    mut last: i64,
) -> anyhow::Result<()> {
    replay(&mut socket, &state, &mut last).await?;

    loop {
        tokio::select! {
            msg = live.recv() => match msg {
                Ok(event) if event.seq <= last => {}
                Ok(event) if event.seq == last + 1 => {
                    send(&mut socket, &event).await?;
                    last = event.seq;
                }
                // A gap or a lagged receiver: fill from the store.
                Ok(_) | Err(RecvError::Lagged(_)) => replay(&mut socket, &state, &mut last).await?,
                Err(RecvError::Closed) => return Ok(()),
            },
            incoming = socket.recv() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return Ok(()),
                Some(Ok(_)) => {}
            },
        }
    }
}

async fn replay(socket: &mut WebSocket, state: &AppState, last: &mut i64) -> anyhow::Result<()> {
    loop {
        let after = *last;
        let page = db(state, move |s| s.events_after(after, REPLAY_PAGE))
            .await
            .map_err(api_err)?;
        let done = page.len() < REPLAY_PAGE as usize;
        for event in &page {
            send(socket, event).await?;
            *last = event.seq;
        }
        if done {
            return Ok(());
        }
    }
}

async fn send(socket: &mut WebSocket, event: &Event) -> anyhow::Result<()> {
    let text = serde_json::to_string(event)?;
    socket.send(Message::Text(text.into())).await?;
    Ok(())
}

fn api_err(e: ApiError) -> anyhow::Error {
    anyhow::anyhow!("{e:?}")
}
