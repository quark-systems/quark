//! Decisions the daemon opens for a prompt on a coordinator's screen: a
//! permission or approval request its harness draws in the terminal and
//! waits on (see `coordinator_prompt.rs`). The decision's answer picks the
//! choice on that screen; a prompt that leaves the screen first closes its
//! decision as answered elsewhere.

use quark_systems::{Decision, DecisionBrief, DecisionOption, DecisionState, EventType};
use rusqlite::{params, OptionalExtension};

use super::{
    append_event, decision_from_row, insert_decision, mark_answered, new_id, Result, Store,
    DECISION_SELECT,
};
use crate::now_rfc3339;

/// Engine ids of these decisions: `quarkd/coordinator-screen/<prompt key>`.
const PREFIX: &str = "quarkd/coordinator-screen/";

/// The prompt key of a decision opened for a coordinator's screen prompt.
pub fn coordinator_prompt_key(engine_id: &str) -> Option<&str> {
    engine_id.strip_prefix(PREFIX)
}

/// A prompt on a coordinator's screen, as its decision asks it.
#[derive(Debug, Clone)]
pub struct CoordinatorPrompt<'a> {
    pub key: &'a str,
    pub question: &'a str,
    /// The screen lines above the choices.
    pub context: &'a [String],
    pub options: &'a [String],
}

impl Store {
    /// Opens the decision for a prompt on `project_id`'s coordinator screen,
    /// emitting `decision.opened`, unless one is already open for it.
    /// Returns the open decision.
    pub fn open_coordinator_prompt(
        &self,
        project_id: &str,
        prompt: &CoordinatorPrompt,
    ) -> Result<Decision> {
        let engine_id = format!("{PREFIX}{}", prompt.key);
        self.write(|tx, events| {
            let open = tx
                .query_row(
                    &format!(
                        "{DECISION_SELECT} WHERE project_id = ?1 AND engine_id = ?2
                         AND state = 'open'"
                    ),
                    params![project_id, engine_id],
                    decision_from_row,
                )
                .optional()?;
            if let Some(d) = open {
                return Ok(d);
            }
            let mut decision = Decision {
                id: new_id("dec"),
                project_id: project_id.to_string(),
                question: prompt.question.to_string(),
                state: DecisionState::Open,
                brief: DecisionBrief {
                    context: (!prompt.context.is_empty()).then(|| prompt.context.join("\n")),
                    options: prompt
                        .options
                        .iter()
                        .map(|o| DecisionOption {
                            label: o.clone(),
                            consequence: None,
                        })
                        .collect(),
                    asked_by: Some("quarkd".into()),
                    ..Default::default()
                },
                opened_at: now_rfc3339(),
                ..Default::default()
            };
            insert_decision(tx, &engine_id, &mut decision)?;
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

    /// Closes `project_id`'s open screen-prompt decisions, other than the
    /// one for `keep`, as answered elsewhere (no answer): their prompts left
    /// the screen. Emits `decision.answered` for each.
    pub fn close_coordinator_prompts(&self, project_id: &str, keep: Option<&str>) -> Result<()> {
        let keep = keep.map(|k| format!("{PREFIX}{k}"));
        self.write(|tx, events| {
            let open: Vec<(String, Decision)> = {
                let mut stmt = tx.prepare(&format!(
                    "SELECT engine_id, {cols} FROM decisions
                     WHERE project_id = ?1 AND state = 'open' AND engine_id LIKE ?2",
                    cols = super::DECISION_COLUMNS
                ))?;
                let rows = stmt.query_map(params![project_id, format!("{PREFIX}%")], |r| {
                    Ok((r.get(0)?, super::decision_from_row_at(r, 1)?))
                })?;
                rows.collect::<std::result::Result<_, _>>()?
            };
            for (engine_id, old) in open {
                if keep.as_deref() == Some(engine_id.as_str()) {
                    continue;
                }
                let decision = Decision {
                    state: DecisionState::Answered,
                    answered_at: Some(now_rfc3339()),
                    ..old
                };
                mark_answered(tx, events, &decision)?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use quark_systems::CreateProject;

    use super::*;

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
    fn a_screen_prompt_opens_once_and_closes_when_it_leaves() {
        let store = Store::open_in_memory().unwrap();
        let p = project(&store);
        let context = vec!["Run podman compose up?".to_string()];
        let options = vec!["Approve Once".to_string(), "Reject".to_string()];
        let prompt = CoordinatorPrompt {
            key: "screen-1234abcd",
            question: "The coordinator's screen waits",
            context: &context,
            options: &options,
        };
        let a = store.open_coordinator_prompt(&p, &prompt).unwrap();
        let b = store.open_coordinator_prompt(&p, &prompt).unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(a.brief.options.len(), 2);
        assert_eq!(a.brief.context.as_deref(), Some("Run podman compose up?"));
        let target = store.decision_target(&a.id).unwrap();
        assert_eq!(
            coordinator_prompt_key(&target.engine_id),
            Some("screen-1234abcd")
        );
        assert!(super::super::is_daemon_decision(&target.engine_id));

        // Still on screen: kept.
        store
            .close_coordinator_prompts(&p, Some("screen-1234abcd"))
            .unwrap();
        assert_eq!(
            store.get_decision(&a.id).unwrap().state,
            DecisionState::Open
        );
        // Gone: answered elsewhere, with no answer.
        store.close_coordinator_prompts(&p, None).unwrap();
        let closed = store.get_decision(&a.id).unwrap();
        assert_eq!(closed.state, DecisionState::Answered);
        assert!(closed.answer.is_none());
        // Back again: a new decision.
        let c = store.open_coordinator_prompt(&p, &prompt).unwrap();
        assert_ne!(c.id, a.id);
    }
}
