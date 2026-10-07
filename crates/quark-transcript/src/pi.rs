//! Pi coding agent session logs: a `session` header line, then entries with a
//! top-level `type`. `message` entries carry a pi-ai message whose `role` is
//! `user`, `assistant`, `toolResult`, or one of Pi's own roles; `bashExecution`
//! is a command the user ran with `!`. Model, thinking-level, compaction,
//! label and custom entries are bookkeeping.

use serde_json::Value;

use crate::{entry, str_at, text_of, tool_call, tool_entry, TranscriptEntry, TranscriptRole};

pub(crate) fn parse(v: &Value) -> Vec<TranscriptEntry> {
    if str_at(v, "type") != Some("message") {
        return Vec::new();
    }
    let ts = str_at(v, "timestamp");
    let Some(m) = v.get("message") else {
        return Vec::new();
    };
    let content = m.get("content").unwrap_or(&Value::Null);
    let mut out = Vec::new();
    match str_at(m, "role") {
        Some("user") => out.extend(entry(TranscriptRole::User, &text_of(content), ts)),
        Some("assistant") => {
            for b in content.as_array().into_iter().flatten() {
                match str_at(b, "type") {
                    Some("text") => out.extend(entry(
                        TranscriptRole::Assistant,
                        str_at(b, "text").unwrap_or(""),
                        ts,
                    )),
                    Some("thinking")
                        if b.get("redacted").and_then(Value::as_bool) != Some(true) =>
                    {
                        out.extend(entry(
                            TranscriptRole::Thinking,
                            str_at(b, "thinking").unwrap_or(""),
                            ts,
                        ))
                    }
                    Some("toolCall") => out.push(tool_call(
                        b.get("arguments"),
                        str_at(b, "name"),
                        str_at(b, "id"),
                        ts,
                        None,
                    )),
                    _ => {}
                }
            }
        }
        Some("toolResult") => out.push(tool_entry(
            TranscriptRole::ToolResult,
            &text_of(content),
            str_at(m, "toolName"),
            str_at(m, "toolCallId"),
            m.get("isError").and_then(Value::as_bool).unwrap_or(false),
            ts,
        )),
        Some("bashExecution") => {
            let command = str_at(m, "command").unwrap_or("");
            let output = str_at(m, "output").unwrap_or("");
            let failed = m
                .get("exitCode")
                .and_then(Value::as_i64)
                .is_some_and(|c| c != 0);
            out.push(tool_call(
                Some(&Value::String(command.to_string())),
                Some("bash"),
                None,
                ts,
                None,
            ));
            out.push(tool_entry(
                TranscriptRole::ToolResult,
                output,
                Some("bash"),
                None,
                failed,
                ts,
            ));
        }
        _ => {}
    }
    out
}
