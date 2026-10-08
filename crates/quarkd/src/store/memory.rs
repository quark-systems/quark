//! Memory proposals (journey J8): `memory_proposals`.
//!
//! A `learned` status line on a finished task becomes one proposal
//! (`memory.proposed`); accepting or rejecting it emits `memory.accepted` or
//! `memory.rejected` in the same transaction. Accepted entries themselves
//! live in the Project repo (`crate::memory`).

use quark_systems::{
    EventType, MemoryEntry, MemoryEvidence, MemoryProposal, MemoryProposalState, MemorySource,
    TaskState,
};
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};

use super::{append_event, new_id, Result, Store, StoreError};
use crate::memory::LEARNED_VERB;
use crate::now_rfc3339;

/// A `learned` line on a finished task that has no proposal yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingLearning {
    pub task_event_id: i64,
    pub task_id: String,
    pub task_title: String,
    pub pull_request_url: Option<String>,
    pub worktree: Option<String>,
    pub note: String,
    pub raw: String,
}

/// A proposal to record.
#[derive(Debug, Clone, PartialEq)]
pub struct NewProposal {
    /// The status line it came from; a line is proposed once.
    pub task_event_id: Option<i64>,
    pub task_id: Option<String>,
    pub text: String,
    pub evidence: MemoryEvidence,
    pub source: MemorySource,
}

/// Tasks whose worker has finished: learnings are proposed from then on.
const FINISHED: [TaskState; 3] = [TaskState::InReview, TaskState::Done, TaskState::Failed];

const PROPOSAL_SELECT: &str = "SELECT id, project_id, text, evidence, source, state, proposed_at, \
                               decided_at, decided_by, entry FROM memory_proposals";

fn state_str(s: MemoryProposalState) -> &'static str {
    match s {
        MemoryProposalState::Proposed => "proposed",
        MemoryProposalState::Accepted => "accepted",
        MemoryProposalState::Rejected => "rejected",
    }
}

fn source_str(s: MemorySource) -> &'static str {
    match s {
        MemorySource::Worker => "worker",
        MemorySource::Coordinator => "coordinator",
    }
}

/// A JSON column; an unreadable one is a storage fault, reported as such.
fn json<T: serde::de::DeserializeOwned>(r: &Row, i: usize) -> rusqlite::Result<T> {
    let text: String = r.get(i)?;
    serde_json::from_str(&text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn proposal_from_row(r: &Row) -> rusqlite::Result<MemoryProposal> {
    let entry: Option<String> = r.get(9)?;
    Ok(MemoryProposal {
        id: r.get(0)?,
        project_id: r.get(1)?,
        text: r.get(2)?,
        evidence: json(r, 3)?,
        source: match r.get::<_, String>(4)?.as_str() {
            "coordinator" => MemorySource::Coordinator,
            _ => MemorySource::Worker,
        },
        state: match r.get::<_, String>(5)?.as_str() {
            "accepted" => MemoryProposalState::Accepted,
            "rejected" => MemoryProposalState::Rejected,
            _ => MemoryProposalState::Proposed,
        },
        proposed_at: r.get(6)?,
        decided_at: r.get(7)?,
        decided_by: r.get(8)?,
        entry: match entry {
            Some(e) => Some(serde_json::from_str(&e).map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(
                    9,
                    rusqlite::types::Type::Text,
                    Box::new(err),
                )
            })?),
            None => None,
        },
    })
}

fn get_proposal(c: &Connection, project_id: &str, id: &str) -> Result<MemoryProposal> {
    c.query_row(
        &format!("{PROPOSAL_SELECT} WHERE id = ?1 AND project_id = ?2"),
        params![id, project_id],
        proposal_from_row,
    )
    .optional()?
    .ok_or(StoreError::NotFound)
}

fn require_project(c: &Connection, project_id: &str) -> Result<()> {
    c.query_row("SELECT 1 FROM projects WHERE id = ?1", [project_id], |_| {
        Ok(())
    })
    .optional()?
    .ok_or(StoreError::NotFound)
}

impl Store {
    /// `learned` lines of the Project's finished tasks that have no proposal
    /// yet, oldest first.
    pub fn pending_learnings(&self, project_id: &str) -> Result<Vec<PendingLearning>> {
        self.read(|c| {
            let finished: Vec<String> = FINISHED
                .iter()
                .map(|s| format!("'{}'", s.as_str()))
                .collect();
            let mut stmt = c.prepare(&format!(
                "SELECT e.id, t.id, t.title, t.pull_request_url, t.worktree_path, e.note, e.raw
                 FROM task_events e JOIN tasks t ON t.id = e.task_id
                 WHERE e.project_id = ?1 AND e.kind = ?2 AND TRIM(e.note) <> ''
                   AND t.state IN ({})
                   AND NOT EXISTS (SELECT 1 FROM memory_proposals m WHERE m.task_event_id = e.id)
                 ORDER BY e.id",
                finished.join(", ")
            ))?;
            let rows = stmt.query_map(params![project_id, LEARNED_VERB], |r| {
                Ok(PendingLearning {
                    task_event_id: r.get(0)?,
                    task_id: r.get(1)?,
                    task_title: r.get(2)?,
                    pull_request_url: r.get(3)?,
                    worktree: r.get(4)?,
                    note: r.get(5)?,
                    raw: r.get(6)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })
    }

    /// Records a proposal and emits `memory.proposed`. Returns `None` when
    /// its status line was already proposed.
    pub fn propose_memory(
        &self,
        project_id: &str,
        p: NewProposal,
    ) -> Result<Option<MemoryProposal>> {
        self.write(|tx, events| {
            let proposal = MemoryProposal {
                id: new_id("mpr"),
                project_id: project_id.to_string(),
                text: p.text,
                evidence: p.evidence,
                source: p.source,
                state: MemoryProposalState::Proposed,
                proposed_at: now_rfc3339(),
                decided_at: None,
                decided_by: None,
                entry: None,
            };
            let inserted = tx.execute(
                "INSERT INTO memory_proposals (id, project_id, task_id, task_event_id, text,
                     evidence, source, state, proposed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT (task_event_id) DO NOTHING",
                params![
                    proposal.id,
                    proposal.project_id,
                    p.task_id,
                    p.task_event_id,
                    proposal.text,
                    serde_json::to_string(&proposal.evidence)?,
                    source_str(proposal.source),
                    state_str(proposal.state),
                    proposal.proposed_at,
                ],
            )?;
            if inserted == 0 {
                return Ok(None);
            }
            append_event(
                tx,
                events,
                Some(project_id),
                EventType::MemoryProposed,
                serde_json::to_value(&proposal)?,
            )?;
            Ok(Some(proposal))
        })
    }

    /// A Project's proposals, oldest first, optionally in one state.
    pub fn list_memory_proposals(
        &self,
        project_id: &str,
        state: Option<MemoryProposalState>,
    ) -> Result<Vec<MemoryProposal>> {
        self.read(|c| {
            require_project(c, project_id)?;
            let mut stmt = c.prepare(&format!(
                "{PROPOSAL_SELECT} WHERE project_id = ?1 AND (?2 IS NULL OR state = ?2)
                 ORDER BY proposed_at, id"
            ))?;
            let rows =
                stmt.query_map(params![project_id, state.map(state_str)], proposal_from_row)?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })
    }

    pub fn get_memory_proposal(&self, project_id: &str, id: &str) -> Result<MemoryProposal> {
        self.read(|c| get_proposal(c, project_id, id))
    }

    /// Records an accepted proposal with the text kept and the entry
    /// committed for it, emitting `memory.accepted`. Conflicts when the
    /// proposal is already decided.
    pub fn accept_memory_proposal(
        &self,
        project_id: &str,
        id: &str,
        decided_by: &str,
        entry: MemoryEntry,
    ) -> Result<MemoryProposal> {
        self.write(|tx, events| {
            let old = undecided(tx, project_id, id)?;
            let proposal = MemoryProposal {
                text: entry.text.clone(),
                state: MemoryProposalState::Accepted,
                decided_at: entry.accepted_at.clone(),
                decided_by: Some(decided_by.to_string()),
                entry: Some(entry),
                ..old
            };
            decide(tx, events, &proposal, EventType::MemoryAccepted)?;
            Ok(proposal)
        })
    }

    /// Rejects a proposal, emitting `memory.rejected`. Conflicts when it is
    /// already decided.
    pub fn reject_memory_proposal(
        &self,
        project_id: &str,
        id: &str,
        decided_by: &str,
    ) -> Result<MemoryProposal> {
        self.write(|tx, events| {
            let old = undecided(tx, project_id, id)?;
            let proposal = MemoryProposal {
                state: MemoryProposalState::Rejected,
                decided_at: Some(now_rfc3339()),
                decided_by: Some(decided_by.to_string()),
                ..old
            };
            decide(tx, events, &proposal, EventType::MemoryRejected)?;
            Ok(proposal)
        })
    }
}

/// The proposal, while it is still awaiting review.
fn undecided(tx: &Transaction, project_id: &str, id: &str) -> Result<MemoryProposal> {
    let p = get_proposal(tx, project_id, id)?;
    if p.state != MemoryProposalState::Proposed {
        return Err(StoreError::Conflict(format!(
            "the proposal is already {}",
            state_str(p.state)
        )));
    }
    Ok(p)
}

fn decide(
    tx: &Transaction,
    events: &mut Vec<quark_systems::Event>,
    p: &MemoryProposal,
    event: EventType,
) -> Result<()> {
    let entry = p.entry.as_ref().map(serde_json::to_string).transpose()?;
    tx.execute(
        "UPDATE memory_proposals SET text = ?2, state = ?3, decided_at = ?4, decided_by = ?5,
             entry = ?6
         WHERE id = ?1",
        params![
            p.id,
            p.text,
            state_str(p.state),
            p.decided_at,
            p.decided_by,
            entry
        ],
    )?;
    append_event(
        tx,
        events,
        Some(&p.project_id),
        event,
        serde_json::to_value(p)?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{EngineTask, FleetSnapshot, StatusEntry};
    use quark_systems::{CreateProject, TaskKind};

    fn setup(state: TaskState) -> (Store, String, String) {
        let store = Store::open_in_memory().unwrap();
        let p = store
            .create_project(CreateProject {
                name: "P".into(),
                goal: None,
                workspace_path: Some("/w".into()),
                repos: vec![],
                agent_config: None,
                dispatch_preset: None,
                delivery: None,
            })
            .unwrap();
        snapshot(&store, &p.id, state);
        let task = store.list_tasks(&p.id).unwrap().remove(0).id;
        (store, p.id, task)
    }

    fn snapshot(store: &Store, project: &str, state: TaskState) {
        store
            .apply_snapshot(
                project,
                &FleetSnapshot {
                    tasks: vec![EngineTask {
                        id: "t1".into(),
                        title: "Fix login".into(),
                        kind: Some(TaskKind::Ship),
                        state,
                        state_note: None,
                        state_source: None,
                        harness: None,
                        pull_request_url: Some("https://github.com/o/r/pull/7".into()),
                        worktree: None,
                        terminal: None,
                    }],
                },
            )
            .unwrap();
    }

    fn line(kind: &str, note: &str) -> StatusEntry {
        StatusEntry {
            kind: kind.into(),
            decision_key: None,
            note: note.into(),
            raw: format!("{kind}: {note}"),
        }
    }

    fn propose_pending(store: &Store, project: &str) -> Vec<MemoryProposal> {
        store
            .pending_learnings(project)
            .unwrap()
            .into_iter()
            .filter_map(|l| {
                store
                    .propose_memory(
                        project,
                        NewProposal {
                            task_event_id: Some(l.task_event_id),
                            task_id: Some(l.task_id),
                            text: l.note,
                            evidence: MemoryEvidence::default(),
                            source: MemorySource::Worker,
                        },
                    )
                    .unwrap()
            })
            .collect()
    }

    #[test]
    fn learnings_wait_for_the_task_to_finish_and_propose_once() {
        let (store, project, task) = setup(TaskState::Running);
        store
            .apply_status(
                &task,
                0,
                &[
                    line("working", "on it"),
                    line("learned", "Keep it small."),
                    line("learned", " "),
                ],
                3,
            )
            .unwrap();
        assert!(store.pending_learnings(&project).unwrap().is_empty());

        snapshot(&store, &project, TaskState::InReview);
        let pending = store.pending_learnings(&project).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].task_title, "Fix login");
        assert_eq!(
            pending[0].pull_request_url.as_deref(),
            Some("https://github.com/o/r/pull/7")
        );

        let seq = store.last_seq().unwrap();
        let made = propose_pending(&store, &project);
        assert_eq!(made.len(), 1);
        assert!(propose_pending(&store, &project).is_empty());
        let events = store.events_after(seq, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, EventType::MemoryProposed);
        assert_eq!(events[0].payload["text"], "Keep it small.");

        // A line added after the task finished is proposed straight away.
        store
            .apply_status(&task, 3, &[line("learned", "Later too.")], 4)
            .unwrap();
        assert_eq!(propose_pending(&store, &project).len(), 1);
        assert_eq!(
            store
                .list_memory_proposals(&project, Some(MemoryProposalState::Proposed))
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn decisions_are_final() {
        let (store, project, task) = setup(TaskState::Done);
        store
            .apply_status(&task, 0, &[line("learned", "a"), line("learned", "b")], 2)
            .unwrap();
        let made = propose_pending(&store, &project);
        let (a, b) = (&made[0].id, &made[1].id);

        let rejected = store.reject_memory_proposal(&project, b, "matt").unwrap();
        assert_eq!(rejected.state, MemoryProposalState::Rejected);
        assert!(matches!(
            store.reject_memory_proposal(&project, b, "matt"),
            Err(StoreError::Conflict(_))
        ));
        assert!(matches!(
            store.reject_memory_proposal("other", a, "matt"),
            Err(StoreError::NotFound)
        ));

        let entry = MemoryEntry {
            id: "2026-10-02-a".into(),
            project_id: project.clone(),
            path: "memory/2026-10-02-a.md".into(),
            text: "a, edited".into(),
            evidence: MemoryEvidence::default(),
            source: Some(MemorySource::Worker),
            date: Some(made[0].proposed_at.clone()),
            accepted_at: Some(now_rfc3339()),
            accepted_by: Some("matt".into()),
            proposal_id: Some(a.clone()),
            commit: Some("abc".into()),
            beads_key: None,
        };
        let accepted = store
            .accept_memory_proposal(&project, a, "matt", entry.clone())
            .unwrap();
        assert_eq!(accepted.text, "a, edited");
        assert_eq!(accepted.entry, Some(entry.clone()));
        assert_eq!(store.get_memory_proposal(&project, a).unwrap(), accepted);
        assert!(matches!(
            store.accept_memory_proposal(&project, a, "matt", entry),
            Err(StoreError::Conflict(_))
        ));
        let types: Vec<_> = store
            .events_after(0, 100)
            .unwrap()
            .into_iter()
            .map(|e| e.event_type)
            .filter(|t| t.as_str().starts_with("memory."))
            .collect();
        assert_eq!(
            types,
            [
                EventType::MemoryProposed,
                EventType::MemoryProposed,
                EventType::MemoryRejected,
                EventType::MemoryAccepted
            ]
        );
    }
}
