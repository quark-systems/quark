//! The coordinator's native tools.
//!
//! The coordinator acts only through these. They cover judgment: starting
//! work and writing its instructions, steering and answering workers,
//! choosing an agent when dispatch asks, and deciding what reaches the user.
//! Lifecycle steps the engine owns (gates, rebase, merge, recovery) are
//! deliberately absent; `clear` only tidies up after a task has finished.
//!
//! [`Hands`] carries a call out; quarkd implements it over the dispatcher,
//! the supervisor, the channels and Project memory. `load_skill` is answered
//! by the coordinator itself from the Project's prompt.

use async_trait::async_trait;
use quark_core::{CoreError, ProjectId, Result, TaskId};
use quark_systems::{DecisionBrief, DecisionOption, EvidenceLink};
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

/// A worker's question, by its task and key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerQuestion {
    pub task: TaskId,
    pub key: String,
}

impl WorkerQuestion {
    /// The engine's id for the question, as `answer` addresses it.
    pub fn hold(&self) -> String {
        format!("{}:{}", self.task, self.key)
    }
}

/// One tool call, as recorded in the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolCall {
    /// Every task with its state.
    Fleet {},
    /// One task: its instructions, state and recent worker messages.
    Task { task: TaskId },
    /// What a running worker's terminal shows now.
    Screen { task: TaskId },
    /// A skill's full text.
    LoadSkill { name: String },
    /// Start a task. Dispatch picks the agent from the Project's rules;
    /// `rule` is the dispatch rule the coordinator matched it to, resolved
    /// in place of a classifier's answer. `agent` stays only so calls
    /// logged before it was refused still read: [`ToolCall::parse`] refuses
    /// it, because a coordinator naming an agent here skipped the rules on
    /// every task.
    StartTask {
        title: String,
        brief: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repo: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<Agent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rule: Option<String>,
    },
    /// Send a running worker a message.
    Steer {
        task: TaskId,
        text: String,
        /// Interrupt the worker's turn first, so the message is read now.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        interrupt: bool,
    },
    /// Answer a worker's question.
    Answer {
        task: TaskId,
        key: String,
        answer: String,
    },
    /// Pick the agent for a task dispatch escalated: the dispatch rule it
    /// fits (`rule`), whose profile the rules then select, or an agent.
    ChooseProfile {
        task: TaskId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<Agent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rule: Option<String>,
    },
    /// Replace a task's worker in the same worktree.
    Relaunch {
        task: TaskId,
        note: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<Agent>,
    },
    /// Stop a task. Its worktree and any unlanded work are kept.
    Cancel { task: TaskId, reason: String },
    /// Clean up a done or failed task: stop its session, give its worktree
    /// back and take it off the board, keeping its history. A worktree with
    /// unlanded work is kept and the task stays, unless `discard`, which
    /// throws away uncommitted changes (commits stay on the branch).
    Clear {
        task: TaskId,
        reason: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        discard: bool,
    },
    /// Raise a decision only the user can make. Everything but `key` and
    /// `question` is the decision's brief ([`ToolCall::brief`]), the same
    /// record firstmate's coordinator attaches to a captain hold.
    AskUser {
        key: String,
        question: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<String>,
        /// Labels, or `{label, consequence}` objects.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        options: Vec<DecisionOption>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recommended: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recommended_why: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        blocks: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        evidence: Vec<EvidenceLink>,
        /// The worker's question this frames for the user: the user's
        /// answer goes to that worker.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answers: Option<WorkerQuestion>,
    },
    /// Withdraw an open decision the coordinator asked, which no longer
    /// needs the user.
    WithdrawDecision { number: i64, reason: String },
    /// Retired: the coordinator's reply already shows in the user's chat,
    /// so a second copy doubled every message. Calls logged before still
    /// read; a new call is refused with [`TELL_USER_RETIRED`].
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
            ToolCall::Screen { .. } => "screen",
            ToolCall::LoadSkill { .. } => "load_skill",
            ToolCall::StartTask { .. } => "start_task",
            ToolCall::Steer { .. } => "steer",
            ToolCall::Answer { .. } => "answer",
            ToolCall::ChooseProfile { .. } => "choose_profile",
            ToolCall::Relaunch { .. } => "relaunch",
            ToolCall::Cancel { .. } => "cancel",
            ToolCall::Clear { .. } => "clear",
            ToolCall::AskUser { .. } => "ask_user",
            ToolCall::WithdrawDecision { .. } => "withdraw_decision",
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
            ToolCall::Fleet {}
                | ToolCall::Task { .. }
                | ToolCall::Screen { .. }
                | ToolCall::LoadSkill { .. }
        )
    }

    /// The decision brief an `ask_user` call carries, asked by the
    /// coordinator; `None` for every other call.
    pub fn brief(&self) -> Option<DecisionBrief> {
        let ToolCall::AskUser {
            context,
            options,
            recommended,
            recommended_why,
            blocks,
            evidence,
            ..
        } = self
        else {
            return None;
        };
        Some(
            DecisionBrief {
                context: context.clone(),
                options: options.clone(),
                recommended: recommended.clone(),
                recommended_why: recommended_why.clone(),
                asked_by: Some("coordinator".into()),
                blocks: blocks.clone(),
                evidence: evidence.clone(),
            }
            .normalized(),
        )
    }

    /// Parse an MCP `tools/call` by name and arguments. Text fields must
    /// not be blank.
    pub fn parse(name: &str, args: &Value) -> std::result::Result<ToolCall, String> {
        if name == "tell_user" {
            return Err(TELL_USER_RETIRED.into());
        }
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
            ToolCall::Task { task } | ToolCall::Screen { task } => blank("task", task.as_str()),
            ToolCall::ChooseProfile { task, agent, rule } => {
                blank("task", task.as_str())?;
                match (agent, rule) {
                    (None, None) => Err("give `rule` or `agent`".into()),
                    (Some(_), Some(_)) => Err("give `rule` or `agent`, not both".into()),
                    (None, Some(r)) => blank("rule", r),
                    (Some(_), None) => Ok(()),
                }
            }
            ToolCall::LoadSkill { name } => blank("name", name),
            ToolCall::StartTask {
                title,
                brief,
                agent,
                ..
            } => {
                blank("title", title)?;
                blank("brief", brief)?;
                match agent {
                    Some(_) => Err(START_TASK_AGENT.into()),
                    None => Ok(()),
                }
            }
            ToolCall::Steer { task, text, .. } => {
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
            ToolCall::Cancel { task, reason } | ToolCall::Clear { task, reason, .. } => {
                blank("task", task.as_str())?;
                blank("reason", reason)
            }
            ToolCall::AskUser {
                key,
                question,
                context,
                options,
                recommended,
                answers,
                ..
            } => {
                blank("key", key)?;
                blank("question", question)?;
                if let Some(a) = answers {
                    blank("answers.task", a.task.as_str())?;
                    blank("answers.key", &a.key)?;
                }
                let labels: Vec<&str> = options
                    .iter()
                    .map(|o| o.label.trim())
                    .filter(|l| !l.is_empty())
                    .collect();
                if labels.len() < 2
                    || context.as_deref().is_none_or(|c| c.trim().is_empty())
                    || !recommended
                        .as_deref()
                        .is_some_and(|r| labels.contains(&r.trim()))
                {
                    return Err(ASK_USER_SHAPE.into());
                }
                Ok(())
            }
            ToolCall::WithdrawDecision { number, reason } => {
                if *number < 1 {
                    return Err("`number` is the decision's number, such as 16 for D-16".into());
                }
                blank("reason", reason)
            }
            ToolCall::TellUser { .. } => Err(TELL_USER_RETIRED.into()),
            ToolCall::Remember { fact } => blank("fact", fact),
            ToolCall::AckMessage { channel, id } => {
                blank("channel", channel)?;
                blank("id", id)
            }
        }
    }
}

/// Why `start_task` refuses an `agent`, and what to do instead.
pub const START_TASK_AGENT: &str =
    "start_task takes no `agent`: dispatch picks the agent from the \
     Project's dispatch rules. Give the rule the task fits as `rule`, or leave it out and dispatch \
     asks you when it needs a pick. Name an agent only when the user asked for one: start the task \
     without it, then give it to choose_profile when dispatch asks, or relaunch the task on it.";

/// Why `ask_user` refuses a bare question, and what it needs.
pub const ASK_USER_SHAPE: &str =
    "ask_user needs a decision the user can answer in one tap: `context` (what they need to know, \
     in plain words), at least two `options` (each a short label with its consequence), and \
     `recommended` naming one of those labels, with `recommended_why`. The app adds \"Something \
     else\" for a written answer, so never ask for free text alone. When you cannot offer choices \
     yet, find out more first (read the task, its screen, the code), or answer the worker \
     yourself.";

/// What a coordinator started before `tell_user` was retired hears when it
/// calls it: its reply text is already the message.
pub const TELL_USER_RETIRED: &str =
    "tell_user is retired: your reply text already shows in the user's chat, so a tell_user \
     message showed twice. Nothing was posted. Say it in your reply, once. Ask any question with \
     ask_user.";

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

fn rule_schema() -> Value {
    json!({
        "type": "string",
        "description": "The Project dispatch rule the task fits: rule_N, the rule's name, or default when no listed rule applies."
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
            description: "Start a task. Write self-contained instructions: the user's ask in their words, scope, and what done looks like. Dispatch picks the agent from the Project's dispatch rules; give `rule` when you already know which rule the task fits (a configured classifier still decides).",
            input_schema: object(
                json!({
                    "title": text("A few words naming the task; for an issue, its id and a short title (`quark-e1c: detect secrets`)."),
                    "brief": text("The worker's instructions."),
                    "repo": text("The Project repo to work in, when there is more than one."),
                    "rule": rule_schema()
                }),
                &["title", "brief"],
            ),
        },
        ToolSpec {
            name: "steer",
            description: "Send a running worker a message. It is delivered even if the worker is replaced, and waits while a prompt is on the worker's screen (typed text would answer it). A worker reads a message sent while it works only at its next safe point, after the command it is running finishes, which can take minutes; set `interrupt` when the message must change what it is doing now. `task` shows whether the worker reacted; one that leaves a steer unanswered may be stuck: read its screen with `screen`, or relaunch it.",
            input_schema: object(
                json!({
                    "task": text("Task id."),
                    "text": text("The message."),
                    "interrupt": {"type": "boolean", "description": "Stop the worker's current turn first (its running command is cancelled), so it reads the message now as its next prompt. Only when waiting for the command to finish would waste it or do harm."}
                }),
                &["task", "text"],
            ),
        },
        ToolSpec {
            name: "answer",
            description: "Answer a worker's question by its key. A worker's question never reaches the user by itself: it waits on you. Answer it yourself whenever the user's intent, the Project's rules or a sensible default settles it; only a real design choice, or anything destructive, irreversible or outside the task, goes to the user, through ask_user with `answers` set to this task and key. Answering closes the question, including one already framed for the user. A question keyed screen-... is a prompt on the worker's screen (a permission or approval request, or a menu): answer with the number of the choice and Quark selects it there. Approve only what the task needs and the user's rules allow; ask the user about anything destructive or outside the task.",
            input_schema: object(
                json!({"task": text("Task id."), "key": text("The question's key."), "answer": text("Your answer.")}),
                &["task", "key", "answer"],
            ),
        },
        ToolSpec {
            name: "choose_profile",
            description: "Pick the agent for a task whose dispatch could not decide. Give the dispatch rule the task fits as `rule`, and the rule's own profiles are selected; give `agent` instead only when the user named one or the rules cannot serve the task.",
            input_schema: object(
                json!({"task": text("Task id."), "rule": rule_schema(), "agent": agent_schema()}),
                &["task"],
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
            name: "clear",
            description: "Clean up a done or failed task once nobody needs it: stops its session, gives its worktree back and takes it off the board, keeping its history. Refused while the task is working, and when its worktree holds work that has not landed unless `discard` is set (a merged task's pre-squash commits do not count). Clearing a cleared task again frees a worktree an earlier clear left leased. Set `discard` only when the user said to throw that work away.",
            input_schema: object(
                json!({
                    "task": text("Task id."),
                    "reason": text("Why."),
                    "discard": {"type": "boolean", "description": "Throw away uncommitted changes and untracked files in its worktree; its commits stay on its branch. Only when the user said so."}
                }),
                &["task", "reason"],
            ),
        },
        ToolSpec {
            name: "ask_user",
            description: "Ask the user something. Every question for the user goes through this tool, one call per question: a design choice, a clarification, a go-ahead, anything destructive, irreversible or security-sensitive, a credential. It files a decision the user answers in the app with one tap. Never ask in your reply text instead. Required: the question standing alone in plain words (never a worker's shorthand or code names), `context`, at least two `options` each with its consequence, `recommended` (one option's label) and `recommended_why`; the app adds \"Something else\" for a written answer. To put a worker's question to the user, set `answers` to its task and key: the decision replaces the worker's question and the user's answer goes straight to the worker. Asking again with the same key, or the same `answers`, rewrites the open decision in place. Without `answers` the answer wakes you.",
            input_schema: object(
                json!({
                    "key": text("Stable key; asking again with it updates the same decision."),
                    "question": text("The question, standing alone."),
                    "context": text("What the user needs to know to answer, in a few sentences. Name each issue by its full id with a short title, never a bare or shortened id."),
                    "options": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "label": text("The option, in a few words that say what it does; never a list of issue ids."),
                                "consequence": text("What happens when it is picked.")
                            },
                            "required": ["label"]
                        }
                    },
                    "recommended": text("The label of the option you recommend."),
                    "recommended_why": text("Why you recommend it."),
                    "blocks": {"type": "array", "items": text("A pull request URL or task id waiting on the answer.")},
                    "answers": {
                        "type": "object",
                        "description": "The worker's question this decision puts to the user; the user's answer goes to that worker.",
                        "properties": {"task": text("Task id."), "key": text("The question's key.")},
                        "required": ["task", "key"],
                        "additionalProperties": false
                    },
                    "evidence": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {"label": text("What it shows."), "url": text("Where it is.")},
                            "required": ["label"]
                        }
                    }
                }),
                &["key", "question", "context", "options", "recommended", "recommended_why"],
            ),
        },
        ToolSpec {
            name: "withdraw_decision",
            description: "Withdraw an open decision you asked that no longer needs the user: another decision covers it, or the work moved on. It closes in the decision log with your reason. A worker's question is closed by answering the worker (`answer`) instead.",
            input_schema: object(
                json!({"number": {"type": "integer", "description": "The decision's number, such as 16 for D-16."}, "reason": text("Why it no longer needs the user.")}),
                &["number", "reason"],
            ),
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
            description: "Every task with its state. [unhealthy] marks a worker that cannot work until a person acts (its agent's sign-in was refused); [last steer unanswered for ...] marks one that has not reacted to your last steer.",
            input_schema: object(json!({}), &[]),
        },
        ToolSpec {
            name: "screen",
            description: "What a running worker's terminal shows now, as text: what it is doing, or a prompt or error it is stuck on. Use it when a worker seems stuck or does not react to steers.",
            input_schema: object(json!({"task": text("Task id.")}), &["task"]),
        },
        ToolSpec {
            name: "task",
            description: "One task: its instructions, state, recent worker messages, and your steers with whether the worker reacted to each.",
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

/// Hands that refuse everything, for tests.
pub struct NoHands;

#[async_trait]
impl Hands for NoHands {
    async fn run(&self, _: &ProjectId, _: &ToolCall) -> Result<String> {
        Err(CoreError::Unsupported("these hands do not act".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_tool_parses_and_round_trips() {
        let args = |name: &str| match name {
            "start_task" => json!({"title": "t", "brief": "b", "rule": "rule_1"}),
            "steer" => json!({"task": "t1", "text": "x"}),
            "answer" => json!({"task": "t1", "key": "k", "answer": "a"}),
            "choose_profile" => json!({"task": "t1", "rule": "rule_2"}),
            "relaunch" => json!({"task": "t1", "note": "n"}),
            "cancel" => json!({"task": "t1", "reason": "r"}),
            "clear" => json!({"task": "t1", "reason": "r", "discard": true}),
            "ask_user" => json!({
                "key": "k", "question": "q", "context": "c",
                "options": ["a", {"label": "b", "consequence": "c"}],
                "recommended": "a", "recommended_why": "w",
                "answers": {"task": "t1", "key": "schema"}
            }),
            "withdraw_decision" => json!({"number": 3, "reason": "covered by D-4"}),
            "remember" => json!({"fact": "f"}),
            "ack_message" => json!({"channel": "inbox", "id": "1"}),
            "load_skill" => json!({"name": "s"}),
            "fleet" => json!({}),
            "task" | "screen" => json!({"task": "t1"}),
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
        // Naming an agent at the start skipped the dispatch rules.
        assert_eq!(
            ToolCall::parse(
                "start_task",
                &json!({"title": "t", "brief": "b", "agent": {"harness": "claude", "model": "opus"}})
            ),
            Err(START_TASK_AGENT.to_string())
        );
        // A call logged before that still reads.
        let logged = json!({"tool": "start_task", "title": "t", "brief": "b", "agent": {"harness": "claude"}});
        assert!(serde_json::from_value::<ToolCall>(logged).is_ok());
        assert!(ToolCall::parse("fleet", &Value::Null).is_ok());
        // The reply is the message: tell_user only doubled it.
        assert_eq!(
            ToolCall::parse("tell_user", &json!({"text": "Merged."})),
            Err(TELL_USER_RETIRED.to_string())
        );
        assert!(!catalog().iter().any(|t| t.name == "tell_user"));
        // A call logged before it was retired still reads.
        let logged = json!({"tool": "tell_user", "text": "PR #46 merged."});
        assert!(serde_json::from_value::<ToolCall>(logged).is_ok());
    }

    #[test]
    fn ask_user_needs_options_context_and_a_recommendation() {
        let full = json!({
            "key": "k", "question": "q", "context": "c",
            "options": ["a", "b"], "recommended": "a", "recommended_why": "w"
        });
        assert!(ToolCall::parse("ask_user", &full).is_ok());
        for (field, value) in [
            ("options", json!([])),
            ("options", json!(["a"])),
            ("context", json!("  ")),
            ("recommended", json!("c")),
        ] {
            let mut bad = full.clone();
            bad[field] = value;
            assert_eq!(
                ToolCall::parse("ask_user", &bad),
                Err(ASK_USER_SHAPE.to_string()),
                "{field}"
            );
        }
        // A call logged before these checks still reads.
        let logged = json!({"tool": "ask_user", "key": "k", "question": "q"});
        assert!(serde_json::from_value::<ToolCall>(logged).is_ok());
    }
}
