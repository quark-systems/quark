//! HTTP endpoints for the MCP and hook transports.
//!
//! quarkd nests [`router`] under its worker prefix and puts the resulting
//! URLs in each worker's MCP config and hook commands:
//!
//! - `POST /mcp/{task}/{generation}`: MCP streamable HTTP. Each POST is one
//!   JSON-RPC message; requests get a JSON response, notifications `202`.
//!   `GET` (a server-sent stream) is `405`, which the spec allows.
//! - `POST /hooks/{task}/{generation}/{event}`: one hook call; the body is
//!   the harness's hook payload (may be empty). An `Idempotency-Key` header
//!   holding a UUID makes a retried POST record once. `200` once recorded,
//!   `409` for a replaced worker, `404` for an unknown task, `400` for a bad
//!   hook or body, `503` when the log failed (retry).

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use quark_core::{CoreError, EventId};
use serde_json::{json, Value};

use crate::mcp::{codes, error, McpServer};
use crate::recorder::{Recorder, WorkerIdentity};

/// The worker transport routes over `recorder`.
pub fn router(recorder: Arc<Recorder>) -> Router {
    Router::new()
        .route("/mcp/{task}/{generation}", post(mcp))
        .route("/hooks/{task}/{generation}/{event}", post(hook))
        .with_state(recorder)
}

async fn mcp(
    State(recorder): State<Arc<Recorder>>,
    Path((task, generation)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    let message: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(error(Value::Null, codes::PARSE_ERROR, &e.to_string())),
            )
                .into_response()
        }
    };
    let who = WorkerIdentity::new(task, generation);
    match McpServer::new(recorder).handle(&who, message).await {
        Some(response) => Json(response).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

async fn hook(
    State(recorder): State<Arc<Recorder>>,
    Path((task, generation, event)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let body: Value = if body.iter().all(u8::is_ascii_whitespace) {
        Value::Null
    } else {
        match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => return reply(CoreError::Invalid(format!("hook body: {e}"))),
        }
    };
    let id = match headers.get("idempotency-key") {
        None => None,
        Some(v) => match v.to_str().ok().and_then(|s| s.parse().ok()) {
            Some(uuid) => Some(EventId(uuid)),
            None => return reply(CoreError::Invalid("Idempotency-Key must be a UUID".into())),
        },
    };
    let who = WorkerIdentity::new(task, generation);
    match crate::hook::ingest(&recorder, &who, &event, &body, id).await {
        Ok(id) => Json(json!({ "id": id })).into_response(),
        Err(e) => reply(e),
    }
}

fn reply(e: CoreError) -> Response {
    let status = match e {
        CoreError::Invalid(_) | CoreError::IllegalTransition(_) => StatusCode::BAD_REQUEST,
        CoreError::NotFound(_) => StatusCode::NOT_FOUND,
        CoreError::Refused(_) => StatusCode::CONFLICT,
        CoreError::Unsupported(_) => StatusCode::NOT_IMPLEMENTED,
        CoreError::Backend(_) => StatusCode::SERVICE_UNAVAILABLE,
    };
    (status, Json(json!({ "error": e.to_string() }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::tests::setup;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
        let res = app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, body)
    }

    fn post(uri: &str, body: &str) -> Request<Body> {
        Request::post(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn mcp_over_http() {
        let (rec, log, _) = setup();
        let app = router(rec);
        let (s, b) = send(
            &app,
            post(
                "/mcp/t1/g2",
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"report","arguments":{"state":"working"}}}"#,
            ),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(b["result"]["isError"], false);
        assert_eq!(log.events().len(), 1);

        let (s, _) = send(
            &app,
            post(
                "/mcp/t1/g2",
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            ),
        )
        .await;
        assert_eq!(s, StatusCode::ACCEPTED);

        let (s, b) = send(&app, post("/mcp/t1/g2", "{nope")).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(b["error"]["code"], codes::PARSE_ERROR);

        let get = Request::get("/mcp/t1/g2").body(Body::empty()).unwrap();
        assert_eq!(send(&app, get).await.0, StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn hooks_over_http() {
        let (rec, log, _) = setup();
        let app = router(rec);
        assert_eq!(
            send(
                &app,
                post("/hooks/t1/g2/Stop", r#"{"hook_event_name":"Stop"}"#)
            )
            .await
            .0,
            StatusCode::OK
        );
        assert_eq!(
            send(&app, post("/hooks/t1/g2/busy", "")).await.0,
            StatusCode::OK
        );
        assert_eq!(
            send(&app, post("/hooks/t1/g1/Stop", "")).await.0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            send(&app, post("/hooks/nope/g2/Stop", "")).await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            send(&app, post("/hooks/t1/g2/Unknown", "")).await.0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(log.events().len(), 3);

        let key = EventId::new().to_string();
        for _ in 0..2 {
            let req = Request::post("/hooks/t1/g2/Stop")
                .header("idempotency-key", &key)
                .body(Body::empty())
                .unwrap();
            assert_eq!(send(&app, req).await.0, StatusCode::OK);
        }
        assert_eq!(log.events().len(), 4);
    }
}
