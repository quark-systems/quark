//! Codex CLI rollouts: one `{"timestamp", "type", "payload"}` envelope per
//! line. `response_item` payloads are model-facing items (messages, tool calls
//! and their outputs); `event_msg` payloads are UI events. The user's own
//! words come from `event_msg` `user_message`, because user-role
//! `response_item` messages also carry injected context (environment,
//! AGENTS.md). Assistant text comes from `response_item` messages, since the
//! matching `agent_message` event is a duplicate. Rollouts written before the
//! envelope existed are not supported.

use serde_json::Value;

use crate::{entry, str_at, text_of, tool_call, tool_entry, TranscriptEntry, TranscriptRole};

pub(crate) fn parse(v: &Value) -> Vec<TranscriptEntry> {
    let ts = str_at(v, "timestamp");
    let Some(p) = v.get("payload") else {
        return Vec::new();
    };
    match (str_at(v, "type"), str_at(p, "type")) {
        (Some("event_msg"), Some("user_message")) => {
            entry(TranscriptRole::User, str_at(p, "message").unwrap_or(""), ts)
                .into_iter()
                .collect()
        }
        (Some("response_item"), Some(item)) => response_item(item, p, ts),
        _ => Vec::new(),
    }
}

fn response_item(item: &str, p: &Value, ts: Option<&str>) -> Vec<TranscriptEntry> {
    let call_id = str_at(p, "call_id");
    let one = match item {
        "message" if str_at(p, "role") == Some("assistant") => entry(
            TranscriptRole::Assistant,
            &text_of(p.get("content").unwrap_or(&Value::Null)),
            ts,
        ),
        "reasoning" => {
            let summary = p
                .get("summary")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|s| str_at(s, "text"))
                        .collect::<Vec<_>>()
                        .join("\n\n")
                })
                .unwrap_or_default();
            entry(TranscriptRole::Thinking, &summary, ts)
        }
        "function_call" => Some(tool_call(
            p.get("arguments"),
            str_at(p, "name"),
            call_id,
            ts,
            None,
        )),
        "custom_tool_call" => Some(tool_call(
            p.get("input"),
            str_at(p, "name"),
            call_id,
            ts,
            None,
        )),
        "local_shell_call" => {
            let command = p
                .get("action")
                .and_then(|a| a.get("command"))
                .and_then(Value::as_array)
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            Some(tool_call(
                Some(&Value::String(command)),
                Some("shell"),
                call_id,
                ts,
                None,
            ))
        }
        "function_call_output" | "custom_tool_call_output" => Some(tool_entry(
            TranscriptRole::ToolResult,
            &text_of(p.get("output").unwrap_or(&Value::Null)),
            str_at(p, "name"),
            call_id,
            false,
            ts,
        )),
        _ => None,
    };
    one.into_iter().collect()
}
