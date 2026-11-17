//! Typing a coordinator's sent messages into its session.
//!
//! Every message sent through `POST /v1/coordinators/{id}/messages` is a row
//! of the store's outbox (`store/outbox.rs`). One sent while the coordinator
//! works waits there as `queued` and is typed in when the coordinator's turn
//! ends (its turn-end hook calls [`deliver_next`]) or, for a harness without
//! that hook, once its transcript has been quiet for [`QUIET`].

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use quark_systems::{OutboxMessage, OutboxState};

use crate::chat::{ChatError, CoordinatorInput, Delivery};
use crate::engine::WorkspaceRef;
use crate::store::{Store, StoreError};

/// How long a coordinator's transcript stays unchanged before a queued
/// message is typed in without a turn-end hook.
pub const QUIET: Duration = Duration::from_secs(30);
/// How often [`run`] looks for queued messages.
pub const INTERVAL: Duration = Duration::from_secs(3);

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, StoreError> + Send + 'static,
) -> Result<T, StoreError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| StoreError::Invalid(e.to_string()))?
}

/// Types a claimed (`sending`) message into its coordinator's session and
/// records how that went. Returns the message and the delivery error, if
/// any.
pub async fn deliver(
    store: Arc<Store>,
    chat: Arc<dyn CoordinatorInput>,
    m: OutboxMessage,
) -> Result<(OutboxMessage, Option<ChatError>), StoreError> {
    let pid = m.coordinator_id.clone();
    let s = store.clone();
    let project = blocking(move || s.get_project(&pid)).await?;
    let outcome = match project.workspace_path {
        None => Err(ChatError::Unavailable(
            "the Project has no workspace, so it has no coordinator yet".into(),
        )),
        Some(root) => {
            let ws = WorkspaceRef {
                project_id: project.id.clone(),
                root: PathBuf::from(root),
            };
            chat.send(&ws, &m.text).await
        }
    };
    let (recorded, error) = match outcome {
        Ok(d) => (Ok(d == Delivery::Confirmed), None),
        Err(e) => (Err(e.to_string()), Some(e)),
    };
    let id = m.id.clone();
    let m = blocking(move || store.finish_outbox_delivery(&id, recorded)).await?;
    Ok((m, error))
}

/// Types in the Project's oldest queued message, if one waits and none is
/// being typed in.
pub async fn deliver_next(store: Arc<Store>, chat: Arc<dyn CoordinatorInput>, project_id: String) {
    // [`run`] types it in once the prompt on the screen is answered.
    if crate::coordinator_prompt::waiting(&project_id) {
        return;
    }
    let s = store.clone();
    let pid = project_id.clone();
    let next = match blocking(move || s.claim_next_queued(&pid)).await {
        Ok(Some(m)) => m,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(project = %project_id, error = %e, "could not read queued messages");
            return;
        }
    };
    match deliver(store, chat, next).await {
        Ok((m, Some(e))) => {
            tracing::warn!(project = %project_id, message = %m.id, error = %e, "queued message not delivered")
        }
        Ok((m, None)) if m.state == OutboxState::Unconfirmed => {
            tracing::info!(project = %project_id, message = %m.id, "queued message typed in, not confirmed")
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(project = %project_id, error = %e, "could not record a delivery"),
    }
}

/// Whether the coordinator's transcript has been quiet for [`QUIET`].
pub fn quiet(store: &Store, project_id: &str) -> Result<bool, StoreError> {
    let Some(ts) = store.coordinator_last_activity(project_id)? else {
        return Ok(true);
    };
    let Ok(at) = time::OffsetDateTime::parse(&ts, &time::format_description::well_known::Rfc3339)
    else {
        return Ok(true);
    };
    Ok(time::OffsetDateTime::now_utc() - at >= QUIET)
}

/// Types in queued messages whose coordinator has gone quiet, every
/// `interval`, until the task is dropped.
pub async fn run(store: Arc<Store>, chat: Arc<dyn CoordinatorInput>, interval: Duration) {
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let s = store.clone();
        let ready = blocking(move || {
            let mut ready = Vec::new();
            for p in s.projects_with_queued_messages()? {
                // A prompt on its screen would take the typing.
                if quiet(&s, &p)? && !crate::coordinator_prompt::waiting(&p) {
                    ready.push(p);
                }
            }
            Ok(ready)
        })
        .await;
        match ready {
            Ok(ready) => {
                for p in ready {
                    tokio::spawn(deliver_next(store.clone(), chat.clone(), p));
                }
            }
            Err(e) => tracing::warn!(error = %e, "could not read the coordinator outbox"),
        }
    }
}

#[cfg(test)]
mod tests {
    use quark_systems::CreateProject;

    use super::*;
    use crate::chat::RecordingInput;

    fn project(store: &Store) -> String {
        store
            .create_project(CreateProject {
                name: "Quark".into(),
                goal: None,
                workspace_path: Some("/ws".into()),
                repos: vec![],
                agent_config: None,
                dispatch_preset: None,
                delivery: None,
            })
            .unwrap()
            .id
    }

    #[tokio::test]
    async fn a_queued_message_goes_in_when_the_coordinator_is_quiet() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let p = project(&store);
        let input = Arc::new(RecordingInput::default());
        let chat: Arc<dyn CoordinatorInput> = input.clone();
        store.coordinator_says(&p, "Working on it").unwrap();
        assert!(!quiet(&store, &p).unwrap());
        let m = store
            .new_outbox_message(&p, "and the docs", OutboxState::Queued)
            .unwrap();
        deliver_next(store.clone(), chat.clone(), p.clone()).await;
        assert_eq!(
            store.outbox_message(&m.id).unwrap().state,
            OutboxState::Sent
        );
        assert_eq!(input.sent()[0].1, "and the docs");
        // Nothing left to send.
        deliver_next(store.clone(), chat, p.clone()).await;
        assert_eq!(input.sent().len(), 1);
    }
}
