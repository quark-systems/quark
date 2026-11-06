//! IBM Bob's conversation, which it keeps in its database
//! (`~/.bob/db/bob.db`, table `messages`) rather than a log file: one row per
//! message, `data` a JSON object `{"role", "content", ...}`. `user` rows are
//! prompts; `assistant` rows carry reply text in `content` and tool use in
//! `toolCalls` (`{id, name, arguments}`); `tool` rows are results, with the
//! call they answer in `toolUsage.signature` (`id`, `name`, `isError`).
//! `system` rows (the system prompt) and `_meta.notAi` rows (the summary
//! Bob writes when it compacts) are not conversation.

use serde_json::Value;

use crate::{entry, str_at, tool_call, tool_entry, TranscriptEntry, TranscriptRole};

/// Entries in one Bob message row's `data`. `ts` is the row's time, `cwd`
/// the task's workspace (shortens paths in tool summaries).
pub fn parse_bob_message(data: &str, ts: Option<&str>, cwd: Option<&str>) -> Vec<TranscriptEntry> {
    let Ok(v) = serde_json::from_str::<Value>(data) else {
        return Vec::new();
    };
    if v.pointer("/_meta/notAi").and_then(Value::as_bool) == Some(true) {
        return Vec::new();
    }
    let content = crate::text_of(v.get("content").unwrap_or(&Value::Null));
    match str_at(&v, "role") {
        Some("user") => entry(TranscriptRole::User, &content, ts)
            .into_iter()
            .collect(),
        Some("assistant") => {
            let mut out: Vec<_> = entry(TranscriptRole::Assistant, &content, ts)
                .into_iter()
                .collect();
            for call in v
                .get("toolCalls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                out.push(tool_call(
                    call.get("arguments"),
                    str_at(call, "name"),
                    str_at(call, "id"),
                    ts,
                    cwd,
                ));
            }
            out
        }
        Some("tool") => {
            let sig = v.pointer("/toolUsage/signature").unwrap_or(&Value::Null);
            vec![tool_entry(
                TranscriptRole::ToolResult,
                &content,
                str_at(sig, "name"),
                str_at(sig, "id"),
                sig.get("isError").and_then(Value::as_bool).unwrap_or(false),
                ts,
            )]
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use quark_systems::ToolKind;
    use serde_json::json;

    use super::*;

    #[test]
    fn reads_prompts_replies_tools_and_skips_bookkeeping() {
        let rows = [
            json!({"role": "user", "content": "fix the test", "envContext": "<environment_details>"}),
            json!({"role": "system", "content": "You are Bob"}),
            json!({"role": "assistant", "content": "On it.", "toolCalls": [
                {"id": "t1", "name": "execute_command", "arguments": {"command": "cargo test"}},
                {"id": "t2", "name": "apply_diff", "arguments": {"path": "/w/src/a.rs",
                    "diff": "<<<<<<< SEARCH\n:start_line:3\n-------\nlet a = 1;\n=======\nlet a = 2;\n>>>>>>> REPLACE"}}
            ], "_meta": {"timestamp": 1, "spend": {"cost": 0.1}}}),
            json!({"role": "tool", "content": "ok", "toolUsage": {"signature":
                {"id": "t1", "name": "execute_command", "isError": false}}}),
            json!({"role": "tool", "content": "no such file", "toolUsage": {"signature":
                {"id": "t2", "name": "apply_diff", "isError": true}}}),
            json!({"role": "assistant", "content": "Previous conversation summary:\n",
                "_meta": {"notAi": true, "summary": true}}),
            json!({"role": "assistant", "content": "", "stop": true}),
        ];
        let e: Vec<_> = rows
            .iter()
            .flat_map(|r| {
                parse_bob_message(&r.to_string(), Some("2026-11-06T18:00:00Z"), Some("/w"))
            })
            .collect();
        let roles: Vec<_> = e.iter().map(|e| e.role).collect();
        assert_eq!(
            roles,
            [
                TranscriptRole::User,
                TranscriptRole::Assistant,
                TranscriptRole::ToolCall,
                TranscriptRole::ToolCall,
                TranscriptRole::ToolResult,
                TranscriptRole::ToolResult,
            ]
        );
        assert_eq!(e[0].text, "fix the test");
        assert_eq!(e[0].ts.as_deref(), Some("2026-11-06T18:00:00Z"));
        assert_eq!(e[2].tool.as_ref().unwrap().kind, ToolKind::Shell);
        let edit = e[3].tool.as_ref().unwrap();
        assert_eq!(edit.kind, ToolKind::Edit);
        assert_eq!(edit.path.as_deref(), Some("src/a.rs"));
        assert_eq!(edit.diff.len(), 2, "{:?}", edit.diff);
        assert_eq!(e[4].tool_call_id.as_deref(), Some("t1"));
        assert!(!e[4].is_error);
        assert!(e[5].is_error);
    }

    #[test]
    fn unreadable_rows_yield_nothing() {
        assert!(parse_bob_message("not json", None, None).is_empty());
    }
}
