//! Harness hooks POSTing to quarkd.
//!
//! The hook command `quark-harness` installs for a task posts the harness's
//! own hook payload to `/hooks/{task}/{generation}/{event}`, where `event`
//! is the harness's hook name (`Stop`, `UserPromptSubmit`, ...). The task's
//! manifest maps that name to a neutral signal in `hooks.events`:
//!
//! - `busy`, `turn_end` and any other signal name become
//!   [`WorkerMessage::Signal`]; the harness payload is not kept.
//! - `report` means the body is itself a [`WorkerMessage`] (`{"tool":
//!   "report", ...}`), for harnesses whose hooks can run a worker command.
//!
//! A neutral name used directly as the event (`/hooks/t/g/turn_end`) works
//! without a mapping, so a generic hook script needs no manifest entry.

use std::collections::BTreeMap;

use quark_core::worker::{Transport, WorkerMessage};
use quark_core::{CoreError, EventId, Result};
use serde_json::Value;

use crate::recorder::{Recorder, WorkerIdentity};

/// The neutral signal meaning "the body is a worker message".
pub const REPORT: &str = "report";

/// Signals a hook may name directly without a manifest mapping.
pub const NEUTRAL: &[&str] = &["busy", "turn_end", REPORT];

/// The message one hook call carries.
pub fn hook_message(
    events: &BTreeMap<String, String>,
    event: &str,
    body: &Value,
) -> Result<WorkerMessage> {
    let signal = match events.get(event) {
        Some(signal) => signal.as_str(),
        None if NEUTRAL.contains(&event) => event,
        None => {
            return Err(CoreError::Invalid(format!(
                "hook {event} is not in the harness manifest's hooks.events"
            )))
        }
    };
    if signal == REPORT {
        return serde_json::from_value(body.clone())
            .map_err(|e| CoreError::Invalid(format!("hook {event} body: {e}")));
    }
    Ok(WorkerMessage::Signal {
        signal: signal.to_string(),
    })
}

/// Records one hook call. `id`, when the hook sends one, makes a retried
/// POST record once.
pub async fn ingest(
    recorder: &Recorder,
    who: &WorkerIdentity,
    event: &str,
    body: &Value,
    id: Option<EventId>,
) -> Result<EventId> {
    let binding = recorder.binding(&who.task).await?;
    let message = hook_message(&binding.hook_events, event, body)?;
    recorder
        .receive_as(
            id.unwrap_or_default(),
            who.envelope(Transport::Hook, message),
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::tests::setup;
    use quark_core::worker::WorkerEnvelope;
    use serde_json::json;

    #[tokio::test]
    async fn mapped_hooks_become_signals() {
        let (rec, log, _) = setup();
        let who = WorkerIdentity::new("t1", "g2");
        let claude_stop = json!({ "hook_event_name": "Stop", "session_id": "abc" });
        ingest(&rec, &who, "Stop", &claude_stop, None)
            .await
            .unwrap();
        ingest(&rec, &who, "UserPromptSubmit", &json!({}), None)
            .await
            .unwrap();
        ingest(&rec, &who, "turn_end", &Value::Null, None)
            .await
            .unwrap();
        let signals: Vec<_> = log
            .events()
            .iter()
            .map(|e| e.decode::<WorkerEnvelope>().unwrap())
            .inspect(|e| assert_eq!(e.via, Transport::Hook))
            .map(|e| match e.message {
                WorkerMessage::Signal { signal } => signal,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(signals, ["turn_end", "busy", "turn_end"]);
    }

    #[tokio::test]
    async fn report_hooks_carry_a_message() {
        let (rec, log, _) = setup();
        let who = WorkerIdentity::new("t1", "g2");
        let body = json!({ "tool": "learned", "fact": "use cargo nextest" });
        ingest(&rec, &who, "Notify", &body, None).await.unwrap();
        let env: WorkerEnvelope = log.events()[0].decode().unwrap();
        assert_eq!(
            env.message,
            WorkerMessage::Learned {
                fact: "use cargo nextest".into()
            }
        );
        let bad = ingest(&rec, &who, "report", &json!({ "tool": "nope" }), None).await;
        assert!(matches!(bad, Err(CoreError::Invalid(_))));
    }

    #[tokio::test]
    async fn unmapped_hooks_are_refused_and_retries_record_once() {
        let (rec, log, _) = setup();
        let who = WorkerIdentity::new("t1", "g2");
        let err = ingest(&rec, &who, "PreCompact", &json!({}), None).await;
        assert!(matches!(err, Err(CoreError::Invalid(_))));
        let id = EventId::new();
        for _ in 0..2 {
            ingest(&rec, &who, "Stop", &json!({}), Some(id))
                .await
                .unwrap();
        }
        assert_eq!(log.events().len(), 1);
    }
}
