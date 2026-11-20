//! Antigravity CLI (`agy`) transcripts:
//! `<app data>/brain/<conversation id>/.system_generated/logs/transcript_full.jsonl`,
//! one step per line, each `{"step_index", "source", "type", "status",
//! "created_at", "content", ...}`.
//!
//! - `USER_INPUT` is a prompt, its text wrapped in `<USER_REQUEST>` beside
//!   metadata agy adds (the local time, a setting the person changed); the
//!   entry is the request alone.
//! - `PLANNER_RESPONSE` is a model call: `content` is what it said,
//!   `thinking` its reasoning and `tool_calls` (`{"name", "args"}`) the tools
//!   it called. It carries the call's `model`, `input_tokens` (not counting
//!   the cache), `cache_read_tokens` and `output_tokens`.
//! - `GENERIC` steps from the model are tool output (a command's exit code
//!   and output).
//! - `CHECKPOINT` from the system is a compaction (`/compact`, or agy on its
//!   own): its content is the summary that replaces the conversation before
//!   it. It is one [`TranscriptRole::Compaction`] entry, and the `/compact`
//!   prompt that asked for it is left out.
//! - `SYSTEM_MESSAGE` steps are notes agy or a hook added (a hook's injected
//!   context, a `Stop` hook's reason to go on): not conversation.
//!
//! agy keeps the newest conversation of each folder in
//! `<app data>/cache/last_conversations.json` (folder path to conversation
//! id), which is how [`crate::locate`] finds a folder's transcript.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::{entry, str_at, tool_call, tool_entry, TranscriptEntry, TranscriptRole};

pub(crate) fn parse(v: &Value) -> Vec<TranscriptEntry> {
    let ts = str_at(v, "created_at");
    let content = str_at(v, "content").unwrap_or_default();
    match (str_at(v, "type"), str_at(v, "source")) {
        (Some("USER_INPUT"), _) => {
            let said = user_request(content);
            if is_compact_command(said) {
                return Vec::new();
            }
            entry(TranscriptRole::User, said, ts).into_iter().collect()
        }
        (Some("PLANNER_RESPONSE"), _) => {
            let mut out = Vec::new();
            if let Some(t) = str_at(v, "thinking") {
                out.extend(entry(TranscriptRole::Thinking, t, ts));
            }
            out.extend(entry(TranscriptRole::Assistant, content, ts));
            for call in v
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                out.push(tool_call(
                    call.get("args"),
                    str_at(call, "name"),
                    str_at(call, "id"),
                    ts,
                    None,
                ));
            }
            out
        }
        (Some("GENERIC"), Some("MODEL")) => vec![tool_entry(
            TranscriptRole::ToolResult,
            content,
            None,
            None,
            false,
            ts,
        )],
        (Some("CHECKPOINT"), _) if is_compaction(content) => entry(
            TranscriptRole::Compaction,
            "The conversation was compacted",
            ts,
        )
        .into_iter()
        .collect(),
        _ => Vec::new(),
    }
}

/// Whether a step is a prompt that starts a turn.
pub(crate) fn is_prompt(v: &Value) -> bool {
    str_at(v, "type") == Some("USER_INPUT")
}

/// Whether a `CHECKPOINT` step's content is a compaction's summary.
pub fn is_compaction(content: &str) -> bool {
    content
        .trim_start()
        .starts_with("# Resuming from a compaction")
}

/// What the person asked: the text inside `<USER_REQUEST>`, or the whole
/// content when it has no such wrapper.
pub(crate) fn user_request(content: &str) -> &str {
    const OPEN: &str = "<USER_REQUEST>";
    const CLOSE: &str = "</USER_REQUEST>";
    let Some(start) = content.find(OPEN).map(|i| i + OPEN.len()) else {
        return content.trim();
    };
    let end = content[start..]
        .rfind(CLOSE)
        .map_or(content.len(), |e| start + e);
    content[start..end].trim()
}

fn is_compact_command(text: &str) -> bool {
    text == "/compact" || text.starts_with("/compact ")
}

/// The transcript of the newest conversation agy holds for `cwd` under the
/// app data directory `root` (`~/.gemini/antigravity-cli`).
pub(crate) fn locate(root: &Path, cwd: &Path) -> Option<PathBuf> {
    let raw = std::fs::read(root.join("cache").join("last_conversations.json")).ok()?;
    let v: Value = serde_json::from_slice(&raw).ok()?;
    let want = cwd.to_string_lossy();
    let id = v
        .as_object()?
        .iter()
        .find(|(k, _)| k.trim_end_matches('/') == want.trim_end_matches('/'))?
        .1
        .as_str()?;
    if id.is_empty() || id.contains(['/', '\\']) || id.starts_with('.') {
        return None;
    }
    let path = transcript_path(root, id);
    path.is_file().then_some(path)
}

/// Where agy writes conversation `id`'s transcript under `root`.
pub fn transcript_path(root: &Path, id: &str) -> PathBuf {
    root.join("brain")
        .join(id)
        .join(".system_generated")
        .join("logs")
        .join("transcript_full.jsonl")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn reads_prompts_replies_tools_and_compactions() {
        let user = json!({"step_index": 0, "source": "USER_EXPLICIT", "type": "USER_INPUT",
            "created_at": "2026-11-20T05:43:58Z",
            "content": "<USER_REQUEST>\nWhat is your codename?\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nThe current local time is: 2026-11-19T23:43:58-06:00.\n</ADDITIONAL_METADATA>"});
        let e = parse(&user);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].role, TranscriptRole::User);
        assert_eq!(e[0].text, "What is your codename?");
        assert_eq!(e[0].ts.as_deref(), Some("2026-11-20T05:43:58Z"));

        let call = json!({"step_index": 3, "source": "MODEL", "type": "PLANNER_RESPONSE",
            "model": "gemini-4-neon-high", "input_tokens": 2233, "cache_read_tokens": 8001,
            "output_tokens": 83, "thinking": "Run it.",
            "tool_calls": [{"name": "run_command", "args": {"CommandLine": "echo ok"}}]});
        let e = parse(&call);
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].role, TranscriptRole::Thinking);
        assert_eq!(e[1].role, TranscriptRole::ToolCall);
        assert_eq!(e[1].tool_name.as_deref(), Some("run_command"));

        let out = json!({"source": "MODEL", "type": "GENERIC",
            "content": "Command exited with code 0.\n\nCommand output:\nok\r\n"});
        assert_eq!(parse(&out)[0].role, TranscriptRole::ToolResult);

        let reply = json!({"source": "MODEL", "type": "PLANNER_RESPONSE", "content": "ok"});
        let e = parse(&reply);
        assert_eq!(
            (e[0].role, e[0].text.as_str()),
            (TranscriptRole::Assistant, "ok")
        );

        // Hook and system notes are not conversation.
        let note = json!({"source": "SYSTEM_SDK", "type": "SYSTEM_MESSAGE",
            "content": "The following is a <SYSTEM_MESSAGE> not actually sent by the user."});
        assert!(parse(&note).is_empty());

        // A compaction is one entry; the /compact prompt is left out.
        let ask = json!({"source": "USER_EXPLICIT", "type": "USER_INPUT",
            "content": "<USER_REQUEST>\n/compact keep the decisions\n</USER_REQUEST>"});
        assert!(parse(&ask).is_empty());
        let cp = json!({"source": "SYSTEM", "type": "CHECKPOINT",
            "content": "# Resuming from a compaction\n\nYou are continuing work..."});
        assert_eq!(parse(&cp)[0].role, TranscriptRole::Compaction);
    }

    #[test]
    fn finds_the_folders_newest_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let cwd = Path::new("/Users/me/.quark/workspaces/p1");
        assert!(locate(root, cwd).is_none());
        std::fs::create_dir_all(root.join("cache")).unwrap();
        std::fs::write(
            root.join("cache/last_conversations.json"),
            r#"{"/Users/me/.quark/workspaces/p1": "c1", "/elsewhere": "c2", "/bad": "../x"}"#,
        )
        .unwrap();
        // Named, but agy has not written it yet.
        assert!(locate(root, cwd).is_none());
        let path = transcript_path(root, "c1");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "").unwrap();
        assert_eq!(locate(root, cwd), Some(path));
        assert!(locate(root, Path::new("/bad")).is_none());
    }
}
