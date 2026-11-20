//! What the native coordinator (slice 6) writes to the store itself: the
//! decisions it raises with `ask_user`, and what it tells the user; and
//! when its conversation is compacted.
//!
//! Its decisions are the daemon's own (no engine holds them), so a refresh
//! never closes one; an answer goes back to the coordinator through the
//! event log instead of the engine (`api::routes::answer`).

use quark_systems::{
    CompactionRecord, CompactionSettings, Decision, DecisionBrief, DecisionState, EventType,
    TranscriptEntry, TranscriptRole,
};
use rusqlite::{params, OptionalExtension};

use super::{
    api_task_ids, append_event, decision_from_row, decision_from_row_at, insert_decision,
    mark_answered, new_id, number_if_unset, Result, Store, StoreError, COORDINATOR,
    DECISION_COLUMNS, DECISION_SELECT,
};

/// `a` then each of `b` not in it.
fn merged(a: &[String], b: &[String]) -> Vec<String> {
    let mut out = a.to_vec();
    for x in b {
        if !out.contains(x) {
            out.push(x.clone());
        }
    }
    out
}
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
    ///
    /// `answers` names a worker's question (`<task>:<key>`, the engine's id
    /// for it) the decision frames. That question's own decision takes the
    /// coordinator's question and brief in place, and opens if it was still
    /// with the coordinator, so the person's answer goes to the worker. A
    /// question that blocks a task with a pending question of its own frames
    /// that one the same way, so no copy is left to answer twice.
    pub fn ask_from_coordinator(
        &self,
        project_id: &str,
        key: &str,
        question: &str,
        brief: DecisionBrief,
        answers: Option<&str>,
    ) -> Result<Decision> {
        let engine_id = coordinator_decision_id(key);
        self.write(|tx, events| {
            let brief = api_task_ids(tx, project_id, brief.normalized())?;
            let pending = |sql_where: &str, a: &str| -> Result<Option<Decision>> {
                Ok(tx
                    .query_row(
                        &format!(
                            "{DECISION_SELECT} WHERE project_id = ?1 AND {sql_where}
                             AND state IN ('open', 'with_coordinator')
                             ORDER BY rowid DESC LIMIT 1"
                        ),
                        params![project_id, a],
                        decision_from_row,
                    )
                    .optional()?)
            };
            let mut worker = match answers {
                Some(hold) => {
                    Some(pending("engine_id = ?2", hold)?.ok_or_else(|| StoreError::NotFound)?)
                }
                None => None,
            };
            if worker.is_none() {
                for task in &brief.blocks {
                    if let Some(d) = pending("task_id = ?2", task)? {
                        worker = Some(d);
                        break;
                    }
                }
            }
            if let Some(old) = worker {
                let mut d = Decision {
                    question: question.to_string(),
                    brief: DecisionBrief {
                        blocks: merged(&old.brief.blocks, &brief.blocks),
                        ..brief
                    },
                    state: DecisionState::Open,
                    ..old
                };
                number_if_unset(tx, &mut d)?;
                tx.execute(
                    "UPDATE decisions SET question = ?2, brief = ?3, state = 'open' WHERE id = ?1",
                    params![d.id, d.question, serde_json::to_string(&d.brief)?],
                )?;
                append_event(
                    tx,
                    events,
                    Some(project_id),
                    EventType::DecisionOpened,
                    serde_json::to_value(&d)?,
                )?;
                return Ok(d);
            }
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

    /// Records the coordinator's own answer to a worker's question (`hold`,
    /// the engine's id for it) on that question's decision, so the log
    /// shows the call it made. `None` when no decision is pending for it.
    pub fn coordinator_answered(
        &self,
        project_id: &str,
        hold: &str,
        answer: &str,
        answered_by: &str,
    ) -> Result<Option<Decision>> {
        self.write(|tx, events| {
            let Some(old) = tx
                .query_row(
                    &format!(
                        "{DECISION_SELECT} WHERE project_id = ?1 AND engine_id = ?2
                         AND state IN ('open', 'with_coordinator')
                         ORDER BY rowid DESC LIMIT 1"
                    ),
                    params![project_id, hold],
                    decision_from_row,
                )
                .optional()?
            else {
                return Ok(None);
            };
            let mut d = Decision {
                state: DecisionState::Answered,
                answer: Some(answer.to_string()),
                answered_by: Some(answered_by.to_string()),
                answered_at: Some(now_rfc3339()),
                answered_via: Some(COORDINATOR.into()),
                ..old
            };
            number_if_unset(tx, &mut d)?;
            mark_answered(tx, events, &d)?;
            Ok(Some(d))
        })
    }

    /// Withdraws the coordinator's own open decision `number`: the question
    /// no longer needs the person (another decision covers it, or the work
    /// moved on). Closed in the log with `reason`, and nothing is sent
    /// anywhere. A worker's question is closed by answering the worker
    /// instead, so this refuses one.
    pub fn withdraw_decision(
        &self,
        project_id: &str,
        number: i64,
        reason: &str,
    ) -> Result<Decision> {
        self.write(|tx, events| {
            let (engine_id, old): (String, Decision) = tx
                .query_row(
                    &format!(
                        "SELECT engine_id, {DECISION_COLUMNS} FROM decisions
                         WHERE project_id = ?1 AND number = ?2
                         ORDER BY rowid DESC LIMIT 1"
                    ),
                    params![project_id, number],
                    |r| Ok((r.get(0)?, decision_from_row_at(r, 1)?)),
                )
                .optional()?
                .ok_or(StoreError::NotFound)?;
            if old.state != DecisionState::Open {
                return Err(StoreError::Conflict(format!(
                    "decision {number} is not open"
                )));
            }
            if coordinator_decision_key(&engine_id).is_none() {
                return Err(StoreError::Conflict(format!(
                    "decision {number} is a worker's question: answer the worker with `answer` \
                     (its task and key), which closes it, or reframe it with ask_user `answers`"
                )));
            }
            let d = Decision {
                state: DecisionState::Answered,
                answered_by: Some(COORDINATOR.into()),
                answered_at: Some(now_rfc3339()),
                answered_via: Some(COORDINATOR.into()),
                answer_why: Some(format!("Withdrawn: {}", reason.trim())),
                ..old
            };
            mark_answered(tx, events, &d)?;
            Ok(d)
        })
    }

    /// Worker questions still with the coordinator, and every open decision
    /// a person sees, each with the engine's id for it, oldest first.
    pub fn pending_for_coordinator(&self, project_id: &str) -> Result<Vec<(String, Decision)>> {
        self.read(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT engine_id, {DECISION_COLUMNS} FROM decisions
                 WHERE project_id = ?1 AND state IN ('open', 'with_coordinator')
                 ORDER BY opened_at, rowid"
            ))?;
            let rows = stmt.query_map([project_id], |r| {
                Ok((r.get(0)?, decision_from_row_at(r, 1)?))
            })?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
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
            turn_end: false,
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

impl Store {
    /// The Project's compaction settings; `default` when it never set its
    /// own.
    pub fn compaction_settings(
        &self,
        project_id: &str,
        default: CompactionSettings,
    ) -> Result<CompactionSettings> {
        self.read(|c| {
            Ok(c.query_row(
                "SELECT auto, at_percent FROM coordinator_compaction WHERE project_id = ?1",
                params![project_id],
                |r| {
                    Ok(CompactionSettings {
                        auto: r.get(0)?,
                        at_percent: r.get::<_, i64>(1)?.clamp(0, 100) as u8,
                    })
                },
            )
            .optional()?
            .unwrap_or(default))
        })
    }

    /// Set the Project's compaction settings.
    pub fn set_compaction_settings(&self, project_id: &str, s: CompactionSettings) -> Result<()> {
        self.write(|tx, _| {
            tx.execute(
                "INSERT INTO coordinator_compaction
                   (project_id, auto, at_tokens, at_percent, updated_at)
                 VALUES (?1, ?2, 0, ?3, ?4)
                 ON CONFLICT(project_id) DO UPDATE SET
                   auto = excluded.auto, at_percent = excluded.at_percent,
                   updated_at = excluded.updated_at",
                params![project_id, s.auto, s.at_percent, now_rfc3339()],
            )?;
            Ok(())
        })
    }

    /// The newest compaction the coordinator's conversation recorded.
    pub fn last_coordinator_compaction(
        &self,
        project_id: &str,
    ) -> Result<Option<CompactionRecord>> {
        self.read(|c| {
            Ok(c.query_row(
                "SELECT json_extract(payload, '$.entry.ts'), json_extract(payload, '$.entry.text')
                 FROM events
                 WHERE type = ?1 AND json_extract(payload, '$.coordinator_id') = ?2
                   AND json_extract(payload, '$.entry.role') = 'compaction'
                 ORDER BY seq DESC LIMIT 1",
                params![EventType::CoordinatorMessage.as_str(), project_id],
                |r| {
                    Ok(CompactionRecord {
                        at: r.get(0)?,
                        text: r.get(1)?,
                    })
                },
            )
            .optional()?)
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
            .ask_from_coordinator(&p, "storage", "Which database?", brief.clone(), None)
            .unwrap();
        assert_eq!(d.state, DecisionState::Open);
        let same = store
            .ask_from_coordinator(&p, "storage", "Which database?", brief.clone(), None)
            .unwrap();
        assert_eq!(same, d);
        let again = store
            .ask_from_coordinator(&p, "storage", "Which database for v1?", brief, None)
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
            .ask_from_coordinator(
                &p,
                "storage",
                "Which database?",
                DecisionBrief::default(),
                None,
            )
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
                Some(0),
                10,
            )
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].entry.text, "PR #12 merged.");
        assert_eq!(items[0].entry.role, TranscriptRole::Assistant);
    }

    #[test]
    fn an_opening_chat_gets_the_newest_messages() {
        let store = Store::open_in_memory().unwrap();
        let p = project(&store);
        for n in 1..=5 {
            store.coordinator_says(&p, &format!("message {n}")).unwrap();
        }
        let source = TranscriptSource::Coordinator {
            project_id: p.clone(),
        };
        let text = |items: Vec<quark_systems::TranscriptItem>| -> Vec<String> {
            items.into_iter().map(|i| i.entry.text).collect()
        };
        // No cursor: the newest two, oldest first.
        let newest = store.transcript(&source, None, 2).unwrap();
        let first_new = newest[0].id;
        assert_eq!(text(newest), ["message 4", "message 5"]);
        // A cursor pages forward from it.
        assert_eq!(
            text(store.transcript(&source, Some(0), 2).unwrap()),
            ["message 1", "message 2"]
        );
        assert_eq!(
            text(store.transcript(&source, Some(first_new), 10).unwrap()),
            ["message 5"]
        );
    }

    #[test]
    fn compaction_settings_and_the_last_compaction() {
        let store = Store::open_in_memory().unwrap();
        let p = project(&store);
        let default = CompactionSettings {
            auto: true,
            at_percent: 70,
        };
        assert_eq!(store.compaction_settings(&p, default).unwrap(), default);
        let off = CompactionSettings {
            auto: false,
            at_percent: 50,
        };
        store.set_compaction_settings(&p, off).unwrap();
        assert_eq!(store.compaction_settings(&p, default).unwrap(), off);

        assert_eq!(store.last_coordinator_compaction(&p).unwrap(), None);
        let entry = |role, text: &str, ts: &str| TranscriptEntry {
            role,
            text: text.into(),
            tool_name: None,
            tool_call_id: None,
            is_error: false,
            truncated: false,
            ts: Some(ts.into()),
            tool: None,
            turn_end: false,
        };
        let source = TranscriptSource::Coordinator {
            project_id: p.clone(),
        };
        store
            .apply_transcript(
                &p,
                &source,
                "/log",
                10,
                &[
                    entry(TranscriptRole::Compaction, "first", "2026-10-23T09:00:00Z"),
                    entry(TranscriptRole::Compaction, "second", "2026-10-23T10:00:00Z"),
                    entry(TranscriptRole::User, "hi", "2026-10-23T10:01:00Z"),
                ],
            )
            .unwrap();
        let last = store.last_coordinator_compaction(&p).unwrap().unwrap();
        assert_eq!(last.text, "second");
        assert_eq!(last.at.as_deref(), Some("2026-10-23T10:00:00Z"));
    }
}
