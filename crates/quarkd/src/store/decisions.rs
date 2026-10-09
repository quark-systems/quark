//! The decision log's later steps: what the asker did with an answer
//! (`decision.acted`), standing rules made from answers (`rule.updated`),
//! and decisions agents logged under a rule.
//!
//! Opening and answering decisions stay with the engine projection in
//! `store.rs` ([`Store::apply_holds`]).

use quark_systems::{
    Decision, DecisionState, EventType, RecordRuleDecision, RuleKind, StandingRule,
};
use rusqlite::{params, OptionalExtension, Row, Transaction};

use super::{
    append_event, decision_from_row, insert_decision, new_id, Result, Store, StoreError,
    DECISION_SELECT,
};
use crate::now_rfc3339;

/// The engine id of a decision an agent logged under a rule: no engine
/// holds it, so nothing answers or closes it there.
const RULE_DECISION_PREFIX: &str = "quark-rule:";

const RULE_SELECT: &str = "SELECT r.id, r.project_id, r.text, r.decision_id, r.created_by,
         r.created_at, r.revoked_at, r.revoked_by, r.changed_at, r.changed_by,
         (SELECT COUNT(*) FROM decisions d WHERE d.rule_id = r.id)
     FROM standing_rules r";

fn rule_from_row(r: &Row) -> rusqlite::Result<StandingRule> {
    Ok(StandingRule {
        id: r.get(0)?,
        project_id: r.get(1)?,
        kind: RuleKind::Answer,
        text: r.get(2)?,
        decision_id: r.get(3)?,
        created_by: r.get(4)?,
        created_at: r.get(5)?,
        revoked_at: r.get(6)?,
        revoked_by: r.get(7)?,
        changed_at: r.get(8)?,
        changed_by: r.get(9)?,
        applied: r.get(10)?,
    })
}

fn get_rule(tx: &Transaction, id: &str) -> Result<StandingRule> {
    tx.query_row(
        &format!("{RULE_SELECT} WHERE r.id = ?1"),
        [id],
        rule_from_row,
    )
    .optional()?
    .ok_or(StoreError::NotFound)
}

impl Store {
    /// Records what the asker did with an answered decision, emitting
    /// `decision.acted`. Recording again replaces the outcome.
    pub fn act_on_decision(&self, id: &str, outcome: &str) -> Result<Decision> {
        self.write(|tx, events| {
            let old = tx
                .query_row(
                    &format!("{DECISION_SELECT} WHERE id = ?1"),
                    [id],
                    decision_from_row,
                )
                .optional()?
                .ok_or(StoreError::NotFound)?;
            if old.state == DecisionState::Open {
                return Err(StoreError::Conflict(
                    "the decision has no answer to act on yet".into(),
                ));
            }
            let decision = Decision {
                state: DecisionState::Acted,
                outcome: Some(outcome.to_string()),
                acted_at: Some(now_rfc3339()),
                ..old
            };
            tx.execute(
                "UPDATE decisions SET state = 'acted', outcome = ?2, acted_at = ?3 WHERE id = ?1",
                params![decision.id, decision.outcome, decision.acted_at],
            )?;
            append_event(
                tx,
                events,
                Some(&decision.project_id),
                EventType::DecisionActed,
                serde_json::to_value(&decision)?,
            )?;
            Ok(decision)
        })
    }

    /// Makes a standing rule from an answered decision's answer, emitting
    /// `rule.updated` and `decision.answered` for the decision that now
    /// names it. Conflicts when the decision is still open or already made
    /// a rule.
    pub fn make_rule(&self, decision_id: &str, text: &str, by: &str) -> Result<StandingRule> {
        self.write(|tx, events| {
            let old = tx
                .query_row(
                    &format!("{DECISION_SELECT} WHERE id = ?1"),
                    [decision_id],
                    decision_from_row,
                )
                .optional()?
                .ok_or(StoreError::NotFound)?;
            if old.state == DecisionState::Open {
                return Err(StoreError::Conflict(
                    "answer the decision before making it a rule".into(),
                ));
            }
            if old.made_rule_id.is_some() {
                return Err(StoreError::Conflict(
                    "the decision already made a rule".into(),
                ));
            }
            let id = new_id("rule");
            tx.execute(
                "INSERT INTO standing_rules (id, project_id, text, decision_id, created_by, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, old.project_id, text, decision_id, by, now_rfc3339()],
            )?;
            tx.execute(
                "UPDATE decisions SET made_rule_id = ?2 WHERE id = ?1",
                params![decision_id, id],
            )?;
            let decision = Decision {
                made_rule_id: Some(id.clone()),
                ..old
            };
            let rule = get_rule(tx, &id)?;
            append_event(
                tx,
                events,
                Some(&rule.project_id),
                EventType::RuleUpdated,
                serde_json::to_value(&rule)?,
            )?;
            append_event(
                tx,
                events,
                Some(&decision.project_id),
                EventType::DecisionAnswered,
                serde_json::to_value(&decision)?,
            )?;
            Ok(rule)
        })
    }

    /// Standing rules made from answers, oldest first; revoked ones too
    /// when `include_revoked`.
    pub fn list_rules(
        &self,
        project_id: Option<&str>,
        include_revoked: bool,
    ) -> Result<Vec<StandingRule>> {
        self.read(|c| {
            let mut stmt = c.prepare(&format!(
                "{RULE_SELECT} WHERE (?1 IS NULL OR r.project_id = ?1)
                   AND (?2 OR r.revoked_at IS NULL)
                 ORDER BY r.created_at, r.id"
            ))?;
            let rows = stmt.query_map(params![project_id, include_revoked], rule_from_row)?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })
    }

    pub fn get_rule(&self, id: &str) -> Result<StandingRule> {
        self.write(|tx, _| get_rule(tx, id))
    }

    /// Revokes a rule, emitting `rule.updated`. Revoking again is a no-op.
    pub fn revoke_rule(&self, id: &str, by: &str) -> Result<StandingRule> {
        self.write(|tx, events| {
            let rule = get_rule(tx, id)?;
            if rule.revoked_at.is_some() {
                return Ok(rule);
            }
            tx.execute(
                "UPDATE standing_rules SET revoked_at = ?2, revoked_by = ?3 WHERE id = ?1",
                params![id, now_rfc3339(), by],
            )?;
            let rule = get_rule(tx, id)?;
            append_event(
                tx,
                events,
                Some(&rule.project_id),
                EventType::RuleUpdated,
                serde_json::to_value(&rule)?,
            )?;
            Ok(rule)
        })
    }

    /// Changes a rule's words, emitting `rule.updated`. Refused once revoked.
    pub fn change_rule(&self, id: &str, text: &str, by: &str) -> Result<StandingRule> {
        self.write(|tx, events| {
            let rule = get_rule(tx, id)?;
            if rule.revoked_at.is_some() {
                return Err(StoreError::Conflict("the rule is revoked".into()));
            }
            if rule.text == text {
                return Ok(rule);
            }
            tx.execute(
                "UPDATE standing_rules SET text = ?2, changed_at = ?3, changed_by = ?4 WHERE id = ?1",
                params![id, text, now_rfc3339(), by],
            )?;
            let rule = get_rule(tx, id)?;
            append_event(
                tx,
                events,
                Some(&rule.project_id),
                EventType::RuleUpdated,
                serde_json::to_value(&rule)?,
            )?;
            Ok(rule)
        })
    }

    /// Records the `decision` bead that mirrors a decision, emitting the
    /// decision again (`decision.opened` while open, else
    /// `decision.answered`) so clients show it.
    pub fn set_decision_bead(&self, id: &str, bead_id: &str) -> Result<Decision> {
        self.write(|tx, events| {
            let n = tx.execute(
                "UPDATE decisions SET bead_id = ?2 WHERE id = ?1 AND bead_id IS NOT ?2",
                params![id, bead_id],
            )?;
            let d = tx
                .query_row(
                    &format!("{DECISION_SELECT} WHERE id = ?1"),
                    [id],
                    decision_from_row,
                )
                .optional()?
                .ok_or(StoreError::NotFound)?;
            if n > 0 {
                let kind = if d.state == DecisionState::Open {
                    EventType::DecisionOpened
                } else {
                    EventType::DecisionAnswered
                };
                append_event(
                    tx,
                    events,
                    Some(&d.project_id),
                    kind,
                    serde_json::to_value(&d)?,
                )?;
            }
            Ok(d)
        })
    }

    /// Logs a decision an agent made under a rule: answered at once (acted
    /// on when `outcome` is given), emitting `decision.opened` and
    /// `decision.answered` (and `decision.acted`). Refused under a revoked
    /// rule.
    pub fn record_rule_decision(&self, input: &RecordRuleDecision) -> Result<Decision> {
        self.write(|tx, events| {
            let rule = get_rule(tx, &input.rule_id)?;
            if rule.revoked_at.is_some() {
                return Err(StoreError::Conflict(
                    "the rule was revoked; ask instead".into(),
                ));
            }
            let task_id: Option<String> = match &input.task_id {
                Some(t) => tx
                    .query_row(
                        "SELECT id FROM tasks WHERE project_id = ?1 AND (id = ?2 OR engine_id = ?2)",
                        params![rule.project_id, t],
                        |r| r.get(0),
                    )
                    .optional()?,
                None => None,
            };
            let now = now_rfc3339();
            let acted = input.outcome.is_some();
            let mut decision = Decision {
                id: new_id("dec"),
                project_id: rule.project_id.clone(),
                task_id,
                question: input.question.clone(),
                state: if acted {
                    DecisionState::Acted
                } else {
                    DecisionState::Answered
                },
                brief: input.brief.clone().normalized(),
                answer: Some(input.answer.clone()),
                answered_by: Some(input.decided_by.clone()),
                answered_via: Some("rule".into()),
                opened_at: now.clone(),
                answered_at: Some(now.clone()),
                outcome: input.outcome.clone(),
                acted_at: acted.then(|| now.clone()),
                rule_id: Some(rule.id.clone()),
                ..Default::default()
            };
            let engine_id = format!("{RULE_DECISION_PREFIX}{}", decision.id);
            insert_decision(tx, &engine_id, &mut decision)?;
            let payload = serde_json::to_value(&decision)?;
            let p = Some(decision.project_id.as_str());
            append_event(tx, events, p, EventType::DecisionOpened, payload.clone())?;
            append_event(tx, events, p, EventType::DecisionAnswered, payload.clone())?;
            if acted {
                append_event(tx, events, p, EventType::DecisionActed, payload)?;
            }
            let rule = get_rule(tx, &rule.id)?;
            append_event(
                tx,
                events,
                Some(&rule.project_id),
                EventType::RuleUpdated,
                serde_json::to_value(&rule)?,
            )?;
            Ok(decision)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Hold;
    use crate::store::AnswerNote;
    use quark_systems::{CreateProject, DecisionBrief, DecisionOption};

    fn project(store: &Store) -> String {
        store
            .create_project(CreateProject {
                name: "p".into(),
                ..Default::default()
            })
            .unwrap()
            .id
    }

    fn hold(id: &str) -> Hold {
        Hold {
            id: id.into(),
            question: "Switch slice 2?".into(),
            brief: DecisionBrief {
                options: vec![DecisionOption {
                    label: "Switch now".into(),
                    consequence: Some("Merges #95".into()),
                }],
                recommended: Some("Switch now".into()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn numbers_briefs_answers_and_outcomes() {
        let store = Store::open_in_memory().unwrap();
        let p = project(&store);
        store
            .apply_holds(&p, &[hold("a"), hold("b")], &now_rfc3339())
            .unwrap();
        let open = store.list_decisions(Some(DecisionState::Open)).unwrap();
        assert_eq!(
            open.iter().map(|d| d.number).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(open[0].brief.options[0].label, "Switch now");

        let d = store
            .answer_decision_with(
                &open[0].id,
                "Switch now",
                "matt",
                &AnswerNote {
                    why: Some("no disagreements".into()),
                    via: Some("phone".into()),
                },
            )
            .unwrap();
        assert_eq!(d.answer_why.as_deref(), Some("no disagreements"));
        assert_eq!(
            store.get_decision(&d.id).unwrap().answered_via.as_deref(),
            Some("phone")
        );
        assert!(matches!(
            store.act_on_decision(&open[1].id, "x"),
            Err(StoreError::Conflict(_))
        ));
        let acted = store.act_on_decision(&d.id, "Merged #95").unwrap();
        assert_eq!(acted.state, DecisionState::Acted);
        assert_eq!(store.get_decision(&d.id).unwrap(), acted);
    }

    #[test]
    fn a_later_brief_updates_the_open_decision() {
        let store = Store::open_in_memory().unwrap();
        let p = project(&store);
        let bare = Hold {
            brief: DecisionBrief::default(),
            ..hold("a")
        };
        store.apply_holds(&p, &[bare], &now_rfc3339()).unwrap();
        store.apply_holds(&p, &[hold("a")], &now_rfc3339()).unwrap();
        let open = store.list_decisions(Some(DecisionState::Open)).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].brief.recommended.as_deref(), Some("Switch now"));
    }

    #[test]
    fn rules_from_answers_log_and_revoke() {
        let store = Store::open_in_memory().unwrap();
        let p = project(&store);
        store.apply_holds(&p, &[hold("a")], &now_rfc3339()).unwrap();
        let d = &store.list_decisions(None).unwrap()[0];
        assert!(matches!(
            store.make_rule(&d.id, "switch when shadow agrees", "matt"),
            Err(StoreError::Conflict(_))
        ));
        store.answer_decision(&d.id, "Switch now", "matt").unwrap();
        let rule = store
            .make_rule(&d.id, "switch when shadow agrees", "matt")
            .unwrap();
        assert_eq!(rule.decision_id.as_deref(), Some(d.id.as_str()));
        assert_eq!(
            store.get_decision(&d.id).unwrap().made_rule_id.as_deref(),
            Some(rule.id.as_str())
        );

        let logged = store
            .record_rule_decision(&RecordRuleDecision {
                rule_id: rule.id.clone(),
                question: "Switch slice 3?".into(),
                answer: "Switched".into(),
                decided_by: "coordinator".into(),
                outcome: Some("Merged #97".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(logged.number, 2);
        assert_eq!(logged.state, DecisionState::Acted);
        assert_eq!(logged.rule_id.as_deref(), Some(rule.id.as_str()));
        assert_eq!(store.get_rule(&rule.id).unwrap().applied, 1);

        // Logged decisions have no engine hold to go away.
        store.apply_holds(&p, &[], &now_rfc3339()).unwrap();
        assert_eq!(
            store.get_decision(&logged.id).unwrap().state,
            DecisionState::Acted
        );

        let changed = store
            .change_rule(&rule.id, "switch after 7 days of agreement", "matt")
            .unwrap();
        assert_eq!(changed.text, "switch after 7 days of agreement");
        assert_eq!(changed.changed_by.as_deref(), Some("matt"));
        assert_eq!(changed.applied, 1);
        // The same words again change nothing.
        let seq = store.events_after(0, 1000).unwrap().len();
        store
            .change_rule(&rule.id, "switch after 7 days of agreement", "ann")
            .unwrap();
        assert_eq!(store.events_after(0, 1000).unwrap().len(), seq);

        let revoked = store.revoke_rule(&rule.id, "matt").unwrap();
        assert!(revoked.revoked_at.is_some());
        assert!(store.list_rules(Some(&p), false).unwrap().is_empty());
        assert_eq!(store.list_rules(Some(&p), true).unwrap().len(), 1);
        assert!(matches!(
            store.record_rule_decision(&RecordRuleDecision {
                rule_id: rule.id.clone(),
                question: "q".into(),
                answer: "a".into(),
                decided_by: "coordinator".into(),
                ..Default::default()
            }),
            Err(StoreError::Conflict(_))
        ));
        assert!(matches!(
            store.change_rule(&rule.id, "too late", "matt"),
            Err(StoreError::Conflict(_))
        ));
    }

    #[test]
    fn a_decision_records_its_bead_once() {
        let store = Store::open_in_memory().unwrap();
        let p = project(&store);
        store.apply_holds(&p, &[hold("a")], &now_rfc3339()).unwrap();
        let d = &store.list_decisions(None).unwrap()[0];
        assert_eq!(d.bead_id, None);
        let seq = store.events_after(0, 1000).unwrap().len();
        let with = store.set_decision_bead(&d.id, "qk-29").unwrap();
        assert_eq!(with.bead_id.as_deref(), Some("qk-29"));
        assert_eq!(store.get_decision(&d.id).unwrap(), with);
        assert_eq!(store.events_after(0, 1000).unwrap().len(), seq + 1);
        store.set_decision_bead(&d.id, "qk-29").unwrap();
        assert_eq!(store.events_after(0, 1000).unwrap().len(), seq + 1);
        assert!(matches!(
            store.set_decision_bead("nope", "qk-1"),
            Err(StoreError::NotFound)
        ));
    }
}
