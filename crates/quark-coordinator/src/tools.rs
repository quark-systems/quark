//! The coordinator's native tools.
//!
//! The coordinator acts only through these. They cover judgment: starting
//! work and writing its instructions, steering and answering workers,
//! choosing an agent when dispatch asks, and deciding what reaches the user.
//! Lifecycle steps the engine owns (gates, rebase, merge, teardown,
//! recovery) are deliberately absent.
//!
//! [`Hands`] carries a call out; quarkd implements it over the dispatcher,
//! the supervisor, the channels and Project memory. `load_skill` is answered
//! by the coordinator itself from the Project's prompt.

use async_trait::async_trait;
use quark_core::{CoreError, ProjectId, Result, TaskId};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// An agent choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub harness: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

/// One tool call, as recorded in the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolCall {
    /// Every task with its state.
    Fleet {},
    /// One task: its instructions, state and recent worker messages.
    Task { task: TaskId },
    /// A skill's full text.
    LoadSkill { name: String },
    /// Start a task. Dispatch picks the agent unless `agent` is given.
    StartTask {
        title: String,
        brief: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repo: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<Agent>,
    },
    /// Send a running worker a message.
    Steer { task: TaskId, text: String },
    /// Answer a worker's question.
    Answer {
        task: TaskId,
        key: String,
        answer: String,
    },
    /// Pick the agent for a task dispatch escalated.
    ChooseProfile { task: TaskId, agent: Agent },
    /// Replace a task's worker in the same worktree.
    Relaunch {
        task: TaskId,
        note: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<Agent>,
    },
    /// Stop a task. Its worktree and any unlanded work are kept.
    Cancel { task: TaskId, reason: String },
    /// Raise a decision only the user can make.
    AskUser {
        key: String,
        question: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        options: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recommended: Option<String>,
    },
    /// Tell the user an outcome.
    TellUser { text: String },
    /// Keep a fact in Project memory (as a proposal the user reviews).
    Remember { fact: String },
    /// Mark an inbound message handled.
    AckMessage { channel: String, id: String },
}

impl ToolCall {
    pub fn name(&self) -> &'static str {
        match self {
            ToolCall::Fleet {} => "fleet",
            ToolCall::Task { .. } => "task",
            ToolCall::LoadSkill { .. } => "load_skill",
            ToolCall::StartTask { .. } => "start_task",
            ToolCall::Steer { .. } => "steer",
            ToolCall::Answer { .. } => "answer",
            ToolCall::ChooseProfile { .. } => "choose_profile",
            ToolCall::Relaunch { .. } => "relaunch",
            ToolCall::Cancel { .. } => "cancel",
            ToolCall::AskUser { .. } => "ask_user",
            ToolCall::TellUser { .. } => "tell_user",
            ToolCall::Remember { .. } => "remember",
            ToolCall::AckMessage { .. } => "ack_message",
        }
    }

    /// Whether the call changes anything. A turn with no changing call is a
    /// status acknowledgement in the efficiency figures.
    pub fn acts(&self) -> bool {
        !matches!(
            self,
            ToolCall::Fleet {} | ToolCall::Task { .. } | ToolCall::LoadSkill { .. }
        )
    }

    /// Parse an MCP `tools/call` by name and arguments. Text fields must
    /// not be blank.
    pub fn parse(name: &str, args: &Value) -> std::result::Result<ToolCall, String> {
        if !catalog().iter().any(|t| t.name == name) {
            return Err(format!("unknown tool {name}"));
        }
        let mut obj = match args {
            Value::Object(m) => m.clone(),
            Value::Null => Default::default(),
            _ => return Err("arguments must be an object".into()),
        };
        obj.insert("tool".into(), Value::String(name.into()));
        let call: ToolCall =
            serde_json::from_value(Value::Object(obj)).map_err(|e| e.to_string())?;
        call.check()?;
        Ok(call)
    }

    fn check(&self) -> std::result::Result<(), String> {
        let blank = |field: &str, v: &str| {
            if v.trim().is_empty() {
                Err(format!("`{field}` must be non-empty text"))
            } else {
                Ok(())
            }
        };
        match self {
            ToolCall::Fleet {} => Ok(()),
            ToolCall::Task { task } | ToolCall::ChooseProfile { task, .. } => {
                blank("task", task.as_str())
            }
            ToolCall::LoadSkill { name } => blank("name", name),
            ToolCall::StartTask { title, brief, .. } => {
                blank("title", title)?;
                blank("brief", brief)
            }
            ToolCall::Steer { task, text } => {
                blank("task", task.as_str())?;
                blank("text", text)
            }
            ToolCall::Answer { task, key, answer } => {
                blank("task", task.as_str())?;
                blank("key", key)?;
                blank("answer", answer)
            }
            ToolCall::Relaunch { task, note, .. } => {
                blank("task", task.as_str())?;
                blank("note", note)
            }
            ToolCall::Cancel { task, reason } => {
                blank("task", task.as_str())?;
                blank("reason", reason)
            }
            ToolCall::AskUser { key, question, .. } => {
                blank("key", key)?;
                blank("question", question)
            }
            ToolCall::TellUser { text } => blank("text", text),
            ToolCall::Remember { fact } => blank("fact", fact),
            ToolCall::AckMessage { channel, id } => {
                blank("channel", channel)?;
                blank("id", id)
            }
        }
    }
}

/// A tool as listed to the coordinator.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

fn agent_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "harness": {"type": "string", "description": "Harness id, such as claude or codex."},
            "model": {"type": "string"},
            "effort": {"type": "string"}
        },
        "required": ["harness"],
        "additionalProperties": false
    })
}

fn object(props: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": props,
        "required": required,
        "additionalProperties": false
    })
}

/// Every tool, in the order the coordinator sees them.
pub fn catalog() -> Vec<ToolSpec> {
    let text = |d: &str| json!({"type": "string", "description": d});
    vec![
        ToolSpec {
            name: "start_task",
            description: "Start a task. Write self-contained instructions: the user's ask in their words, scope, and what done looks like. Dispatch picks the agent unless you give one.",
            input_schema: object(
                json!({
                    "title": text("A few words naming the task."),
                    "brief": text("The worker's instructions."),
                    "repo": text("The Project repo to work in, when there is more than one."),
                    "agent": agent_schema()
                }),
                &["title", "brief"],
            ),
        },
        ToolSpec {
            name: "steer",
            description: "Send a running worker a message. It is delivered even if the worker is replaced.",
            input_schema: object(
                json!({"task": text("Task id."), "text": text("The message.")}),
                &["task", "text"],
            ),
        },
        ToolSpec {
            name: "answer",
            description: "Answer a worker's question by its key. Answer yourself when the user's intent already settles it; otherwise use ask_user.",
            input_schema: object(
                json!({"task": text("Task id."), "key": text("The question's key."), "answer": text("Your answer.")}),
                &["task", "key", "answer"],
            ),
        },
        ToolSpec {
            name: "choose_profile",
            description: "Pick the agent for a task whose dispatch could not decide.",
            input_schema: object(
                json!({"task": text("Task id."), "agent": agent_schema()}),
                &["task", "agent"],
            ),
        },
        ToolSpec {
            name: "relaunch",
            description: "Replace a task's worker in the same worktree, optionally on another agent. The note tells the new worker where things stand.",
            input_schema: object(
                json!({"task": text("Task id."), "note": text("Where things stand."), "agent": agent_schema()}),
                &["task", "note"],
            ),
        },
        ToolSpec {
            name: "cancel",
            description: "Stop a task. Its worktree and any unlanded work are kept.",
            input_schema: object(
                json!({"task": text("Task id."), "reason": text("Why.")}),
                &["task", "reason"],
            ),
        },
        ToolSpec {
            name: "ask_user",
            description: "Raise a decision only the user can make: a design choice, anything destructive, irreversible or security-sensitive, a credential. Ask once with options and your recommendation.",
            input_schema: object(
                json!({
                    "key": text("Stable key; asking again with it updates the same decision."),
                    "question": text("The question, standing alone."),
                    "options": {"type": "array", "items": {"type": "string"}},
                    "recommended": text("The option you recommend.")
                }),
                &["key", "question"],
            ),
        },
        ToolSpec {
            name: "tell_user",
            description: "Tell the user an outcome they need: finished work with its link, findings, a real blocker. Lead with the outcome.",
            input_schema: object(json!({"text": text("The message.")}), &["text"]),
        },
        ToolSpec {
            name: "remember",
            description: "Propose a fact for Project memory.",
            input_schema: object(json!({"fact": text("The fact, standing alone.")}), &["fact"]),
        },
        ToolSpec {
            name: "ack_message",
            description: "Mark an inbound message handled.",
            input_schema: object(
                json!({"channel": text("The channel, such as inbox."), "id": text("The message id.")}),
                &["channel", "id"],
            ),
        },
        ToolSpec {
            name: "load_skill",
            description: "Load a skill listed in your instructions by name.",
            input_schema: object(json!({"name": text("The skill's name.")}), &["name"]),
        },
        ToolSpec {
            name: "fleet",
            description: "Every task with its state.",
            input_schema: object(json!({}), &[]),
        },
        ToolSpec {
            name: "task",
            description: "One task: its instructions, state and recent worker messages.",
            input_schema: object(json!({"task": text("Task id.")}), &["task"]),
        },
    ]
}

/// Carries out the coordinator's calls. quarkd implements it.
#[async_trait]
pub trait Hands: Send + Sync {
    /// Run `call` for `project` and say what happened, in a line or a short
    /// block the coordinator reads. A refusal is an error.
    async fn run(&self, project: &ProjectId, call: &ToolCall) -> Result<String>;
}

/// Hands that refuse everything, for shadow mode.
pub struct NoHands;

#[async_trait]
impl Hands for NoHands {
    async fn run(&self, _: &ProjectId, _: &ToolCall) -> Result<String> {
        Err(CoreError::Unsupported("shadow mode does not act".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_tool_parses_and_round_trips() {
        let args = |name: &str| match name {
            "start_task" => json!({"title": "t", "brief": "b", "agent": {"harness": "claude"}}),
            "steer" => json!({"task": "t1", "text": "x"}),
            "answer" => json!({"task": "t1", "key": "k", "answer": "a"}),
            "choose_profile" => {
                json!({"task": "t1", "agent": {"harness": "codex", "effort": "high"}})
            }
            "relaunch" => json!({"task": "t1", "note": "n"}),
            "cancel" => json!({"task": "t1", "reason": "r"}),
            "ask_user" => json!({"key": "k", "question": "q", "options": ["a", "b"]}),
            "tell_user" => json!({"text": "x"}),
            "remember" => json!({"fact": "f"}),
            "ack_message" => json!({"channel": "inbox", "id": "1"}),
            "load_skill" => json!({"name": "s"}),
            "fleet" => json!({}),
            "task" => json!({"task": "t1"}),
            other => panic!("no sample for {other}"),
        };
        for spec in catalog() {
            let call = ToolCall::parse(spec.name, &args(spec.name)).unwrap();
            assert_eq!(call.name(), spec.name);
            let v = serde_json::to_value(&call).unwrap();
            assert_eq!(serde_json::from_value::<ToolCall>(v).unwrap(), call);
        }
    }

    #[test]
    fn reads_do_not_act() {
        assert!(!ToolCall::Fleet {}.acts());
        assert!(!ToolCall::LoadSkill { name: "x".into() }.acts());
        assert!(ToolCall::TellUser { text: "x".into() }.acts());
    }

    #[test]
    fn bad_calls_are_refused() {
        assert!(ToolCall::parse("merge", &json!({})).is_err());
        assert!(ToolCall::parse("steer", &json!({"task": "t1", "text": "  "})).is_err());
        assert!(ToolCall::parse("steer", &json!({"task": "t1"})).is_err());
        assert!(ToolCall::parse("fleet", &json!({"extra": 1})).is_err());
        assert!(ToolCall::parse("fleet", &Value::Null).is_ok());
    }
}
