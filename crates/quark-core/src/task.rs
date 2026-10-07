//! The task state machine: a pure function from state and event to state.

use quark_systems::TaskState;
use serde::{Deserialize, Serialize};

use crate::{CoreError, Result};

/// A neutral lifecycle event for one task, carried as the payload of a
/// [`crate::event::kinds::TASK`] event. Every state change is one of these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskEvent {
    /// The task was created and waits for dispatch.
    Queued { title: String },
    /// A worker started (or restarted) on the task.
    Started { generation: String },
    /// The worker asked a question only a person can answer.
    DecisionNeeded { key: String, question: String },
    /// A person answered an open decision.
    DecisionAnswered { key: String },
    /// Firstmate-style `blocked`: the coordinator must act.
    Blocked { reason: String },
    /// A bounded external wait expected to clear on its own.
    Paused { reason: String },
    /// Work resumed after a block or pause.
    Resumed,
    /// A pull request or ready branch is up for review.
    InReview { pull_request: Option<String> },
    /// The work landed (merged, or the report was delivered).
    Completed,
    /// The work failed; the worktree is kept.
    Failed { reason: String },
    /// A person or the coordinator stopped the task.
    Cancelled,
}

/// Turns a task's state and one of its events into the next state.
///
/// `apply` must be pure and deterministic so replaying the log rebuilds the
/// same state; illegal transitions are [`CoreError::IllegalTransition`] and
/// leave the caller's state unchanged.
pub trait TaskMachine: Send + Sync {
    fn apply(&self, state: TaskState, event: &TaskEvent) -> Result<TaskState>;
}

/// The reference transition table. Implementations may be stricter but not
/// looser; `quark-eventlog` is expected to reuse this one.
///
/// | From | Event | To |
/// |---|---|---|
/// | `Unknown` | `Queued` | `Queued` |
/// | `Queued`, `Running`, `Blocked`, `Paused`, `Failed`, `InReview` | `Started` | `Running` |
/// | `Running` | `DecisionNeeded` | `NeedsDecision` |
/// | `NeedsDecision` | `DecisionAnswered` | `Running` |
/// | `Running`, `NeedsDecision` | `Blocked` / `Paused` | `Blocked` / `Paused` |
/// | `Blocked`, `Paused` | `Resumed` | `Running` |
/// | `Running` | `InReview` | `InReview` |
/// | `InReview` | `Completed` | `Done` |
/// | `Running` | `Completed` | `Done` (reports, local-only) |
/// | any but `Done`, `Failed` | `Failed` / `Cancelled` | `Failed` |
#[derive(Debug, Clone, Copy, Default)]
pub struct ReferenceMachine;

impl TaskMachine for ReferenceMachine {
    fn apply(&self, state: TaskState, event: &TaskEvent) -> Result<TaskState> {
        use TaskState as S;
        let terminal = matches!(state, S::Done);
        let next = match (state, event) {
            (S::Unknown, TaskEvent::Queued { .. }) => Some(S::Queued),
            (
                S::Queued | S::Running | S::Blocked | S::Paused | S::Failed | S::InReview,
                TaskEvent::Started { .. },
            ) => Some(S::Running),
            (S::Running, TaskEvent::DecisionNeeded { .. }) => Some(S::NeedsDecision),
            (S::NeedsDecision, TaskEvent::DecisionAnswered { .. }) => Some(S::Running),
            (S::Running | S::NeedsDecision, TaskEvent::Blocked { .. }) => Some(S::Blocked),
            (S::Running | S::NeedsDecision, TaskEvent::Paused { .. }) => Some(S::Paused),
            (S::Blocked | S::Paused, TaskEvent::Resumed) => Some(S::Running),
            (S::Running, TaskEvent::InReview { .. }) => Some(S::InReview),
            (S::InReview | S::Running, TaskEvent::Completed) => Some(S::Done),
            (s, TaskEvent::Failed { .. } | TaskEvent::Cancelled) if !terminal && s != S::Failed => {
                Some(S::Failed)
            }
            _ => None,
        };
        next.ok_or_else(|| CoreError::IllegalTransition(format!("{} on {event:?}", state.as_str())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(events: &[TaskEvent]) -> Result<TaskState> {
        events
            .iter()
            .try_fold(TaskState::Unknown, |s, e| ReferenceMachine.apply(s, e))
    }

    #[test]
    fn ship_task_happy_path() {
        let s = run(&[
            TaskEvent::Queued { title: "t".into() },
            TaskEvent::Started {
                generation: "1".into(),
            },
            TaskEvent::DecisionNeeded {
                key: "k".into(),
                question: "q".into(),
            },
            TaskEvent::DecisionAnswered { key: "k".into() },
            TaskEvent::InReview { pull_request: None },
            TaskEvent::Completed,
        ]);
        assert_eq!(s, Ok(TaskState::Done));
    }

    #[test]
    fn relaunch_after_failure() {
        let s = run(&[
            TaskEvent::Queued { title: "t".into() },
            TaskEvent::Started {
                generation: "1".into(),
            },
            TaskEvent::Failed {
                reason: "crash".into(),
            },
            TaskEvent::Started {
                generation: "2".into(),
            },
        ]);
        assert_eq!(s, Ok(TaskState::Running));
    }

    #[test]
    fn done_is_terminal() {
        let err = run(&[
            TaskEvent::Queued { title: "t".into() },
            TaskEvent::Started {
                generation: "1".into(),
            },
            TaskEvent::Completed,
            TaskEvent::Cancelled,
        ]);
        assert!(matches!(err, Err(CoreError::IllegalTransition(_))));
    }

    #[test]
    fn answer_without_question_is_illegal() {
        let err = run(&[
            TaskEvent::Queued { title: "t".into() },
            TaskEvent::Started {
                generation: "1".into(),
            },
            TaskEvent::DecisionAnswered { key: "k".into() },
        ]);
        assert!(err.is_err());
    }
}
