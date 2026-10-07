//! Claude Code session logs: one JSON object per line with a top-level
//! `type`. `user` and `assistant` lines carry an Anthropic Messages API
//! `message`; everything else (attachments, summaries, snapshots, queue
//! operations) is bookkeeping. Sidechain lines belong to subagents and meta
//! lines are harness-injected context, so both are skipped.

use serde_json::Value;

use crate::{entry, str_at, text_of, tool_call, tool_entry, TranscriptEntry, TranscriptRole};

pub(crate) fn parse(v: &Value) -> Vec<TranscriptEntry> {
    let kind = str_at(v, "type");
    if !matches!(kind, Some("user" | "assistant")) {
        return Vec::new();
    }
    let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    if flag("isSidechain") || flag("isMeta") {
        return Vec::new();
    }
    let ts = str_at(v, "timestamp");
    let Some(content) = v.get("message").and_then(|m| m.get("content")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match content {
        Value::String(s) => {
            let role = if kind == Some("user") {
                TranscriptRole::User
            } else {
                TranscriptRole::Assistant
            };
            out.extend(entry(role, s, ts));
        }
        Value::Array(blocks) => {
            for b in blocks {
                match str_at(b, "type") {
                    Some("text") => {
                        let role = if kind == Some("user") {
                            TranscriptRole::User
                        } else {
                            TranscriptRole::Assistant
                        };
                        out.extend(entry(role, str_at(b, "text").unwrap_or(""), ts));
                    }
                    Some("thinking") => out.extend(entry(
                        TranscriptRole::Thinking,
                        str_at(b, "thinking").unwrap_or(""),
                        ts,
                    )),
                    Some("tool_use") => out.push(tool_call(
                        b.get("input"),
                        str_at(b, "name"),
                        str_at(b, "id"),
                        ts,
                        str_at(v, "cwd"),
                    )),
                    Some("tool_result") => out.push(tool_entry(
                        TranscriptRole::ToolResult,
                        &text_of(b.get("content").unwrap_or(&Value::Null)),
                        None,
                        str_at(b, "tool_use_id"),
                        b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                        ts,
                    )),
                    _ => {}
                }
            }
        }
        _ => {}
    }
    out
}
