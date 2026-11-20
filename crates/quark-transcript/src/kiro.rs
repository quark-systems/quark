//! Kiro CLI V3 session logs: `messages.jsonl`, one event per line, each
//! `{"id", "timestamp", "payload": {"type", ...}}`. `user` and `assistant`
//! carry `content` (a string or text blocks); an assistant event whose
//! `operationType` is `Reasoning` is the model's thinking. `tool_call` and
//! `tool_result` are tool use. `turn_start`, `turn_end` and `usage_summary`
//! are bookkeeping (the last carries the turn's credits, read by
//! [`crate::read_turns`]).
//!
//! A message typed while Kiro works steers the running turn: Kiro records
//! it wrapped in a `[LIVE STEERING …]` note that asks the model to report
//! how it handled it as `[STEERING <id>: …]`. The wrapper is read as the
//! message inside it and those reports are left out of replies, so a wake
//! or a person's message reads as it was typed, whenever it arrived.

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
        Some("user") => entry(TranscriptRole::User, unsteer(&content), ts)
            .into_iter()
            .collect(),
        Some("assistant") => {
            let role = if str_at(p, "operationType") == Some("Reasoning") {
                TranscriptRole::Thinking
            } else {
                TranscriptRole::Assistant
            };
            entry(role, &drop_steering_reports(&content), ts)
                .into_iter()
                .collect()
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

/// Opens the note Kiro wraps a steering message in.
const STEERING: &str = "[LIVE STEERING";

/// The message a steering note wraps (`<user_message id="steer-…">…
/// </user_message>`), or `text` itself when it is not one.
fn unsteer(text: &str) -> &str {
    if !text.trim_start().starts_with(STEERING) {
        return text;
    }
    let inner = text
        .find("<user_message")
        .and_then(|i| text[i..].find('>').map(|j| i + j + 1))
        .and_then(|start| {
            text[start..]
                .rfind("</user_message>")
                .map(|end| &text[start..start + end])
        });
    inner.map_or(text, str::trim)
}

/// `text` without the `[STEERING steer-…: …]` reports a steering note asks
/// for. A report runs to the first `]` that ends a line.
fn drop_steering_reports(text: &str) -> String {
    const REPORT: &str = "[STEERING steer-";
    if !text.contains(REPORT) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(REPORT) {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        let end = after.match_indices(']').map(|(j, _)| j + 1).find(|&j| {
            let line = after[j..].split(['\n', '\r']).next().unwrap_or("");
            line.trim().is_empty()
        });
        match end {
            Some(j) => rest = &after[j..],
            None => {
                rest = "";
            }
        }
    }
    out.push_str(rest);
    // Blank lines a report left behind.
    let mut joined = out.trim().to_string();
    while joined.contains("\n\n\n") {
        joined = joined.replace("\n\n\n", "\n\n");
    }
    joined
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
    fn reads_a_steering_message_as_the_message_and_drops_its_report() {
        let steered = "[LIVE STEERING - New message from user]\n\nThe user sent a new message while you are working. As the currently active agent, adjust your approach if necessary based on this guidance.\n\n<user_message id=\"steer-249c\">\nWake prj_1#41: 1 item needs your judgment.\n- [merged] t1: merged.\n</user_message>\n\nIMPORTANT: After completing your work, include a brief note about how you handled this steering message. Use this exact format:\n\n[STEERING steer-249c: <describe what you did or why it wasn't applicable>]";
        let e = parse(&json!({"payload": {"type": "user", "content": steered}}));
        assert_eq!(e[0].role, TranscriptRole::User);
        assert_eq!(
            e[0].text,
            "Wake prj_1#41: 1 item needs your judgment.\n- [merged] t1: merged."
        );

        let reply = "Started t2 now that t1 merged.\n\n[STEERING steer-249c: started the waiting task [t2] after the merge]";
        let e = parse(&json!({"payload": {"type": "assistant", "content": reply}}));
        assert_eq!(e[0].text, "Started t2 now that t1 merged.");
        let middle = "One.\n[STEERING steer-1: noted]\nTwo.";
        let e = parse(&json!({"payload": {"type": "assistant", "content": middle}}));
        assert_eq!(e[0].text, "One.\n\nTwo.");
        // A reply that is only a report leaves nothing to show.
        let only = "[STEERING steer-1: not applicable]";
        assert!(parse(&json!({"payload": {"type": "assistant", "content": only}})).is_empty());
        // Ordinary text is untouched.
        let plain = "Use [brackets] freely.";
        let e = parse(&json!({"payload": {"type": "assistant", "content": plain}}));
        assert_eq!(e[0].text, plain);
    }

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
