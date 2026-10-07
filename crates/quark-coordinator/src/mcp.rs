//! The coordinator's tools over MCP.
//!
//! [`CoordinatorMcp::handle`] answers one JSON-RPC message for one Project's
//! coordinator and is transport-free; quarkd serves it as MCP streamable
//! HTTP the same way it serves the worker tools (`quark_worker::http`).

use std::sync::Arc;

use quark_core::ProjectId;
use serde_json::{json, Value};

use crate::tools::{catalog, ToolCall};
use crate::Coordinator;

/// Protocol versions this server speaks, newest first.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// JSON-RPC error codes.
pub mod codes {
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
}

const INSTRUCTIONS: &str = "These are your only way to act. The engine owns the \
lifecycle (spawning, watching, recovering, gates, rebase, merge, cleanup); you \
decide. When a wake's items need nothing from you, end your turn without \
calling anything.";

/// Serves one coordinator's tools.
#[derive(Clone)]
pub struct CoordinatorMcp {
    coordinator: Arc<Coordinator>,
}

impl CoordinatorMcp {
    pub fn new(coordinator: Arc<Coordinator>) -> Self {
        Self { coordinator }
    }

    /// Answers one JSON-RPC message from `project`'s coordinator. `None`
    /// for a notification.
    pub async fn handle(&self, project: &ProjectId, message: Value) -> Option<Value> {
        let Some(obj) = message.as_object() else {
            return Some(error(
                Value::Null,
                codes::INVALID_REQUEST,
                "expected one JSON-RPC object",
            ));
        };
        let id = obj.get("id").cloned();
        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            return id.map(|id| error(id, codes::INVALID_REQUEST, "missing method"));
        };
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        let id = id?;
        let result = match method {
            "initialize" => Ok(initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": catalog() })),
            "tools/call" => self.call(project, &params).await,
            other => Err((codes::METHOD_NOT_FOUND, format!("unknown method {other}"))),
        };
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, msg)) => error(id, code, &msg),
        })
    }

    async fn call(&self, project: &ProjectId, params: &Value) -> Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or((codes::INVALID_PARAMS, "missing tool name".to_string()))?;
        let args = params.get("arguments").cloned().unwrap_or(Value::Null);
        let call = match ToolCall::parse(name, &args) {
            Ok(c) => c,
            Err(msg) if msg.starts_with("unknown tool") => {
                return Err((codes::INVALID_PARAMS, msg))
            }
            Err(msg) => return Ok(tool_result(true, &msg)),
        };
        // A failed call is the coordinator's to judge, so it is a tool
        // result, not a protocol error a client might blindly retry.
        Ok(match self.coordinator.call(project, call).await {
            Ok(text) => tool_result(false, &text),
            Err(e) => tool_result(true, &e.to_string()),
        })
    }
}

fn initialize(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = asked
        .filter(|v| PROTOCOL_VERSIONS.contains(v))
        .unwrap_or(PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "quark-coordinator", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}

fn tool_result(is_error: bool, text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}
