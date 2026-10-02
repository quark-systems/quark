//! Typed engine writes (spec ADR-8).
//!
//! Each [`WriteOp`] names one allowlisted script and renders its own argument
//! vector after validating every field. Caller text only ever lands in a
//! position the script reads as data: the message operand of `fm-send.sh`, or
//! the `--opt=value` form of `fm-control.sh`, whose parser never treats such a
//! value as another option.

use std::time::Duration;

use crate::{validate_task_id, Error, Result};

pub const SEND: &str = "fm-send.sh";
pub const CONTROL: &str = "fm-control.sh";

/// Largest steering message accepted, in bytes.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// Largest relaunch note accepted, in bytes.
pub const MAX_NOTE_BYTES: usize = 16 * 1024;

/// Longest harness, model or effort token accepted.
const MAX_TOKEN_LEN: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOp {
    /// Steer a task through its durable inbox: `fm-send.sh <task> <text>`.
    Send { task_id: String, text: String },
    /// Stop a task's agent, keeping its terminal, worktree and every
    /// uncommitted change: `fm-control.sh <task> exit`.
    Exit { task_id: String },
    /// Replace a task's agent in the same terminal and worktree, optionally on
    /// another harness, model or effort: `fm-control.sh <task> relaunch`.
    Relaunch {
        task_id: String,
        harness: Option<String>,
        model: Option<String>,
        effort: Option<String>,
        /// Carried to the new agent, which inherits the worktree but none of
        /// the conversation.
        note: String,
    },
}

impl WriteOp {
    pub fn script(&self) -> &'static str {
        match self {
            WriteOp::Send { .. } => SEND,
            WriteOp::Exit { .. } | WriteOp::Relaunch { .. } => CONTROL,
        }
    }

    pub fn task_id(&self) -> &str {
        match self {
            WriteOp::Send { task_id, .. }
            | WriteOp::Exit { task_id }
            | WriteOp::Relaunch { task_id, .. } => task_id,
        }
    }

    /// Default bound for the call. `fm-control.sh` waits on verified
    /// postconditions (up to 30s for an exit, 90s more for a launch).
    pub fn timeout(&self) -> Duration {
        match self {
            WriteOp::Send { .. } => Duration::from_secs(60),
            WriteOp::Exit { .. } => Duration::from_secs(120),
            WriteOp::Relaunch { .. } => Duration::from_secs(300),
        }
    }

    /// Validate every field and render the script's argument vector.
    pub fn argv(&self) -> Result<Vec<String>> {
        let script = self.script();
        let invalid = |reason: String| Error::InvalidArgument { script, reason };
        validate_task_id(self.task_id())?;
        // Both scripts read their first operand as the target, but
        // fm-control.sh answers `-h`/`--help` there with usage and exit 0.
        if self.task_id().starts_with('-') {
            return Err(Error::InvalidTaskId(self.task_id().to_string()));
        }
        match self {
            WriteOp::Send { task_id, text } => {
                check_text("message", text, MAX_MESSAGE_BYTES).map_err(invalid)?;
                // fm-send.sh reads a leading `--` as one of its own flags, and
                // types a leading `/` or `$` straight into the harness as a
                // command instead of recording it in the durable inbox.
                let lead = text.trim_start();
                if lead.starts_with("--") || lead.starts_with('/') || lead.starts_with('$') {
                    return Err(invalid(
                        "a message may not start with \"--\", \"/\" or \"$\"".into(),
                    ));
                }
                Ok(vec![task_id.clone(), text.clone()])
            }
            WriteOp::Exit { task_id } => Ok(vec![task_id.clone(), "exit".into()]),
            WriteOp::Relaunch {
                task_id,
                harness,
                model,
                effort,
                note,
            } => {
                let mut argv = vec![task_id.clone(), "relaunch".into()];
                for (flag, value) in [("harness", harness), ("model", model), ("effort", effort)] {
                    if let Some(v) = value {
                        check_token(flag, v).map_err(invalid)?;
                        argv.push(format!("--{flag}={v}"));
                    }
                }
                check_text("note", note, MAX_NOTE_BYTES).map_err(invalid)?;
                argv.push(format!("--note={note}"));
                Ok(argv)
            }
        }
    }
}

fn check_text(what: &str, text: &str, max: usize) -> std::result::Result<(), String> {
    if text.trim().is_empty() {
        return Err(format!("{what} is empty"));
    }
    if text.len() > max {
        return Err(format!(
            "{what} is {} bytes; the limit is {max}",
            text.len()
        ));
    }
    if text.contains('\0') {
        return Err(format!("{what} contains a NUL byte"));
    }
    Ok(())
}

/// Harness, model and effort names: `A-Za-z0-9._:/+-`, not starting with `-`.
fn check_token(what: &str, v: &str) -> std::result::Result<(), String> {
    let ok = !v.is_empty()
        && v.len() <= MAX_TOKEN_LEN
        && !v.starts_with('-')
        && v.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'/' | b'+' | b'-')
        });
    if ok {
        Ok(())
    } else {
        Err(format!("{what} {v:?} is not a valid name"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn send(text: &str) -> WriteOp {
        WriteOp::Send {
            task_id: "t1".into(),
            text: text.into(),
        }
    }

    #[test]
    fn send_renders_task_and_text() {
        assert_eq!(
            send("please add tests\nfor the parser").argv().unwrap(),
            vec!["t1", "please add tests\nfor the parser"]
        );
        // A single leading dash is ordinary text to fm-send.sh.
        assert!(send("- first item").argv().is_ok());
    }

    #[test]
    fn send_refuses_flag_and_command_text() {
        for bad in [
            "",
            "   ",
            "--resolve-key x",
            "--key",
            "/quit",
            "  /compact",
            "$skill",
            "a\0b",
        ] {
            assert!(
                matches!(send(bad).argv(), Err(Error::InvalidArgument { .. })),
                "{bad:?}"
            );
        }
        let big = "x".repeat(MAX_MESSAGE_BYTES + 1);
        assert!(send(&big).argv().is_err());
    }

    #[test]
    fn task_id_is_validated() {
        let op = WriteOp::Exit {
            task_id: "../x".into(),
        };
        assert!(matches!(op.argv(), Err(Error::InvalidTaskId(_))));
        // fm-control.sh would print usage and exit 0 for this "task".
        let op = WriteOp::Exit {
            task_id: "--help".into(),
        };
        assert!(matches!(op.argv(), Err(Error::InvalidTaskId(_))));
    }

    #[test]
    fn relaunch_uses_equals_form() {
        let op = WriteOp::Relaunch {
            task_id: "t1".into(),
            harness: Some("codex".into()),
            model: Some("openai/gpt-5.6".into()),
            effort: None,
            note: "--carry on from the failing test".into(),
        };
        assert_eq!(
            op.argv().unwrap(),
            vec![
                "t1",
                "relaunch",
                "--harness=codex",
                "--model=openai/gpt-5.6",
                "--note=--carry on from the failing test"
            ]
        );
        assert_eq!(op.script(), CONTROL);
    }

    #[test]
    fn relaunch_refuses_bad_tokens_and_empty_note() {
        for harness in ["", "-x", "a b", "a=b", "a;b"] {
            let op = WriteOp::Relaunch {
                task_id: "t1".into(),
                harness: Some(harness.into()),
                model: None,
                effort: None,
                note: "n".into(),
            };
            assert!(op.argv().is_err(), "{harness:?}");
        }
        let op = WriteOp::Relaunch {
            task_id: "t1".into(),
            harness: None,
            model: None,
            effort: None,
            note: " ".into(),
        };
        assert!(op.argv().is_err());
    }
}
