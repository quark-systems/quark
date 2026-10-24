//! Changes of a Project coordinator's agent ([`CoordinatorSwitch`]), each
//! step recorded before its effect, so the switch driver
//! ([`crate::switch`]) picks up after a crash where it stopped. Committing
//! the new agent and putting the old one back each change the Project in
//! the same transaction as the step.

use quark_systems::{AgentConfig, CoordinatorSwitch, EventType, SwitchKind, SwitchState};
use rusqlite::{params, OptionalExtension, Row, Transaction};

use super::{append_event, get_project, new_id, spec_json, Result, Store, StoreError};
use crate::now_rfc3339;

/// A switch with what only the driver needs.
#[derive(Debug, Clone, PartialEq)]
pub struct SwitchRow {
    pub switch: CoordinatorSwitch,
    /// The event log position when it was asked for.
    pub after_seq: i64,
    /// When the wrap up's turn ended.
    pub wrapped_at: Option<String>,
}

const SELECT: &str = "SELECT id, project_id, from_agent, to_agent, kind, state, after_seq, \
                      wrapped_at, brief, error, requested_at, updated_at \
                      FROM coordinator_switches";

fn row(r: &Row) -> rusqlite::Result<SwitchRow> {
    let json = |i: usize| -> rusqlite::Result<Option<AgentConfig>> {
        Ok(r.get::<_, Option<String>>(i)?
            .and_then(|s| serde_json::from_str(&s).ok()))
    };
    let to = json(3)?.unwrap_or(AgentConfig {
        harness: String::new(),
        model: None,
        effort: None,
        pool: None,
    });
    Ok(SwitchRow {
        switch: CoordinatorSwitch {
            id: r.get(0)?,
            project_id: r.get(1)?,
            from: json(2)?,
            to,
            kind: if r.get::<_, String>(4)? == "resume" {
                SwitchKind::Resume
            } else {
                SwitchKind::Handoff
            },
            state: SwitchState::parse(&r.get::<_, String>(5)?).unwrap_or(SwitchState::Failed),
            brief: r.get(8)?,
            error: r.get(9)?,
            requested_at: r.get(10)?,
            updated_at: r.get(11)?,
        },
        after_seq: r.get(6)?,
        wrapped_at: r.get(7)?,
    })
}

fn kind_str(k: SwitchKind) -> &'static str {
    match k {
        SwitchKind::Resume => "resume",
        SwitchKind::Handoff => "handoff",
    }
}

fn get(tx: &Transaction, id: &str) -> Result<SwitchRow> {
    tx.query_row(&format!("{SELECT} WHERE id = ?1"), [id], row)
        .optional()?
        .ok_or(StoreError::NotFound)
}

/// Set the Project's agent config, emitting `project.updated`.
fn set_agent(
    tx: &Transaction,
    events: &mut Vec<quark_systems::Event>,
    project_id: &str,
    agent: Option<AgentConfig>,
) -> Result<()> {
    let mut project = get_project(tx, project_id)?;
    project.agent_config = agent;
    project.updated_at = now_rfc3339();
    tx.execute(
        "UPDATE projects SET spec = ?2, updated_at = ?3 WHERE id = ?1",
        params![project.id, spec_json(&project)?, project.updated_at],
    )?;
    append_event(
        tx,
        events,
        Some(project_id),
        EventType::ProjectUpdated,
        serde_json::to_value(&project)?,
    )
}

impl Store {
    /// Record a switch of the Project's coordinator to `to`. Refused while
    /// another is open, and when `to` is the agent it already runs.
    pub fn start_switch(
        &self,
        project_id: &str,
        to: &AgentConfig,
        kind: SwitchKind,
    ) -> Result<CoordinatorSwitch> {
        self.write(|tx, _| {
            let project = get_project(tx, project_id)?;
            let open: Option<String> = tx
                .query_row(
                    "SELECT id FROM coordinator_switches
                     WHERE project_id = ?1 AND state NOT IN ('done', 'failed')",
                    [project_id],
                    |r| r.get(0),
                )
                .optional()?;
            if open.is_some() {
                return Err(StoreError::Conflict(
                    "the coordinator is already switching agents".into(),
                ));
            }
            if project.agent_config.as_ref() == Some(to) {
                return Err(StoreError::Invalid(
                    "the coordinator already runs this agent".into(),
                ));
            }
            let after: i64 =
                tx.query_row("SELECT COALESCE(MAX(seq), 0) FROM events", [], |r| r.get(0))?;
            let (id, now) = (new_id("sw"), now_rfc3339());
            tx.execute(
                "INSERT INTO coordinator_switches
                   (id, project_id, from_agent, to_agent, kind, state, after_seq,
                    requested_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'requested', ?6, ?7, ?7)",
                params![
                    id,
                    project_id,
                    project
                        .agent_config
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()?,
                    serde_json::to_string(to)?,
                    kind_str(kind),
                    after,
                    now
                ],
            )?;
            Ok(get(tx, &id)?.switch)
        })
    }

    /// Every switch with steps left, oldest first.
    pub fn open_switches(&self) -> Result<Vec<SwitchRow>> {
        self.read(|c| {
            let mut stmt = c.prepare(&format!(
                "{SELECT} WHERE state NOT IN ('done', 'failed') ORDER BY requested_at"
            ))?;
            let rows = stmt.query_map([], row)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }

    /// The Project's newest switch.
    pub fn latest_switch(&self, project_id: &str) -> Result<Option<CoordinatorSwitch>> {
        self.read(|c| {
            Ok(c.query_row(
                &format!(
                    "{SELECT} WHERE project_id = ?1 ORDER BY requested_at DESC, rowid DESC LIMIT 1"
                ),
                [project_id],
                row,
            )
            .optional()?
            .map(|r| r.switch))
        })
    }

    /// Projects whose coordinator is mid-switch: recovery leaves them to
    /// the switch driver.
    pub fn switching_projects(&self) -> Result<Vec<String>> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT DISTINCT project_id FROM coordinator_switches
                 WHERE state NOT IN ('done', 'failed')",
            )?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }

    /// Record the switch's next step.
    pub fn set_switch_state(&self, id: &str, state: SwitchState) -> Result<()> {
        self.write(|tx, _| {
            tx.execute(
                "UPDATE coordinator_switches SET state = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, state.as_str(), now_rfc3339()],
            )?;
            Ok(())
        })
    }

    /// A coordinator turn ended: a switch waiting on its wrap up may go on.
    pub fn switch_turn_ended(&self, project_id: &str) -> Result<()> {
        self.write(|tx, _| {
            tx.execute(
                "UPDATE coordinator_switches SET wrapped_at = ?2
                 WHERE project_id = ?1 AND state = 'wrapping_up' AND wrapped_at IS NULL",
                params![project_id, now_rfc3339()],
            )?;
            Ok(())
        })
    }

    /// The commit point: the Project runs the new agent from now on, and
    /// the switch moves to `switching` with `brief`.
    pub fn commit_switch(&self, id: &str, brief: Option<&str>) -> Result<CoordinatorSwitch> {
        self.write(|tx, events| {
            let s = get(tx, id)?.switch;
            set_agent(tx, events, &s.project_id, Some(s.to.clone()))?;
            tx.execute(
                "UPDATE coordinator_switches SET state = 'switching', brief = ?2, updated_at = ?3
                 WHERE id = ?1",
                params![id, brief, now_rfc3339()],
            )?;
            Ok(get(tx, id)?.switch)
        })
    }

    /// The new agent could not start: put the old one back and record why.
    pub fn fail_switch(&self, id: &str, error: &str) -> Result<CoordinatorSwitch> {
        self.write(|tx, events| {
            let s = get(tx, id)?.switch;
            if get_project(tx, &s.project_id)?.agent_config.as_ref() != s.from.as_ref() {
                set_agent(tx, events, &s.project_id, s.from.clone())?;
            }
            tx.execute(
                "UPDATE coordinator_switches SET state = 'failed', error = ?2, updated_at = ?3
                 WHERE id = ?1",
                params![id, error, now_rfc3339()],
            )?;
            Ok(get(tx, id)?.switch)
        })
    }

    /// One switch, by id.
    pub fn switch_row(&self, id: &str) -> Result<SwitchRow> {
        self.read(|c| {
            c.query_row(&format!("{SELECT} WHERE id = ?1"), [id], row)
                .optional()?
                .ok_or(StoreError::NotFound)
        })
    }

    /// The wrap up note was typed: a turn that ended before it is not the
    /// wrap up's, and the timeout runs from now.
    pub fn switch_noted(&self, id: &str) -> Result<()> {
        self.write(|tx, _| {
            tx.execute(
                "UPDATE coordinator_switches SET wrapped_at = NULL, updated_at = ?2
                 WHERE id = ?1 AND state = 'wrapping_up'",
                params![id, now_rfc3339()],
            )?;
            Ok(())
        })
    }

    /// The coordinator's last few replies recorded after `seq`, newest first.
    pub fn coordinator_replies_after(&self, project_id: &str, seq: i64) -> Result<Vec<String>> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT json_extract(payload, '$.entry.text') FROM events
                 WHERE type = ?1 AND json_extract(payload, '$.coordinator_id') = ?2
                   AND json_extract(payload, '$.entry.role') = 'assistant' AND seq > ?3
                 ORDER BY seq DESC LIMIT 5",
            )?;
            let rows = stmt.query_map(
                params![EventType::CoordinatorMessage.as_str(), project_id, seq],
                |r| r.get(0),
            )?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }
}

#[cfg(test)]
mod tests {
    use quark_systems::{CreateProject, TranscriptEntry, TranscriptRole};

    use super::*;
    use crate::store::TranscriptSource;

    fn agent(model: &str) -> AgentConfig {
        AgentConfig {
            harness: "claude-code".into(),
            model: Some(model.into()),
            effort: None,
            pool: None,
        }
    }

    fn project(store: &Store) -> String {
        store
            .create_project(CreateProject {
                name: "Quark".into(),
                workspace_path: Some("/ws".into()),
                agent_config: Some(agent("sonnet")),
                ..Default::default()
            })
            .unwrap()
            .id
    }

    #[test]
    fn a_switch_commits_the_new_agent_and_a_failure_puts_the_old_one_back() {
        let store = Store::open_in_memory().unwrap();
        let p = project(&store);
        assert!(matches!(
            store.start_switch(&p, &agent("sonnet"), SwitchKind::Resume),
            Err(StoreError::Invalid(_))
        ));
        let s = store
            .start_switch(&p, &agent("opus"), SwitchKind::Resume)
            .unwrap();
        assert_eq!(s.state, SwitchState::Requested);
        assert_eq!(s.from, Some(agent("sonnet")));
        assert!(matches!(
            store.start_switch(&p, &agent("haiku"), SwitchKind::Resume),
            Err(StoreError::Conflict(_))
        ));
        assert_eq!(store.switching_projects().unwrap(), vec![p.clone()]);

        let s = store.commit_switch(&s.id, Some("brief")).unwrap();
        assert_eq!(s.state, SwitchState::Switching);
        assert_eq!(s.brief.as_deref(), Some("brief"));
        assert_eq!(
            store.get_project(&p).unwrap().agent_config,
            Some(agent("opus"))
        );

        let s = store.fail_switch(&s.id, "would not start").unwrap();
        assert_eq!(s.state, SwitchState::Failed);
        assert_eq!(
            store.get_project(&p).unwrap().agent_config,
            Some(agent("sonnet"))
        );
        assert!(store.open_switches().unwrap().is_empty());
        assert_eq!(store.latest_switch(&p).unwrap().unwrap().id, s.id);
    }

    #[test]
    fn the_wrap_up_turn_and_the_reply_after_it_are_found() {
        let store = Store::open_in_memory().unwrap();
        let p = project(&store);
        let source = TranscriptSource::Coordinator {
            project_id: p.clone(),
        };
        let said = |text: &str| TranscriptEntry {
            role: TranscriptRole::Assistant,
            text: text.into(),
            tool_name: None,
            tool_call_id: None,
            is_error: false,
            truncated: false,
            ts: None,
            tool: None,
        };
        store
            .apply_transcript(&p, &source, "/log", 1, &[said("before")])
            .unwrap();
        let s = store
            .start_switch(&p, &agent("opus"), SwitchKind::Handoff)
            .unwrap();
        let row = || store.open_switches().unwrap().remove(0);
        assert!(store
            .coordinator_replies_after(&p, row().after_seq)
            .unwrap()
            .is_empty());
        // Only a switch waiting on its wrap up is marked.
        store.switch_turn_ended(&p).unwrap();
        assert_eq!(row().wrapped_at, None);
        store
            .set_switch_state(&s.id, SwitchState::WrappingUp)
            .unwrap();
        store.switch_turn_ended(&p).unwrap();
        assert!(row().wrapped_at.is_some());
        // Typing the note drops a turn end from before it.
        store.switch_noted(&s.id).unwrap();
        assert_eq!(row().wrapped_at, None);
        store
            .apply_transcript(&p, &source, "/log", 2, &[said("one"), said("the brief")])
            .unwrap();
        assert_eq!(
            store
                .coordinator_replies_after(&p, row().after_seq)
                .unwrap(),
            vec!["the brief".to_string(), "one".into()]
        );
    }
}
