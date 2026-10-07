//! The MCP tool set a worker calls: `report`, `ask`, `learned`, `done`.
//!
//! [`McpServer::handle`] answers one JSON-RPC message for one worker and is
//! transport-free; [`crate::http`] serves it as MCP streamable HTTP (JSON
//! responses only, no server-sent stream, since the server never initiates).

use std::sync::Arc;

use quark_core::worker::{Transport, WorkerMessage, WorkerProtocol};
use quark_core::CoreError;
use serde_json::{json, Value};

use crate::recorder::{Recorder, WorkerIdentity};

/// Protocol versions this server speaks, newest first.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// JSON-RPC error codes.
pub mod codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
}

const INSTRUCTIONS: &str = "Tell the coordinator about your work with these tools \
instead of writing status into chat. Use `report` when your state changes \
(working, blocked, paused), `ask` for a question only a person can answer, \
`learned` for a fact worth keeping in Project memory, and `done` once the \
deliverable is ready. If a call says this worker was replaced, stop.";

/// Serves the worker tools over JSON-RPC.
#[derive(Clone)]
pub struct McpServer {
    recorder: Arc<Recorder>,
}

impl McpServer {
    pub fn new(recorder: Arc<Recorder>) -> Self {
        Self { recorder }
    }

    /// Answers one JSON-RPC message from `who`. Returns `None` for a
    /// notification, which gets no response.
    pub async fn handle(&self, who: &WorkerIdentity, message: Value) -> Option<Value> {
        let Some(obj) = message.as_object() else {
            return Some(error(
                Value::Null,
                codes::INVALID_REQUEST,
                "expected one JSON-RPC object",
            ));
        };
        let id = obj.get("id").cloned();
        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            // A response from the client; this server sends no requests.
            return id.map(|id| error(id, codes::INVALID_REQUEST, "missing method"));
        };
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        let id = id?; // Notifications (`notifications/initialized`, ...) need no answer.
        let result = match method {
            "initialize" => Ok(initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => self.call(who, &params).await,
            other => Err((codes::METHOD_NOT_FOUND, format!("unknown method {other}"))),
        };
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, msg)) => error(id, code, &msg),
        })
    }

    async fn call(&self, who: &WorkerIdentity, params: &Value) -> Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or((codes::INVALID_PARAMS, "missing tool name".to_string()))?;
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        let message = match tool_message(name, &args) {
            Ok(m) => m,
            Err(ToolError::Unknown) => {
                return Err((codes::INVALID_PARAMS, format!("unknown tool {name}")))
            }
            Err(ToolError::Arguments(msg)) => return Ok(tool_result(true, &msg)),
        };
        match self
            .recorder
            .receive(who.envelope(Transport::Mcp, message))
            .await
        {
            Ok(_) => Ok(tool_result(false, "Recorded.")),
            Err(CoreError::Backend(msg)) => Err((
                codes::INTERNAL_ERROR,
                format!("not recorded, try again: {msg}"),
            )),
            Err(e) => Ok(tool_result(true, &e.to_string())),
        }
    }
}

enum ToolError {
    Unknown,
    Arguments(String),
}

/// Builds the message a tool call carries.
fn tool_message(name: &str, args: &Value) -> Result<WorkerMessage, ToolError> {
    let text = |field: &str| -> Result<String, ToolError> {
        match args.get(field) {
            Some(Value::String(s)) if !s.trim().is_empty() => Ok(s.clone()),
            _ => Err(ToolError::Arguments(format!(
                "`{field}` is required and must be non-empty text"
            ))),
        }
    };
    let optional = |field: &str| -> Option<String> {
        args.get(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Ok(match name {
        "report" => WorkerMessage::Report {
            state: text("state")?,
            note: optional("note").unwrap_or_default(),
        },
        "ask" => WorkerMessage::Ask {
            key: optional("key").unwrap_or_else(|| "default".into()),
            question: text("question")?,
        },
        "learned" => WorkerMessage::Learned {
            fact: text("fact")?,
        },
        "done" => WorkerMessage::Done {
            summary: text("summary")?,
            pull_request: optional("pull_request"),
        },
        _ => return Err(ToolError::Unknown),
    })
}

fn initialize(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = asked
        .filter(|v| PROTOCOL_VERSIONS.contains(v))
        .unwrap_or(PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "quark", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}

/// The tool definitions `tools/list` returns.
pub fn tools() -> Value {
    json!([
        {
            "name": "report",
            "description": "Report a change in your state. Use `working` when you start or resume, `blocked` when the coordinator must act before you can continue, `paused` for an external wait that will clear on its own (CI, a rate limit). Do not report routine progress.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "state": { "type": "string", "description": "working, blocked, paused, or another one-word state" },
                    "note": { "type": "string", "description": "One line on why, for the coordinator" }
                },
                "required": ["state"]
            }
        },
        {
            "name": "ask",
            "description": "Ask a question only a person can answer, such as a design choice. Keep working on anything the answer does not affect. Asking again with the same key replaces the question.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": { "type": "string", "description": "Short slug naming the decision, such as api-shape; defaults to `default`" },
                    "question": { "type": "string", "description": "The question with the options and your recommendation" }
                },
                "required": ["question"]
            }
        },
        {
            "name": "learned",
            "description": "Record a durable fact about the Project worth keeping for future workers, such as a build quirk. Not for task progress.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "fact": { "type": "string", "description": "The fact, in one or two sentences" }
                },
                "required": ["fact"]
            }
        },
        {
            "name": "done",
            "description": "Say the deliverable is ready: a pull request, a ready branch or a written report.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "summary": { "type": "string", "description": "What changed and what a reviewer should know" },
                    "pull_request": { "type": "string", "description": "The pull request URL, if there is one" }
                },
                "required": ["summary"]
            }
        }
    ])
}

fn tool_result(is_error: bool, text: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}

/// A JSON-RPC error response.
pub fn error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::tests::setup;
    use quark_core::worker::WorkerEnvelope;

    fn req(method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": params })
    }

    #[tokio::test]
    async fn initialize_negotiates_a_version() {
        let (rec, _, _) = setup();
        let mcp = McpServer::new(rec);
        let who = WorkerIdentity::new("t1", "g2");
        let r = mcp
            .handle(
                &who,
                req("initialize", json!({ "protocolVersion": "2025-03-26" })),
            )
            .await
            .unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(r["result"]["serverInfo"]["name"], "quark");
        let r = mcp
            .handle(
                &who,
                req("initialize", json!({ "protocolVersion": "1999-01-01" })),
            )
            .await
            .unwrap();
        assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
    }

    #[tokio::test]
    async fn notifications_get_no_answer() {
        let (rec, _, _) = setup();
        let mcp = McpServer::new(rec);
        let who = WorkerIdentity::new("t1", "g2");
        let n = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(mcp.handle(&who, n).await.is_none());
    }

    #[tokio::test]
    async fn lists_the_four_tools() {
        let (rec, _, _) = setup();
        let r = McpServer::new(rec)
            .handle(
                &WorkerIdentity::new("t1", "g2"),
                req("tools/list", json!({})),
            )
            .await
            .unwrap();
        let names: Vec<_> = r["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["report", "ask", "learned", "done"]);
    }

    #[tokio::test]
    async fn tool_calls_land_in_the_log() {
        let (rec, log, _) = setup();
        let mcp = McpServer::new(rec);
        let who = WorkerIdentity::new("t1", "g2");
        let calls = [
            json!({ "name": "report", "arguments": { "state": "blocked", "note": "need a token" } }),
            json!({ "name": "ask", "arguments": { "question": "REST or gRPC?" } }),
            json!({ "name": "learned", "arguments": { "fact": "run tests with --test-threads=1" } }),
            json!({ "name": "done", "arguments": { "summary": "added X", "pull_request": "https://github.com/o/r/pull/1" } }),
        ];
        for call in calls {
            let r = mcp.handle(&who, req("tools/call", call)).await.unwrap();
            assert_eq!(r["result"]["isError"], false, "{r}");
        }
        let messages: Vec<_> = log
            .events()
            .iter()
            .map(|e| e.decode::<WorkerEnvelope>().unwrap())
            .inspect(|e| assert_eq!(e.via, Transport::Mcp))
            .map(|e| e.message)
            .collect();
        assert_eq!(
            messages,
            [
                WorkerMessage::Report {
                    state: "blocked".into(),
                    note: "need a token".into()
                },
                WorkerMessage::Ask {
                    key: "default".into(),
                    question: "REST or gRPC?".into()
                },
                WorkerMessage::Learned {
                    fact: "run tests with --test-threads=1".into()
                },
                WorkerMessage::Done {
                    summary: "added X".into(),
                    pull_request: Some("https://github.com/o/r/pull/1".into())
                },
            ]
        );
    }

    #[tokio::test]
    async fn bad_arguments_and_old_workers_get_tool_errors() {
        let (rec, log, _) = setup();
        let mcp = McpServer::new(rec);
        let r = mcp
            .handle(
                &WorkerIdentity::new("t1", "g2"),
                req("tools/call", json!({ "name": "done", "arguments": {} })),
            )
            .await
            .unwrap();
        assert_eq!(r["result"]["isError"], true);
        assert!(log.events().is_empty());

        let r = mcp
            .handle(
                &WorkerIdentity::new("t1", "g1"),
                req(
                    "tools/call",
                    json!({ "name": "learned", "arguments": { "fact": "x" } }),
                ),
            )
            .await
            .unwrap();
        assert_eq!(r["result"]["isError"], true);
        assert!(r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("replaced"));
        assert_eq!(log.events().len(), 1);
    }

    #[tokio::test]
    async fn unknown_tools_and_methods_are_protocol_errors() {
        let (rec, _, _) = setup();
        let mcp = McpServer::new(rec);
        let who = WorkerIdentity::new("t1", "g2");
        let r = mcp
            .handle(&who, req("tools/call", json!({ "name": "deploy" })))
            .await
            .unwrap();
        assert_eq!(r["error"]["code"], codes::INVALID_PARAMS);
        let r = mcp
            .handle(&who, req("resources/list", json!({})))
            .await
            .unwrap();
        assert_eq!(r["error"]["code"], codes::METHOD_NOT_FOUND);
        let r = mcp.handle(&who, json!([1, 2])).await.unwrap();
        assert_eq!(r["error"]["code"], codes::INVALID_REQUEST);
    }
}
