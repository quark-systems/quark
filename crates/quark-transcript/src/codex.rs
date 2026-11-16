//! Codex CLI rollouts: one `{"timestamp", "type", "payload"}` envelope per
//! line. `response_item` payloads are model-facing items (messages, tool calls
//! and their outputs); `event_msg` payloads are UI events. The user's own
//! words come from an `event_msg`, because user-role `response_item` messages
//! also carry injected context (environment, AGENTS.md): `user_message` until
//! Codex 0.16x, then an `item_completed` whose item is a `UserMessage`. Assistant text comes from `response_item` messages, since the
//! matching `agent_message` event is a duplicate. Rollouts written before the
//! envelope existed are not supported. A `compacted` line, where Codex
//! replaced the conversation with a summary, is one
//! [`TranscriptRole::Compaction`] entry. A `task_complete` or `turn_aborted`
//! event, Codex ending its turn, is a [`turn_end`] entry: a turn whose last
//! step was a tool call, with an empty final message, would otherwise read as
//! still running.

use serde_json::Value;

use crate::{
    entry, str_at, text_of, tool_call, tool_entry, turn_end, TranscriptEntry, TranscriptRole,
};

/// The user's words when `v` is a line recording a message the user sent.
pub(crate) fn user_message(v: &Value) -> Option<String> {
    if str_at(v, "type") != Some("event_msg") {
        return None;
    }
    let p = v.get("payload")?;
    match str_at(p, "type")? {
        "user_message" => Some(str_at(p, "message").unwrap_or("").to_string()),
        "item_completed" => {
            let item = p.get("item")?;
            (str_at(item, "type") == Some("UserMessage"))
                .then(|| text_of(item.get("content").unwrap_or(&Value::Null)))
        }
        _ => None,
    }
}

pub(crate) fn parse(v: &Value) -> Vec<TranscriptEntry> {
    let ts = str_at(v, "timestamp");
    if let Some(text) = user_message(v) {
        return entry(TranscriptRole::User, &text, ts).into_iter().collect();
    }
    let Some(p) = v.get("payload") else {
        return Vec::new();
    };
    match (str_at(v, "type"), str_at(p, "type")) {
        (Some("response_item"), Some(item)) => response_item(item, p, ts),
        (Some("event_msg"), Some("task_complete" | "turn_aborted")) => vec![turn_end(ts)],
        (Some("compacted"), _) => entry(
            TranscriptRole::Compaction,
            "Codex compacted the conversation",
            ts,
        )
        .into_iter()
        .collect(),
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
