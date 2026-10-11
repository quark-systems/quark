//! Slice 3's shadow: does the file protocol read firstmate's status lines
//! the way firstmate does?
//!
//! Until slice 3 switches on, firstmate's workers write their status files
//! and the slice 1 bridge mirrors every line into the log. For each line,
//! [`compare`] puts what [`parse_status_line`] makes of it next to what
//! firstmate makes of it (`bin/fm-classify-lib.sh`: the verb, the decision
//! key, the note, and a pull request named in a `done` line), both as a
//! [`Reading`]. A difference means a worker briefed for firstmate would be
//! misunderstood once the native protocol reads its file.
//!
//! Lines firstmate writes itself when it answers (`resolved`,
//! `captain-held`) are not worker messages and are skipped, as are lines
//! the native protocol ignores (blank, `#` comments).

use quark_core::worker::WorkerMessage;
use serde::{Deserialize, Serialize};

use crate::file::{parse_status_line, pull_request_in};

/// The operation a divergence is recorded under.
pub const OPERATION: &str = "status_line";

/// What one status line says, in terms both sides share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reading {
    /// `ask`, `done`, `report`, or `none` for a line that is not a message.
    pub kind: String,
    /// For a report: its state word.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// The decision key: an ask's, or one a report states explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<String>,
}

impl Reading {
    fn none() -> Self {
        Self {
            kind: "none".into(),
            state: None,
            key: None,
            text: String::new(),
            pull_request: None,
        }
    }
}

/// Lines firstmate appends itself.
fn firstmates_own(verb: &str) -> bool {
    matches!(verb, "resolved" | "captain-held")
}

/// The two readings of `raw` when they differ. `None` when they agree or
/// the line is not compared.
pub fn compare(raw: &str) -> Option<(Reading, Reading)> {
    let native = match parse_status_line(raw) {
        Ok(None) => return None,
        Ok(Some(m)) => native_reading(&m),
        Err(_) => Reading::none(),
    };
    let bash = firstmate_reading(raw)?;
    (bash != native).then_some((bash, native))
}

/// The native protocol's reading of a parsed message.
pub fn native_reading(m: &WorkerMessage) -> Reading {
    match m {
        WorkerMessage::Ask { key, question } => Reading {
            kind: "ask".into(),
            state: None,
            key: Some(key.clone()),
            text: question.clone(),
            pull_request: None,
        },
        WorkerMessage::Done {
            summary,
            pull_request,
        } => Reading {
            kind: "done".into(),
            state: None,
            key: None,
            text: summary.clone(),
            pull_request: pull_request.clone(),
        },
        WorkerMessage::Report { state, note, key } => Reading {
            kind: "report".into(),
            state: Some(state.clone()),
            key: key.clone(),
            text: note.clone(),
            pull_request: None,
        },
        WorkerMessage::Learned { fact } => Reading {
            kind: "report".into(),
            state: Some("learned".into()),
            key: None,
            text: fact.clone(),
            pull_request: None,
        },
        WorkerMessage::Signal { signal } => Reading {
            kind: "report".into(),
            state: Some(signal.clone()),
            key: None,
            text: String::new(),
            pull_request: None,
        },
    }
}

/// Firstmate's reading of a line; `None` for a line firstmate wrote itself.
pub fn firstmate_reading(raw: &str) -> Option<Reading> {
    let line = raw.trim_end_matches(['\n', '\r']);
    // A line without a colon declares nothing (continuation prose).
    let Some((prefix, rest)) = line.split_once(':') else {
        return Some(Reading::none());
    };
    let verb = verb_of(prefix);
    if firstmates_own(&verb) {
        return None;
    }
    if verb.is_empty() {
        return Some(Reading::none());
    }
    let note = rest.trim();
    // `[key=<slug>]` before the colon, else at the head of the note. A
    // malformed slug makes firstmate skip the decision.
    let head_key = note.strip_prefix("[key=").and_then(|r| r.split_once(']'));
    let (stated, key, note) = match (bracket_key(prefix), head_key) {
        (Some(k), _) => (true, k, note.to_string()),
        (None, Some((k, after))) if valid_key(k) => {
            (true, k.to_string(), after.trim_start().into())
        }
        (None, Some((k, _))) => (true, k.to_string(), note.to_string()),
        (None, None) => (false, String::new(), note.to_string()),
    };
    let key = (stated && valid_key(&key)).then_some(key);
    Some(match verb.as_str() {
        "needs-decision" => Reading {
            kind: "ask".into(),
            state: None,
            key: if stated { key } else { Some("default".into()) },
            text: note,
            pull_request: None,
        },
        "done" => Reading {
            kind: "done".into(),
            state: None,
            key: None,
            pull_request: pull_request_in(&note),
            text: if note.is_empty() { "done".into() } else { note },
        },
        _ => Reading {
            kind: "report".into(),
            // Of the other verbs only a blocker opens a decision by key.
            key: if verb == "blocked" { key } else { None },
            state: Some(verb),
            text: note,
            pull_request: None,
        },
    })
}

/// `status_line_verb`: the prefix up to any `[`, trimmed, without
/// `corr=<16 hex>` words after the first.
fn verb_of(prefix: &str) -> String {
    let head = prefix.split('[').next().unwrap_or("").trim();
    if !head.contains("corr=") {
        return head.to_string();
    }
    let mut words = head.split_whitespace();
    let mut out = words.next().unwrap_or("").to_string();
    for w in words {
        let corr = w
            .strip_prefix("corr=")
            .is_some_and(|h| h.len() == 16 && h.bytes().all(|b| b.is_ascii_hexdigit()));
        if !corr {
            out.push(' ');
            out.push_str(w);
        }
    }
    out
}

fn bracket_key(prefix: &str) -> Option<String> {
    let start = prefix.find("[key=")? + "[key=".len();
    let len = prefix[start..].find(']')?;
    Some(prefix[start..start + len].to_string())
}

fn valid_key(k: &str) -> bool {
    !k.is_empty()
        && k.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agrees(raw: &str) {
        assert_eq!(compare(raw), None, "{raw}");
    }

    #[test]
    fn ordinary_lines_agree() {
        agrees("working: starting on the parser");
        agrees("needs-decision [key=api-shape]: REST or gRPC?");
        agrees("needs-decision: [key=api-shape] REST or gRPC?");
        agrees("needs-decision: which one");
        agrees("blocked: the staging token expired");
        agrees("paused: waiting on CI until 2026-10-07T06:00Z");
        agrees("failed: gave up");
        agrees("done: PR https://github.com/o/r/pull/12 checks green");
        agrees("done: report at data/t1/report.md");
        agrees("continuation prose without a colon");
        agrees("");
        agrees("# a comment: ignored");
        // Found by journey-shadows: a keyed blocker opens a decision.
        agrees("blocked [key=creds]: need a token");
        agrees("blocked: [key=creds] need a token");
        agrees("working [key=x]: a key on a report that opens nothing");
        agrees("needs-decision[key=x]: pick");
        agrees("done: see https://ci.example/run/1 and PR https://github.com/o/r/pull/3");
        agrees("done: branch pushed to https://example.com/o/r");
    }

    #[test]
    fn firstmates_own_lines_are_skipped() {
        assert_eq!(compare("resolved [key=api]: went with REST"), None);
        assert_eq!(compare("captain-held [key=k]: moved to backlog"), None);
    }

    #[test]
    fn differences_are_reported() {
        // A malformed key opens nothing in firstmate.
        let (b, n) = compare("needs-decision [key=a b]: pick").unwrap();
        assert_eq!(b.key, None);
        assert_eq!(n.key.as_deref(), Some("default"));
    }

    #[test]
    fn pull_requests_match_firstmates_pattern() {
        assert_eq!(
            pull_request_in("PR https://github.com/o/r/pull/12, green").as_deref(),
            Some("https://github.com/o/r/pull/12")
        );
        assert_eq!(
            pull_request_in("(https://github.com/o/r/pull/7)").as_deref(),
            Some("https://github.com/o/r/pull/7")
        );
        assert_eq!(pull_request_in("https://github.com/o/r/pulls"), None);
    }
}
