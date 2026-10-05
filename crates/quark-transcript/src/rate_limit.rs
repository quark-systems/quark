//! Rate-limit signals in harness session logs.
//!
//! Each harness records that its account ran out in its own way. These are
//! the exact lines matched:
//!
//! - Claude Code writes the failed turn as a synthetic assistant line,
//!   `{"type":"assistant","isApiErrorMessage":true,"error":"rate_limit",...}`,
//!   whose text is what the terminal showed (for example "You've hit your
//!   session limit"). It writes one only after its own retries gave up, so a
//!   transient 429 it recovered from leaves no such line. Other `error`
//!   values (`authentication_failed`, `server_error`, ...) are not limits.
//! - Codex records the account's limits with every `token_count` event:
//!   `{"type":"event_msg","payload":{"type":"token_count","rate_limits":{...}}}`.
//!   The account is out when `rate_limits.rate_limit_reached_type` is set, or
//!   when the `primary` or `secondary` window's `used_percent` is 100 or
//!   more. An `error` event whose `codex_error_info` is
//!   `usage_limit_exceeded` says the same, where a Codex version writes it.

use serde_json::Value;

use crate::{str_at, text_of};

/// Longest message kept from a rate-limit line, in characters.
const MAX_MESSAGE_CHARS: usize = 300;

/// A harness's report that its account hit a rate limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimit {
    /// The log line matched, e.g. `claude: assistant error=rate_limit`.
    pub signal: &'static str,
    /// What the harness said about it, when it said anything.
    pub message: Option<String>,
    /// When the harness wrote the line (RFC 3339), when the line says.
    pub ts: Option<String>,
}

pub(crate) const CLAUDE_API_ERROR: &str = "claude: assistant isApiErrorMessage error=rate_limit";
pub(crate) const CODEX_REACHED_TYPE: &str =
    "codex: token_count rate_limits.rate_limit_reached_type";
pub(crate) const CODEX_USED_PERCENT: &str = "codex: token_count rate_limits used_percent>=100";
pub(crate) const CODEX_ERROR_EVENT: &str = "codex: error codex_error_info=usage_limit_exceeded";

fn limit(signal: &'static str, message: &str, ts: Option<&str>) -> RateLimit {
    let message = message.split_whitespace().collect::<Vec<_>>().join(" ");
    RateLimit {
        signal,
        message: (!message.is_empty()).then(|| message.chars().take(MAX_MESSAGE_CHARS).collect()),
        ts: ts.map(str::to_string),
    }
}

pub(crate) fn claude(v: &Value) -> Option<RateLimit> {
    let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    // A subagent's limit is its parent's to report.
    if str_at(v, "type") != Some("assistant")
        || !flag("isApiErrorMessage")
        || flag("isSidechain")
        || str_at(v, "error") != Some("rate_limit")
    {
        return None;
    }
    let text = v
        .get("message")
        .and_then(|m| m.get("content"))
        .map(text_of)
        .unwrap_or_default();
    Some(limit(CLAUDE_API_ERROR, &text, str_at(v, "timestamp")))
}

pub(crate) fn codex(v: &Value) -> Option<RateLimit> {
    if str_at(v, "type") != Some("event_msg") {
        return None;
    }
    let ts = str_at(v, "timestamp");
    let p = v.get("payload")?;
    match str_at(p, "type") {
        Some("token_count") => {
            let limits = p.get("rate_limits").filter(|l| l.is_object())?;
            if let Some(reached) = limits
                .get("rate_limit_reached_type")
                .filter(|r| !r.is_null())
            {
                let reached = reached
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| reached.to_string());
                return Some(limit(CODEX_REACHED_TYPE, &reached, ts));
            }
            ["primary", "secondary"].iter().find_map(|window| {
                let w = limits.get(window)?;
                let used = w.get("used_percent")?.as_f64()?;
                (used >= 100.0).then(|| {
                    limit(
                        CODEX_USED_PERCENT,
                        &format!("{window} window {used}% used"),
                        ts,
                    )
                })
            })
        }
        Some("error") => {
            let info = p.get("codex_error_info")?;
            // A unit variant is a string; one with data is a single-key object.
            let exceeded = info.as_str() == Some("usage_limit_exceeded")
                || info.get("usage_limit_exceeded").is_some();
            exceeded.then(|| limit(CODEX_ERROR_EVENT, str_at(p, "message").unwrap_or(""), ts))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SessionFormat;

    fn of(format: SessionFormat, line: &str) -> Option<RateLimit> {
        format.rate_limit(&serde_json::from_str(line).unwrap())
    }

    #[test]
    fn claude_reports_a_rate_limited_turn() {
        let line = r#"{"type":"assistant","timestamp":"2026-10-05T10:00:00.000Z","isApiErrorMessage":true,"error":"rate_limit","message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"You've hit your session limit · resets 3pm"}]}}"#;
        let l = of(SessionFormat::Claude, line).unwrap();
        assert_eq!(l.signal, CLAUDE_API_ERROR);
        assert_eq!(
            l.message.as_deref(),
            Some("You've hit your session limit · resets 3pm")
        );
        assert_eq!(l.ts.as_deref(), Some("2026-10-05T10:00:00.000Z"));
    }

    #[test]
    fn claude_ignores_other_errors_subagents_and_ordinary_turns() {
        for line in [
            r#"{"type":"assistant","isApiErrorMessage":true,"error":"server_error","message":{"content":"API Error"}}"#,
            r#"{"type":"assistant","isApiErrorMessage":true,"error":"rate_limit","isSidechain":true,"message":{"content":"limit"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"the API returned a rate_limit error earlier"}]}}"#,
            r#"{"type":"user","error":"rate_limit","isApiErrorMessage":true,"message":{"content":"x"}}"#,
        ] {
            assert_eq!(of(SessionFormat::Claude, line), None, "{line}");
        }
    }

    #[test]
    fn codex_reports_a_reached_limit() {
        let reached = r#"{"timestamp":"2026-10-05T10:00:00.000Z","type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{"limit_id":"codex","primary":{"used_percent":100.0,"window_minutes":300,"resets_at":1781555503},"secondary":{"used_percent":41.0,"window_minutes":10080,"resets_at":1781802993},"plan_type":"plus","rate_limit_reached_type":"rate_limit_reached"}}}"#;
        let l = of(SessionFormat::Codex, reached).unwrap();
        assert_eq!(l.signal, CODEX_REACHED_TYPE);
        assert_eq!(l.message.as_deref(), Some("rate_limit_reached"));

        let full = reached.replace("\"rate_limit_reached\"", "null");
        let l = of(SessionFormat::Codex, &full).unwrap();
        assert_eq!(l.signal, CODEX_USED_PERCENT);
        assert_eq!(l.message.as_deref(), Some("primary window 100% used"));

        let error = r#"{"timestamp":"t","type":"event_msg","payload":{"type":"error","message":"You've hit your usage limit.","codex_error_info":"usage_limit_exceeded"}}"#;
        assert_eq!(
            of(SessionFormat::Codex, error).unwrap().signal,
            CODEX_ERROR_EVENT
        );
    }

    #[test]
    fn codex_ignores_usage_below_the_limit_and_other_errors() {
        for line in [
            r#"{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":6.0},"secondary":{"used_percent":99.9},"rate_limit_reached_type":null}}}"#,
            r#"{"type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":null}}"#,
            r#"{"type":"event_msg","payload":{"type":"error","message":"stream disconnected","codex_error_info":"other"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"usage_limit_exceeded"}]}}"#,
        ] {
            assert_eq!(of(SessionFormat::Codex, line), None, "{line}");
        }
    }

    #[test]
    fn pi_reports_none() {
        let line = r#"{"type":"assistant","isApiErrorMessage":true,"error":"rate_limit"}"#;
        assert_eq!(of(SessionFormat::Pi, line), None);
    }
}
