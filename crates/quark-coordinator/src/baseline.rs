//! Firstmate's coordinator, measured from its transcript.
//!
//! The native coordinator's token target is set against today's: how many
//! turns firstmate's coordinator takes per task, what they cost, and how
//! many only acknowledge status. This module reads a Claude Code session
//! log (JSONL) and cuts it into turns.
//!
//! - A turn starts at a user entry that is not a tool result, and runs to
//!   the next one.
//! - Its usage is the sum over its model calls; streamed entries of one
//!   call share a message id and are counted once.
//! - Its cause is `wake` when the prompt is a supervision notification
//!   (a finished background watcher, or a wake reason line), else `user`.
//! - It **acts** when the user started it, or when it edited a file or ran
//!   one of firstmate's acting scripts (spawn, steer, control, merge, hold,
//!   brief, backlog changes). A wake turn that did none of that only
//!   acknowledged status. This is a heuristic over the transcript; the
//!   native side counts acting tool calls exactly.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::events::Usage;

/// What started a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    /// The user wrote.
    User,
    /// The supervision machinery woke the coordinator.
    Wake,
}

/// One firstmate coordinator turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineTurn {
    /// The transcript entry id the turn started at.
    pub id: String,
    /// When it started, as the transcript says.
    pub at: String,
    pub cause: Cause,
    pub usage: Usage,
    pub tool_calls: u32,
    pub acts: bool,
}

impl BaselineTurn {
    /// A wake turn that changed nothing.
    pub fn is_ack(&self) -> bool {
        self.cause == Cause::Wake && !self.acts
    }
}

/// firstmate scripts whose use is a judgment act, not a status read.
const ACTING_SCRIPTS: &[&str] = &[
    "fm-spawn",
    "fm-send",
    "fm-control",
    "fm-teardown",
    "fm-pr-merge",
    "fm-merge-local",
    "fm-captain-hold",
    "fm-promote",
    "fm-brief",
    "fm-pr-check",
    "fm-project-add",
    "fm-check-register",
    "gh pr merge",
];
/// `fm-tasks-axi.sh` subcommands that change the backlog.
const BACKLOG_CHANGES: &[&str] = &["add", "edit", "done", "move", "rm", "hold", "note"];
const EDIT_TOOLS: &[&str] = &["Write", "Edit", "MultiEdit", "NotebookEdit"];

/// Turns completed in `path` from byte `offset` on, and the offset to
/// resume from: the start of the turn still open at the end (it is read
/// again next time, once it has ended).
pub fn read_turns(path: &Path, offset: u64) -> std::io::Result<(Vec<BaselineTurn>, u64)> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let offset = if offset > len { 0 } else { offset };
    f.seek(SeekFrom::Start(offset))?;
    let mut reader = BufReader::new(f.take(len - offset));
    let mut turns = Vec::new();
    let mut open: Option<(u64, Builder)> = None;
    let mut pos = offset;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 || !line.ends_with('\n') {
            break; // end, or a line still being written
        }
        let at = pos;
        pos += n as u64;
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(cause) = prompt_cause(&entry) {
            if let Some((_, b)) = open.take() {
                turns.push(b.finish());
            }
            open = Some((at, Builder::new(&entry, cause)));
        } else if let Some((_, b)) = open.as_mut() {
            b.add(&entry);
        }
    }
    let resume = open.map(|(at, _)| at).unwrap_or(pos);
    Ok((turns, resume))
}

/// `Some(cause)` when `entry` starts a turn.
fn prompt_cause(entry: &Value) -> Option<Cause> {
    if entry.get("type").and_then(Value::as_str) != Some("user") {
        return None;
    }
    if entry.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let content = entry.pointer("/message/content")?;
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => {
            if parts
                .iter()
                .any(|p| p.get("type").and_then(Value::as_str) == Some("tool_result"))
            {
                return None;
            }
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        }
        _ => return None,
    };
    let t = text.trim_start();
    let wake = t.starts_with("<task-notification>")
        || t.contains("<task-notification>")
        || ["signal:", "stale:", "check:", "heartbeat", "WAKE"]
            .iter()
            .any(|p| t.starts_with(p))
        || t.contains("FIRSTMATE_OP:");
    Some(if wake { Cause::Wake } else { Cause::User })
}

struct Builder {
    turn: BaselineTurn,
    calls: std::collections::BTreeMap<String, Usage>,
    tool_ids: std::collections::BTreeSet<String>,
}

impl Builder {
    fn new(entry: &Value, cause: Cause) -> Self {
        let s = |k: &str| {
            entry
                .get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        Builder {
            turn: BaselineTurn {
                id: s("uuid"),
                at: s("timestamp"),
                cause,
                usage: Usage::default(),
                tool_calls: 0,
                acts: cause == Cause::User,
            },
            calls: Default::default(),
            tool_ids: Default::default(),
        }
    }

    fn add(&mut self, entry: &Value) {
        if entry.get("type").and_then(Value::as_str) != Some("assistant") {
            return;
        }
        let Some(msg) = entry.get("message") else {
            return;
        };
        if let Some(u) = msg.get("usage") {
            let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
            let read = n("cache_read_input_tokens");
            let usage = Usage {
                input: n("input_tokens") + n("cache_creation_input_tokens") + read,
                output: n("output_tokens"),
                cache_read: read,
                calls: 1,
            };
            let id = msg
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("anon{}", self.calls.len()));
            // Streamed entries of one call repeat its usage; keep the last.
            self.calls.insert(id, usage);
        }
        for part in msg
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if part.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            // A streamed entry may repeat a call already seen.
            if let Some(id) = part.get("id").and_then(Value::as_str) {
                if !self.tool_ids.insert(id.to_string()) {
                    continue;
                }
            }
            self.turn.tool_calls += 1;
            let name = part.get("name").and_then(Value::as_str).unwrap_or("");
            if EDIT_TOOLS.contains(&name) {
                self.turn.acts = true;
            }
            if name == "Bash" {
                let cmd = part
                    .pointer("/input/command")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if command_acts(cmd) {
                    self.turn.acts = true;
                }
            }
        }
    }

    fn finish(mut self) -> BaselineTurn {
        for u in self.calls.values() {
            self.turn.usage.add(u);
        }
        self.turn
    }
}

fn command_acts(cmd: &str) -> bool {
    if ACTING_SCRIPTS.iter().any(|s| cmd.contains(s)) {
        return true;
    }
    cmd.split("fm-tasks-axi.sh").skip(1).any(|rest| {
        rest.split_whitespace()
            .next()
            .is_some_and(|sub| BACKLOG_CHANGES.contains(&sub))
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use serde_json::json;

    use super::*;

    fn line(v: Value) -> String {
        format!("{v}\n")
    }

    fn user(id: &str, content: Value) -> String {
        line(
            json!({"type": "user", "uuid": id, "timestamp": "2026-10-07T01:00:00Z", "message": {"role": "user", "content": content}}),
        )
    }

    fn assistant(msg: &str, usage: (u64, u64, u64), content: Value) -> String {
        line(
            json!({"type": "assistant", "message": {"id": msg, "role": "assistant", "content": content,
            "usage": {"input_tokens": usage.0, "cache_read_input_tokens": usage.1, "output_tokens": usage.2}}}),
        )
    }

    fn bash(cmd: &str) -> Value {
        json!([{"type": "tool_use", "id": format!("tu-{cmd}"), "name": "Bash", "input": {"command": cmd}}])
    }

    #[test]
    fn cuts_turns_and_classifies_acks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut text = String::new();
        // A wake that only re-arms the watcher: an ack.
        text += &user(
            "u1",
            json!("<task-notification>\n<status>completed</status>"),
        );
        text += &assistant("m1", (10, 100, 5), bash("bin/fm-watch.sh --arm"));
        text += &assistant("m1", (10, 100, 5), bash("bin/fm-watch.sh --arm"));
        text += &user("r1", json!([{"type": "tool_result", "content": "ok"}]));
        text += &assistant(
            "m2",
            (3, 120, 7),
            json!([{"type": "text", "text": "Captain, shipshape."}]),
        );
        // A wake that steers a worker: acts.
        text += &user("u2", json!("signal: state/t1.status"));
        text += &assistant("m3", (1, 0, 1), bash("bin/fm-send.sh t1 'rebase please'"));
        // The user writes: always acts. Still open at the end.
        text += &user("u3", json!([{"type": "text", "text": "ship it"}]));
        text += &assistant("m4", (1, 0, 1), json!([{"type": "text", "text": "ok"}]));
        std::fs::write(&path, &text).unwrap();

        let (turns, resume) = read_turns(&path, 0).unwrap();
        assert_eq!(turns.len(), 2);
        let ack = &turns[0];
        assert_eq!(ack.id, "u1");
        assert_eq!(ack.cause, Cause::Wake);
        assert!(ack.is_ack());
        assert_eq!(ack.usage.calls, 2);
        assert_eq!(ack.usage.input, 110 + 123);
        assert_eq!(ack.usage.output, 12);
        assert_eq!(ack.tool_calls, 1);
        assert!(turns[1].acts);

        // The open turn closes once another starts, and is read once.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(user("u4", json!("check: pr merged")).as_bytes())
            .unwrap();
        f.write_all(b"{\"type\": \"assistant\", \"partial").unwrap();
        let (more, resume2) = read_turns(&path, resume).unwrap();
        assert_eq!(more.len(), 1);
        assert_eq!(more[0].id, "u3");
        assert_eq!(more[0].cause, Cause::User);
        assert!(!more[0].is_ack());
        let (none, _) = read_turns(&path, resume2).unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn backlog_changes_and_edits_act() {
        assert!(command_acts("bin/fm-tasks-axi.sh add 'x'"));
        assert!(!command_acts("bin/fm-tasks-axi.sh list"));
        assert!(!command_acts("bin/fm-crew-state.sh t1"));
        let mut b = Builder::new(&json!({"uuid": "u"}), Cause::Wake);
        b.add(
            &serde_json::from_str(&assistant(
                "m",
                (1, 0, 1),
                json!([{"type": "tool_use", "name": "Write", "input": {}}]),
            ))
            .unwrap(),
        );
        assert!(b.finish().acts);
    }
}
