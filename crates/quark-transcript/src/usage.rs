//! Token use per turn, read from a session log.
//!
//! A turn starts at a prompt from the user (or from whatever drives the
//! agent: a brief, a steer, a wake) and runs to the next one. Its usage is
//! the sum over its model calls, split by model:
//!
//! - Claude Code: each assistant entry's `message.usage` and
//!   `message.model`. Streamed entries of one call share a message id and
//!   repeat its usage, so the last one per id counts.
//! - Codex: `token_count` events' `info.last_token_usage`; the model is the
//!   latest `turn_context`'s. Codex can repeat a `token_count`, so one whose
//!   running total did not move is skipped.
//! - Pi: each assistant message's `usage`, including Pi's own cost.
//!
//! [`read_turns`] reads completed turns from a byte offset and returns where
//! to resume: the start of the turn still open at the end, read again once
//! it has ended. A log that went quiet can have its open turn closed early
//! (`close_open`); calls written to it later come back as a continuation of
//! the same turn id.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{str_at, SessionFormat};

/// Tokens one model used in a turn.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelUsage {
    /// As the harness wrote it, e.g. `claude-opus-5-5` or `gpt-5-codex`;
    /// empty when the log does not say.
    pub model: String,
    /// Input tokens, including prompt cache reads and writes.
    pub input: u64,
    pub output: u64,
    /// Of `input`, read from the prompt cache.
    pub cache_read: u64,
    /// Of `input`, written to the prompt cache.
    pub cache_write: u64,
    /// Model calls.
    pub calls: u32,
    /// What the harness itself says the calls cost, in US dollars, when it
    /// records cost (Pi does).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

impl ModelUsage {
    pub fn add(&mut self, o: &ModelUsage) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
        self.calls += o.calls;
        self.cost_usd = match (self.cost_usd, o.cost_usd) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
        };
    }
}

/// One turn's token use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnUsage {
    /// The prompt entry's id where the log has one, else its byte offset.
    pub id: String,
    /// When the prompt was written, as the log says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// More calls of a turn already reported, after it was closed early.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub continued: bool,
    pub models: Vec<ModelUsage>,
}

/// Where to resume reading a log.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageCursor {
    pub offset: u64,
    /// The turn open at `offset`, when it was closed early.
    #[serde(default)]
    pub turn: Option<String>,
    /// Codex: the model in force at `offset`.
    #[serde(default)]
    pub model: Option<String>,
}

/// Turns completed in `path` from `cursor` on, and the cursor to resume
/// from. With `close_open`, the turn still open at the end is returned too.
/// A log shorter than the cursor was replaced and is read from the start.
pub fn read_turns(
    path: &Path,
    cursor: &UsageCursor,
    format: SessionFormat,
    close_open: bool,
) -> std::io::Result<(Vec<TurnUsage>, UsageCursor)> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let cursor = if cursor.offset > len {
        UsageCursor::default()
    } else {
        cursor.clone()
    };
    f.seek(SeekFrom::Start(cursor.offset))?;
    let mut reader = BufReader::new(f.take(len - cursor.offset));
    let mut model = cursor.model.clone();
    let mut turns = Vec::new();
    // The open turn, the offset it started at, and the model then.
    let mut open = cursor
        .turn
        .clone()
        .map(|id| (cursor.offset, model.clone(), Builder::new(id, None, true)));
    let mut pos = cursor.offset;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 || !line.ends_with('\n') {
            break; // end, or a line still being written
        }
        let at = pos;
        pos += n as u64;
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some((id, ts)) = prompt(format, &v) {
            if let Some((_, _, b)) = open.take() {
                turns.extend(b.finish());
            }
            let id = id.unwrap_or_else(|| at.to_string());
            open = Some((at, model.clone(), Builder::new(id, ts, false)));
            continue;
        }
        if format == SessionFormat::Codex {
            if let Some(m) = codex_model(&v) {
                model = Some(m.to_string());
                continue;
            }
        }
        if let Some(b) = open.as_mut().map(|(_, _, b)| b) {
            b.add(format, &v, model.as_deref());
        }
    }
    let next = match open {
        Some((_, _, b)) if close_open => {
            let id = b.id.clone();
            turns.extend(b.finish());
            UsageCursor {
                offset: pos,
                turn: Some(id),
                model,
            }
        }
        Some((start, model_then, b)) => UsageCursor {
            offset: start,
            turn: b.continued.then_some(b.id),
            model: model_then,
        },
        None => UsageCursor {
            offset: pos,
            turn: None,
            model,
        },
    };
    Ok((turns, next))
}

/// `Some((id, timestamp))` when `v` is a prompt that starts a turn.
fn prompt(format: SessionFormat, v: &Value) -> Option<(Option<String>, Option<String>)> {
    let ts = str_at(v, "timestamp").map(str::to_string);
    let starts = match format {
        SessionFormat::Claude => {
            str_at(v, "type") == Some("user")
                && v.get("isMeta").and_then(Value::as_bool) != Some(true)
                && match v.pointer("/message/content") {
                    Some(Value::String(_)) => true,
                    Some(Value::Array(parts)) => !parts
                        .iter()
                        .any(|p| str_at(p, "type") == Some("tool_result")),
                    _ => false,
                }
        }
        SessionFormat::Codex => {
            str_at(v, "type") == Some("event_msg")
                && v.pointer("/payload/type").and_then(Value::as_str) == Some("user_message")
        }
        SessionFormat::Pi => {
            str_at(v, "type") == Some("message")
                && v.pointer("/message/role").and_then(Value::as_str) == Some("user")
        }
    };
    let id = str_at(v, "uuid").or_else(|| str_at(v, "id"));
    starts.then(|| (id.map(str::to_string), ts))
}

fn codex_model(v: &Value) -> Option<&str> {
    (str_at(v, "type") == Some("turn_context"))
        .then(|| v.pointer("/payload/model").and_then(Value::as_str))
        .flatten()
}

struct Builder {
    id: String,
    at: Option<String>,
    continued: bool,
    /// Calls by id, so a streamed call counts once.
    calls: BTreeMap<String, ModelUsage>,
    anon: u32,
    /// Codex: the last running total seen.
    codex_total: Option<u64>,
}

impl Builder {
    fn new(id: String, at: Option<String>, continued: bool) -> Self {
        Builder {
            id,
            at,
            continued,
            calls: BTreeMap::new(),
            anon: 0,
            codex_total: None,
        }
    }

    fn call(&mut self, id: Option<&str>, usage: ModelUsage) {
        let id = match id {
            Some(id) => id.to_string(),
            None => {
                self.anon += 1;
                format!("\0{}", self.anon)
            }
        };
        self.calls.insert(id, usage);
    }

    fn add(&mut self, format: SessionFormat, v: &Value, model: Option<&str>) {
        match format {
            SessionFormat::Claude => {
                if str_at(v, "type") != Some("assistant") {
                    return;
                }
                let Some(msg) = v.get("message") else { return };
                let Some(u) = msg.get("usage") else { return };
                let name = str_at(msg, "model").unwrap_or_default();
                // Claude Code's own error entries: no model ran.
                if name == "<synthetic>" {
                    return;
                }
                let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                let (read, write) = (
                    n("cache_read_input_tokens"),
                    n("cache_creation_input_tokens"),
                );
                self.call(
                    str_at(msg, "id"),
                    ModelUsage {
                        model: name.to_string(),
                        input: n("input_tokens") + read + write,
                        output: n("output_tokens"),
                        cache_read: read,
                        cache_write: write,
                        calls: 1,
                        cost_usd: None,
                    },
                );
            }
            SessionFormat::Codex => {
                if str_at(v, "type") != Some("event_msg")
                    || v.pointer("/payload/type").and_then(Value::as_str) != Some("token_count")
                {
                    return;
                }
                let Some(info) = v.pointer("/payload/info").filter(|i| !i.is_null()) else {
                    return;
                };
                let total = info
                    .pointer("/total_token_usage/total_tokens")
                    .and_then(Value::as_u64);
                if total.is_some() && total == self.codex_total {
                    return;
                }
                self.codex_total = total;
                let Some(u) = info.get("last_token_usage") else {
                    return;
                };
                let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                self.call(
                    None,
                    ModelUsage {
                        model: model.unwrap_or_default().to_string(),
                        input: n("input_tokens"),
                        output: n("output_tokens"),
                        cache_read: n("cached_input_tokens"),
                        cache_write: 0,
                        calls: 1,
                        cost_usd: None,
                    },
                );
            }
            SessionFormat::Pi => {
                if str_at(v, "type") != Some("message") {
                    return;
                }
                let Some(msg) = v.get("message") else { return };
                if str_at(msg, "role") != Some("assistant") {
                    return;
                }
                let Some(u) = msg.get("usage") else { return };
                let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                let (read, write) = (n("cacheRead"), n("cacheWrite"));
                self.call(
                    str_at(v, "id"),
                    ModelUsage {
                        model: str_at(msg, "model").unwrap_or_default().to_string(),
                        input: n("input") + read + write,
                        output: n("output"),
                        cache_read: read,
                        cache_write: write,
                        calls: 1,
                        cost_usd: u.pointer("/cost/total").and_then(Value::as_f64),
                    },
                );
            }
        }
    }

    /// The turn, unless no model ran in it.
    fn finish(self) -> Option<TurnUsage> {
        let mut by_model: BTreeMap<String, ModelUsage> = BTreeMap::new();
        for c in self.calls.into_values() {
            let m = by_model
                .entry(c.model.clone())
                .or_insert_with(|| ModelUsage {
                    model: c.model.clone(),
                    ..Default::default()
                });
            m.add(&c);
        }
        (!by_model.is_empty()).then(|| TurnUsage {
            id: self.id,
            at: self.at,
            continued: self.continued,
            models: by_model.into_values().collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use serde_json::json;

    use super::*;

    fn write(path: &Path, lines: &[Value]) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    fn claude_user(id: &str, content: Value) -> Value {
        json!({"type": "user", "uuid": id, "timestamp": "2026-10-07T10:00:00Z",
            "message": {"role": "user", "content": content}})
    }

    fn claude_call(
        id: &str,
        model: &str,
        (input, read, write, output): (u64, u64, u64, u64),
    ) -> Value {
        json!({"type": "assistant", "message": {"id": id, "model": model, "role": "assistant",
            "content": [{"type": "text", "text": "x"}],
            "usage": {"input_tokens": input, "cache_read_input_tokens": read,
                "cache_creation_input_tokens": write, "output_tokens": output}}})
    }

    #[test]
    fn claude_turns_count_each_call_once_and_resume_at_the_open_turn() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        write(
            &path,
            &[
                claude_user("u1", json!("do it")),
                claude_call("m1", "claude-opus-5-5", (10, 100, 20, 1)),
                // Streamed again with its final output count.
                claude_call("m1", "claude-opus-5-5", (10, 100, 20, 5)),
                claude_user("r1", json!([{"type": "tool_result", "content": "ok"}])),
                claude_call("m2", "claude-haiku-4-5", (3, 0, 0, 2)),
                json!({"type": "assistant", "message": {"id": "e", "model": "<synthetic>",
                    "usage": {"input_tokens": 0, "output_tokens": 0}}}),
                claude_user("u2", json!("next")),
                claude_call("m3", "claude-opus-5-5", (1, 0, 0, 1)),
            ],
        );
        let (turns, cur) =
            read_turns(&path, &UsageCursor::default(), SessionFormat::Claude, false).unwrap();
        assert_eq!(turns.len(), 1);
        let t = &turns[0];
        assert_eq!((t.id.as_str(), t.continued), ("u1", false));
        assert_eq!(t.at.as_deref(), Some("2026-10-07T10:00:00Z"));
        assert_eq!(t.models.len(), 2);
        let opus = t
            .models
            .iter()
            .find(|m| m.model == "claude-opus-5-5")
            .unwrap();
        assert_eq!(
            (
                opus.input,
                opus.output,
                opus.cache_read,
                opus.cache_write,
                opus.calls
            ),
            (130, 5, 100, 20, 1)
        );
        assert_eq!(cur.turn, None);

        // The open turn is read again and stays open until closed.
        let (again, same) = read_turns(&path, &cur, SessionFormat::Claude, false).unwrap();
        assert!(again.is_empty());
        assert_eq!(same, cur);
        let (closed, after) = read_turns(&path, &cur, SessionFormat::Claude, true).unwrap();
        assert_eq!(closed[0].id, "u2");
        assert_eq!(after.turn.as_deref(), Some("u2"));

        // Calls written after an early close continue the same turn.
        write(&path, &[claude_call("m4", "claude-opus-5-5", (2, 0, 0, 2))]);
        let (more, _) = read_turns(&path, &after, SessionFormat::Claude, true).unwrap();
        assert_eq!((more[0].id.as_str(), more[0].continued), ("u2", true));
        assert_eq!(more[0].models[0].input, 2);
    }

    #[test]
    fn codex_reads_last_usage_under_the_turn_model_and_skips_repeats() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        let count = |total: u64, input: u64| {
            json!({"type": "event_msg", "payload": {"type": "token_count", "info": {
                "total_token_usage": {"total_tokens": total},
                "last_token_usage": {"input_tokens": input, "cached_input_tokens": 4,
                    "output_tokens": 3, "total_tokens": input + 3}}}})
        };
        write(
            &path,
            &[
                json!({"type": "turn_context", "payload": {"model": "gpt-5-codex"}}),
                json!({"timestamp": "t1", "type": "event_msg", "payload": {"type": "user_message", "message": "go"}}),
                count(13, 10),
                count(13, 10),
                json!({"type": "event_msg", "payload": {"type": "token_count", "info": null}}),
                count(33, 17),
                json!({"type": "event_msg", "payload": {"type": "user_message", "message": "again"}}),
            ],
        );
        let (turns, cur) =
            read_turns(&path, &UsageCursor::default(), SessionFormat::Codex, false).unwrap();
        assert_eq!(turns.len(), 1);
        let m = &turns[0].models[0];
        assert_eq!(m.model, "gpt-5-codex");
        assert_eq!((m.input, m.output, m.cache_read, m.calls), (27, 6, 8, 2));
        assert_eq!(cur.model.as_deref(), Some("gpt-5-codex"));
    }

    #[test]
    fn pi_keeps_its_own_cost() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pi.jsonl");
        write(
            &path,
            &[
                json!({"type": "message", "id": "a", "message": {"role": "user", "content": "hi"}}),
                json!({"type": "message", "id": "b", "message": {"role": "assistant", "model": "claude-sonnet-5-5",
                    "usage": {"input": 5, "output": 7, "cacheRead": 10, "cacheWrite": 1,
                        "cost": {"total": 0.25}}}}),
            ],
        );
        let (turns, _) =
            read_turns(&path, &UsageCursor::default(), SessionFormat::Pi, true).unwrap();
        let m = &turns[0].models[0];
        assert_eq!(
            (m.input, m.output, m.cache_read, m.cache_write),
            (16, 7, 10, 1)
        );
        assert_eq!(m.cost_usd, Some(0.25));
    }
}
