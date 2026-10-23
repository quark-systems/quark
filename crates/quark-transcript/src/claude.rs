//! Claude Code session logs: one JSON object per line with a top-level
//! `type`. `user` and `assistant` lines carry an Anthropic Messages API
//! `message`; everything else (attachments, summaries, snapshots, queue
//! operations) is bookkeeping. Sidechain lines belong to subagents and meta
//! lines are harness-injected context, so both are skipped, and so is the
//! summary a compaction leaves in place of the earlier conversation. The
//! compaction itself (a `system` `compact_boundary` line) is one
//! [`TranscriptRole::Compaction`] entry, and the `/compact` command that
//! asked for it is left out. Text the person
//! pasted reaches the log wrapped in `<pasted_content id="..">` tags, which
//! are dropped so the entry reads as what they sent.

use serde_json::Value;

use crate::{entry, str_at, text_of, tool_call, tool_entry, TranscriptEntry, TranscriptRole};

pub(crate) fn parse(v: &Value) -> Vec<TranscriptEntry> {
    let kind = str_at(v, "type");
    if kind == Some("system") && str_at(v, "subtype") == Some("compact_boundary") {
        return compaction(v).into_iter().collect();
    }
    if !matches!(kind, Some("user" | "assistant")) {
        return Vec::new();
    }
    let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    // A compaction writes its summary of the earlier conversation as a user
    // line: the harness's note to itself, not something anyone sent.
    if flag("isSidechain") || flag("isMeta") || flag("isCompactSummary") {
        return Vec::new();
    }
    let ts = str_at(v, "timestamp");
    let Some(content) = v.get("message").and_then(|m| m.get("content")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match content {
        Value::String(s) if kind == Some("user") && is_compact_command(s) => {}
        Value::String(s) => {
            if kind == Some("user") {
                out.extend(entry(TranscriptRole::User, &unwrap_pasted(s), ts));
            } else {
                out.extend(entry(TranscriptRole::Assistant, s, ts));
            }
        }
        Value::Array(blocks) => {
            for b in blocks {
                match str_at(b, "type") {
                    Some("text") => {
                        let text = str_at(b, "text").unwrap_or("");
                        if kind == Some("user") && is_compact_command(text) {
                            continue;
                        }
                        if kind == Some("user") {
                            out.extend(entry(TranscriptRole::User, &unwrap_pasted(text), ts));
                        } else {
                            out.extend(entry(TranscriptRole::Assistant, text, ts));
                        }
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

/// The entry a `compact_boundary` line stands for: who compacted the
/// conversation (`compactMetadata.trigger`: `auto` is Claude Code on its
/// own, `manual` a `/compact`) and how big it was (`preTokens`).
fn compaction(v: &Value) -> Option<TranscriptEntry> {
    let meta = v.get("compactMetadata");
    let auto = meta.and_then(|m| str_at(m, "trigger")) == Some("auto");
    let size = meta
        .and_then(|m| m.get("preTokens"))
        .and_then(Value::as_u64)
        .map(|t| format!(" at {}k tokens", (t + 500) / 1000))
        .unwrap_or_default();
    let text = if auto {
        format!("Claude Code compacted the conversation on its own{size}")
    } else {
        format!("The conversation was compacted{size}")
    };
    entry(TranscriptRole::Compaction, &text, str_at(v, "timestamp"))
}

/// Whether a user line is the `/compact` command Claude Code logs before
/// compacting: its `compact_boundary` line stands for it.
fn is_compact_command(text: &str) -> bool {
    text.trim_start()
        .starts_with("<command-name>/compact</command-name>")
}

/// `text` with each `<pasted_content id="X">..</pasted_content id="X">`
/// wrapper replaced by what it holds. A tag without its matching close stays.
fn unwrap_pasted(text: &str) -> String {
    const OPEN: &str = "<pasted_content id=\"";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(OPEN) {
        let after = &rest[at + OPEN.len()..];
        let Some(id_end) = after.find("\">") else {
            break;
        };
        let id = &after[..id_end];
        let body = &after[id_end + 2..];
        let close = format!("</pasted_content id=\"{id}\">");
        let Some(close_at) = body.find(&close) else {
            break;
        };
        out.push_str(&rest[..at]);
        let inner = body[..close_at]
            .strip_prefix('\n')
            .unwrap_or(&body[..close_at]);
        out.push_str(inner.strip_suffix('\n').unwrap_or(inner));
        rest = &body[close_at + close.len()..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_compaction_summary_is_not_a_message() {
        let line = |extra: &str| {
            serde_json::from_str::<Value>(&format!(
                r#"{{"type":"user"{extra},"timestamp":"2026-10-13T19:09:11Z","message":{{"role":"user","content":"This session is being continued from a previous conversation."}}}}"#
            ))
            .unwrap()
        };
        assert!(parse(&line(r#","isCompactSummary":true"#)).is_empty());
        assert_eq!(parse(&line("")).len(), 1);
    }

    #[test]
    fn a_compaction_is_one_entry_and_its_command_is_left_out() {
        let v = |s: &str| serde_json::from_str::<Value>(s).unwrap();
        let auto = parse(&v(
            r#"{"type":"system","subtype":"compact_boundary","content":"Conversation compacted","timestamp":"2026-10-23T10:00:00Z","compactMetadata":{"trigger":"auto","preTokens":167450}}"#,
        ));
        assert_eq!(auto.len(), 1);
        assert_eq!(auto[0].role, TranscriptRole::Compaction);
        assert_eq!(
            auto[0].text,
            "Claude Code compacted the conversation on its own at 167k tokens"
        );
        assert_eq!(auto[0].ts.as_deref(), Some("2026-10-23T10:00:00Z"));
        let manual = parse(&v(
            r#"{"type":"system","subtype":"compact_boundary","compactMetadata":{"trigger":"manual"}}"#,
        ));
        assert_eq!(manual[0].text, "The conversation was compacted");
        assert!(parse(&v(
            r#"{"type":"system","subtype":"informational","content":"x"}"#
        ))
        .is_empty());
        let command = r#"<command-name>/compact</command-name>\n<command-message>compact</command-message>\n<command-args>Keep open decisions</command-args>"#;
        assert!(parse(&v(&format!(
            r#"{{"type":"user","message":{{"role":"user","content":"{command}"}}}}"#
        )))
        .is_empty());
        assert!(parse(&v(&format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"{command}"}}]}}}}"#
        )))
        .is_empty());
    }

    #[test]
    fn pasted_text_reads_as_sent() {
        assert_eq!(
            unwrap_pasted("<pasted_content id=\"967c\">\nBuild it.\nThen ship.\n</pasted_content id=\"967c\">"),
            "Build it.\nThen ship."
        );
        assert_eq!(
            unwrap_pasted("see <pasted_content id=\"a\">x</pasted_content id=\"a\"> and <pasted_content id=\"b\">y</pasted_content id=\"b\">"),
            "see x and y"
        );
        let open = "<pasted_content id=\"a\">no close";
        assert_eq!(unwrap_pasted(open), open);
        assert_eq!(unwrap_pasted("plain"), "plain");
    }

    #[test]
    fn a_pasted_user_line_has_no_tags() {
        let v: Value = serde_json::from_str(
            r#"{"type":"user","message":{"role":"user","content":"<pasted_content id=\"1\">\nhi\n</pasted_content id=\"1\">"}}"#,
        )
        .unwrap();
        let e = parse(&v);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].text, "hi");
    }
}
