//! Kiro CLI V3 session logs: `messages.jsonl`, one event per line, each
//! `{"id", "timestamp", "payload": {"type", ...}}`. `user` and `assistant`
//! carry `content` (a string or text blocks); an assistant event whose
//! `operationType` is `Reasoning` is the model's thinking. `tool_call` and
//! `tool_result` are tool use. `turn_start`, `turn_end` and `usage_summary`
//! are bookkeeping (the last carries the turn's credits, read by
//! [`crate::read_turns`]).

use serde_json::Value;

use crate::{entry, str_at, tool_call, tool_entry, TranscriptEntry, TranscriptRole};

pub(crate) fn parse(v: &Value) -> Vec<TranscriptEntry> {
    let Some(p) = v.get("payload") else {
        return Vec::new();
    };
    let ts = str_at(v, "timestamp");
    let content = text(p.get("content").unwrap_or(&Value::Null));
    let id = || {
        str_at(p, "toolUseId")
            .or_else(|| str_at(p, "toolCallId"))
            .or_else(|| str_at(p, "callId"))
    };
    match str_at(p, "type") {
        Some("user") => entry(TranscriptRole::User, &content, ts)
            .into_iter()
            .collect(),
        Some("assistant") => {
            let role = if str_at(p, "operationType") == Some("Reasoning") {
                TranscriptRole::Thinking
            } else {
                TranscriptRole::Assistant
            };
            entry(role, &content, ts).into_iter().collect()
        }
        Some("tool_call") => vec![tool_call(
            p.get("input")
                .or_else(|| p.get("arguments"))
                .or_else(|| p.get("args")),
            str_at(p, "toolName").or_else(|| str_at(p, "name")),
            id(),
            ts,
            None,
        )],
        Some("tool_result") => vec![tool_entry(
            TranscriptRole::ToolResult,
            &content,
            str_at(p, "toolName").or_else(|| str_at(p, "name")),
            id(),
            p.get("isError").and_then(Value::as_bool).unwrap_or(false)
                || matches!(str_at(p, "status"), Some("error" | "failed")),
            ts,
        )],
        _ => Vec::new(),
    }
}

/// Text of `content`: a string, or blocks of `{"kind": "text", "data"}` or
/// `{"type": "text", "text"}`.
fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|b| match b {
                Value::String(s) => Some(s.as_str()),
                _ if str_at(b, "kind") == Some("text") => str_at(b, "data"),
                _ if str_at(b, "type") == Some("text") => str_at(b, "text"),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The model a session ran, from the `session.json` beside its log; Kiro's
/// automatic choice reads as `kiro-auto`.
pub(crate) fn session_model(log: &std::path::Path) -> Option<String> {
    let meta = std::fs::read(log.with_file_name("session.json")).ok()?;
    let meta: Value = serde_json::from_slice(&meta).ok()?;
    let model = str_at(&meta, "modelId")?.trim();
    Some(match model {
        "" | "auto" => "kiro-auto".to_string(),
        m => m.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn reads_prompts_replies_thinking_and_tools() {
        let lines = [
            json!({"id": "u1", "timestamp": "2026-10-21T10:00:00Z",
                "payload": {"type": "user", "content": "fix the test"}}),
            json!({"id": "a1", "payload": {"type": "assistant", "operationType": "Reasoning",
                "content": [{"kind": "text", "data": "thinking"}]}}),
            json!({"id": "a2", "payload": {"type": "assistant", "content": "On it."}}),
            json!({"id": "t1", "payload": {"type": "tool_call", "toolName": "shell",
                "toolUseId": "x", "input": {"command": "cargo test"}}}),
            json!({"id": "t2", "payload": {"type": "tool_result", "toolUseId": "x",
                "content": "ok", "status": "success"}}),
            json!({"id": "s", "payload": {"type": "turn_start", "executionId": "e1"}}),
        ];
        let e: Vec<_> = lines.iter().flat_map(parse).collect();
        let roles: Vec<_> = e.iter().map(|e| e.role).collect();
        assert_eq!(
            roles,
            [
                TranscriptRole::User,
                TranscriptRole::Thinking,
                TranscriptRole::Assistant,
                TranscriptRole::ToolCall,
                TranscriptRole::ToolResult,
            ]
        );
        assert_eq!(e[0].text, "fix the test");
        assert_eq!(e[0].ts.as_deref(), Some("2026-10-21T10:00:00Z"));
        assert_eq!(e[3].tool_name.as_deref(), Some("shell"));
        assert_eq!(e[4].tool_call_id.as_deref(), Some("x"));
        assert!(!e[4].is_error);
    }
}
