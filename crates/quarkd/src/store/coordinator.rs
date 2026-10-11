//! What the native coordinator (slice 6) writes to the store itself: the
//! decisions it raises with `ask_user`, and what it tells the user.
//!
//! Its decisions are the daemon's own (no engine holds them), so a refresh
//! never closes one; an answer goes back to the coordinator through the
//! event log instead of the engine (`api::routes::answer`).

use quark_systems::{
    Decision, DecisionBrief, DecisionState, EventType, TranscriptEntry, TranscriptRole,
};
use rusqlite::{params, OptionalExtension};

use super::{
    api_task_ids, append_event, decision_from_row, insert_decision, new_id, Result, Store,
    DECISION_SELECT,
};
use crate::now_rfc3339;

/// Engine ids of the coordinator's decisions, under the daemon's prefix.
const PREFIX: &str = "quarkd/coordinator/";

/// The engine id of the coordinator's decision asked under `key`.
pub fn coordinator_decision_id(key: &str) -> String {
    format!("{PREFIX}{key}")
}

/// The key the coordinator asked a decision under, when the engine id is
/// one of its decisions.
pub fn coordinator_decision_key(engine_id: &str) -> Option<&str> {
    engine_id.strip_prefix(PREFIX)
}

impl Store {
    /// Opens the coordinator's decision asked under `key`, emitting
    /// `decision.opened`. Asking again while it is still open updates its
    /// question and brief in place (and emits `decision.opened` again when
    /// either changed); asking after it was answered opens a new one.
    pub fn ask_from_coordinator(
        &self,
        project_id: &str,
        key: &str,
        question: &str,
        brief: DecisionBrief,
    ) -> Result<Decision> {
        let engine_id = coordinator_decision_id(key);
        self.write(|tx, events| {
            let brief = api_task_ids(tx, project_id, brief.normalized())?;
            let open = tx
                .query_row(
                    &format!(
                        "{DECISION_SELECT} WHERE project_id = ?1 AND engine_id = ?2
                         AND state = 'open' ORDER BY rowid DESC LIMIT 1"
                    ),
                    params![project_id, engine_id],
                    decision_from_row,
                )
                .optional()?;
            let decision = match open {
                Some(old) if old.question == question && old.brief == brief => return Ok(old),
                Some(old) => {
                    let d = Decision {
                        question: question.to_string(),
                        brief,
                        ..old
                    };
                    tx.execute(
                        "UPDATE decisions SET question = ?2, brief = ?3 WHERE id = ?1",
                        params![d.id, d.question, serde_json::to_string(&d.brief)?],
                    )?;
                    d
                }
                None => {
                    let mut d = Decision {
                        id: new_id("dec"),
                        project_id: project_id.to_string(),
                        question: question.to_string(),
                        state: DecisionState::Open,
                        brief,
                        opened_at: now_rfc3339(),
                        ..Default::default()
                    };
                    insert_decision(tx, &engine_id, &mut d)?;
                    d
                }
            };
            append_event(
                tx,
                events,
                Some(project_id),
                EventType::DecisionOpened,
                serde_json::to_value(&decision)?,
            )?;
            Ok(decision)
        })
    }

    /// Puts what the coordinator tells the user into the Project's
    /// coordinator chat, emitting `coordinator.message`.
    pub fn coordinator_says(&self, project_id: &str, text: &str) -> Result<()> {
        let entry = TranscriptEntry {
            role: TranscriptRole::Assistant,
            text: text.to_string(),
            tool_name: None,
            tool_call_id: None,
            is_error: false,
            truncated: false,
            ts: Some(now_rfc3339()),
            tool: None,
        };
        self.write(|tx, events| {
            append_event(
                tx,
                events,
                Some(project_id),
                EventType::CoordinatorMessage,
                serde_json::json!({ "coordinator_id": project_id, "entry": entry }),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use quark_systems::{CreateProject, DecisionOption};

    use super::*;
    use crate::engine::Hold;
    use crate::store::TranscriptSource;

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

    #[test]
    fn asking_again_updates_and_a_refresh_never_closes_it() {
        let store = Store::open_in_memory().unwrap();
        let p = project(&store);
        assert!(crate::store::is_daemon_decision(&coordinator_decision_id(
            "k"
        )));
        let brief = DecisionBrief {
            options: ["SQLite", "Postgres"]
                .map(|l| DecisionOption {
                    label: l.into(),
                    consequence: None,
                })
                .to_vec(),
            asked_by: Some("coordinator".into()),
            ..Default::default()
        };
        let d = store
            .ask_from_coordinator(&p, "storage", "Which database?", brief.clone())
            .unwrap();
        assert_eq!(d.state, DecisionState::Open);
        let same = store
            .ask_from_coordinator(&p, "storage", "Which database?", brief.clone())
            .unwrap();
        assert_eq!(same, d);
        let again = store
            .ask_from_coordinator(&p, "storage", "Which database for v1?", brief)
            .unwrap();
        assert_eq!(again.id, d.id);
        assert_eq!(again.question, "Which database for v1?");

        // The engine holds nothing, so a refresh leaves it open.
        store
            .apply_holds(&p, &[] as &[Hold], "2999-01-01T00:00:00Z")
            .unwrap();
        assert_eq!(
            store.get_decision(&d.id).unwrap().state,
            DecisionState::Open
        );
        let target = store.decision_target(&d.id).unwrap();
        assert_eq!(coordinator_decision_key(&target.engine_id), Some("storage"));

        store.answer_decision(&d.id, "SQLite", "Matt").unwrap();
        let next = store
            .ask_from_coordinator(&p, "storage", "Which database?", DecisionBrief::default())
            .unwrap();
        assert_ne!(next.id, d.id);
    }

    #[test]
    fn what_it_says_lands_in_the_coordinator_chat() {
        let store = Store::open_in_memory().unwrap();
        let p = project(&store);
        store.coordinator_says(&p, "PR #12 merged.").unwrap();
        let items = store
            .transcript(
                &TranscriptSource::Coordinator {
                    project_id: p.clone(),
                },
                0,
                10,
            )
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].entry.text, "PR #12 merged.");
        assert_eq!(items[0].entry.role, TranscriptRole::Assistant);
    }
}
