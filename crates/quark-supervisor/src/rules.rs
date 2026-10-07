//! How worker messages and answers change a task: the supervisor's rules,
//! as pure functions so the shadow can apply them to firstmate's workers
//! ([`crate::shadow`]) exactly as the supervisor applies them to its own.

use std::collections::BTreeSet;

use quark_core::worker::WorkerMessage;
use quark_core::TaskEvent;
use quark_systems::TaskState;

/// The transitions one worker message asks for, in order. The supervisor
/// records them until the machine refuses one.
pub fn message_transitions(
    state: TaskState,
    open_decisions: &BTreeSet<String>,
    message: &WorkerMessage,
) -> Vec<TaskEvent> {
    let paused = matches!(state, TaskState::Blocked | TaskState::Paused);
    let mut events = Vec::new();
    match message {
        WorkerMessage::Report { state, note } => match state.to_ascii_lowercase().as_str() {
            "blocked" => events.push(TaskEvent::Blocked {
                reason: note.clone(),
            }),
            "paused" => events.push(TaskEvent::Paused {
                reason: note.clone(),
            }),
            "failed" => events.push(TaskEvent::Failed {
                reason: note.clone(),
            }),
            _ if paused => events.push(TaskEvent::Resumed),
            _ => {}
        },
        WorkerMessage::Ask { key, question } => {
            if !open_decisions.contains(key) {
                if paused {
                    events.push(TaskEvent::Resumed);
                }
                events.push(TaskEvent::DecisionNeeded {
                    key: key.clone(),
                    question: question.clone(),
                });
            }
        }
        WorkerMessage::Done { pull_request, .. } => {
            if paused {
                events.push(TaskEvent::Resumed);
            }
            events.push(match pull_request {
                Some(pr) => TaskEvent::InReview {
                    pull_request: Some(pr.clone()),
                },
                None => TaskEvent::Completed,
            });
        }
        WorkerMessage::Learned { .. } | WorkerMessage::Signal { .. } => {}
    }
    events
}

/// The transitions answering the open decision `key` records; `None` when
/// no such decision is open, which the supervisor refuses.
pub fn answer_transitions(
    state: TaskState,
    open_decisions: &BTreeSet<String>,
    key: &str,
) -> Option<Vec<TaskEvent>> {
    if !open_decisions.contains(key) {
        return None;
    }
    Some(if state == TaskState::NeedsDecision {
        vec![TaskEvent::DecisionAnswered {
            key: key.to_string(),
        }]
    } else {
        Vec::new()
    })
}
