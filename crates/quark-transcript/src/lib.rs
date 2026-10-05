//! Harness session logs, parsed into neutral transcript entries.
//!
//! Every supported harness writes an append-only JSONL session log. This crate
//! knows where each one lives ([`locate`]), how to read only what was
//! appended since a byte offset ([`read_from`]), and how to turn one log line
//! into zero or more [`TranscriptEntry`] values ([`SessionFormat::parse_line`]).
//! Chat output comes from these logs, never from the terminal screen.
//!
//! Supported formats:
//!
//! - [`SessionFormat::Claude`]: Claude Code,
//!   `<config dir>/projects/<cwd slug>/<session id>.jsonl`.
//! - [`SessionFormat::Codex`]: Codex CLI rollouts,
//!   `<codex home>/sessions/YYYY/MM/DD/rollout-<ts>-<id>.jsonl`.
//! - [`SessionFormat::Pi`]: Pi coding agent,
//!   `<agent dir>/sessions/--<cwd slug>--/<ts>_<id>.jsonl`.

mod claude;
mod codex;
mod locate;
mod pi;
mod rate_limit;
mod reader;

pub use locate::{locate, SessionRoots};
pub use quark_systems::{TranscriptEntry, TranscriptRole};
pub use rate_limit::RateLimit;
pub use reader::{read_from, ReadBatch, MAX_BATCH_BYTES};

use serde_json::Value;

/// Longest `text` an entry carries; longer text is cut on a character
/// boundary and marked `truncated`.
pub const MAX_TEXT_BYTES: usize = 16 * 1024;

/// A harness session-log format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionFormat {
    Claude,
    Codex,
    Pi,
}

impl SessionFormat {
    pub const ALL: [SessionFormat; 3] = [
        SessionFormat::Claude,
        SessionFormat::Codex,
        SessionFormat::Pi,
    ];

    /// The format a harness writes, by the harness name the engine reports.
    pub fn for_harness(harness: &str) -> Option<SessionFormat> {
        match harness {
            "claude" => Some(SessionFormat::Claude),
            "codex" => Some(SessionFormat::Codex),
            "pi" | "pi-signed" => Some(SessionFormat::Pi),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SessionFormat::Claude => "claude",
            SessionFormat::Codex => "codex",
            SessionFormat::Pi => "pi",
        }
    }

    /// Entries in one log line. Lines that are not JSON, or that record
    /// bookkeeping rather than conversation, yield nothing.
    pub fn parse_line(self, line: &str) -> Vec<TranscriptEntry> {
        match serde_json::from_str::<Value>(line) {
            Ok(v) => self.parse_value(&v),
            Err(_) => Vec::new(),
        }
    }

    /// Entries in one already-parsed log line.
    pub fn parse_value(self, v: &Value) -> Vec<TranscriptEntry> {
        match self {
            SessionFormat::Claude => claude::parse(v),
            SessionFormat::Codex => codex::parse(v),
            SessionFormat::Pi => pi::parse(v),
        }
    }

    /// The rate limit one already-parsed log line reports, if it reports
    /// one. Claude Code and Codex only; see [`RateLimit`] for the lines
    /// matched.
    pub fn rate_limit(self, v: &Value) -> Option<RateLimit> {
        match self {
            SessionFormat::Claude => rate_limit::claude(v),
            SessionFormat::Codex => rate_limit::codex(v),
            SessionFormat::Pi => None,
        }
    }
}

/// Builds an entry, dropping empty text and enforcing [`MAX_TEXT_BYTES`].
pub(crate) fn entry(role: TranscriptRole, text: &str, ts: Option<&str>) -> Option<TranscriptEntry> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (text, truncated) = cap(trimmed);
    Some(TranscriptEntry {
        role,
        text,
        tool_name: None,
        tool_call_id: None,
        is_error: false,
        truncated,
        ts: ts.map(str::to_string),
    })
}

/// A tool call or result. Unlike [`entry`], empty text is kept: a tool that
/// printed nothing still ran.
pub(crate) fn tool_entry(
    role: TranscriptRole,
    text: &str,
    name: Option<&str>,
    call_id: Option<&str>,
    is_error: bool,
    ts: Option<&str>,
) -> TranscriptEntry {
    let (text, truncated) = cap(text.trim_end());
    TranscriptEntry {
        role,
        text,
        tool_name: name.map(str::to_string),
        tool_call_id: call_id.map(str::to_string),
        is_error,
        truncated,
        ts: ts.map(str::to_string),
    }
}

fn cap(s: &str) -> (String, bool) {
    if s.len() <= MAX_TEXT_BYTES {
        return (s.to_string(), false);
    }
    let mut end = MAX_TEXT_BYTES;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_string(), true)
}

/// Text of a JSON value that is either a string or an array of
/// `{"type": "text" | "input_text" | "output_text", "text": ...}` blocks.
pub(crate) fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|b| match b {
                Value::String(s) => Some(s.as_str()),
                _ => match b.get("type").and_then(Value::as_str) {
                    Some("text" | "input_text" | "output_text") => {
                        b.get("text").and_then(Value::as_str)
                    }
                    _ => None,
                },
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A tool's input as display text: strings as-is, anything else as compact
/// JSON.
pub(crate) fn input_text(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

pub(crate) fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_long_text_on_a_char_boundary() {
        let long = "é".repeat(MAX_TEXT_BYTES);
        let e = entry(TranscriptRole::Assistant, &long, None).unwrap();
        assert!(e.truncated);
        assert!(e.text.len() <= MAX_TEXT_BYTES);
        assert!(e.text.chars().all(|c| c == 'é'));
    }

    #[test]
    fn drops_blank_text() {
        assert!(entry(TranscriptRole::User, "  \n", None).is_none());
    }

    #[test]
    fn non_json_lines_yield_nothing() {
        for f in SessionFormat::ALL {
            assert!(f.parse_line("not json").is_empty());
            assert!(f.parse_line("").is_empty());
        }
    }

    #[test]
    fn harness_names_map_to_formats() {
        assert_eq!(
            SessionFormat::for_harness("claude"),
            Some(SessionFormat::Claude)
        );
        assert_eq!(
            SessionFormat::for_harness("codex"),
            Some(SessionFormat::Codex)
        );
        assert_eq!(SessionFormat::for_harness("pi"), Some(SessionFormat::Pi));
        assert_eq!(SessionFormat::for_harness("bob"), None);
    }
}
