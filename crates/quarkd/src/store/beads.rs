//! New issue drafts (`issue_drafts`) and events about a Project's Beads
//! database. The beads themselves live in Beads (`crate::beads`); only the
//! drafts, which exist before any bead does, are kept here.

use quark_systems::{DraftMessage, DraftRole, EventType, IssueDraft, IssueDraftState};
use rusqlite::{params, OptionalExtension};

use super::{append_event, get_project, new_id, Result, Store, StoreError};
use crate::now_rfc3339;

fn state_str(s: IssueDraftState) -> &'static str {
    match s {
        IssueDraftState::Open => "open",
        IssueDraftState::Accepted => "accepted",
        IssueDraftState::Discarded => "discarded",
    }
}

impl Store {
    /// Records an event that has no row of its own behind it, such as a
    /// Beads database changing.
    pub fn emit(
        &self,
        project_id: Option<&str>,
        event_type: EventType,
        payload: serde_json::Value,
    ) -> Result<()> {
        self.write(|tx, events| append_event(tx, events, project_id, event_type, payload))
    }

    /// Opens a draft with the user's first message, waiting for the
    /// coordinator's drafts.
    pub fn create_issue_draft(&self, project_id: &str, text: &str) -> Result<IssueDraft> {
        self.write(|tx, events| {
            get_project(tx, project_id)?;
            let now = now_rfc3339();
            let draft = IssueDraft {
                id: new_id("draft"),
                project_id: project_id.to_string(),
                state: IssueDraftState::Open,
                messages: vec![DraftMessage {
                    role: DraftRole::User,
                    text: text.to_string(),
                    at: now.clone(),
                }],
                issues: Vec::new(),
                related: Vec::new(),
                waiting: true,
                created: Default::default(),
                created_at: now.clone(),
                updated_at: now,
            };
            tx.execute(
                "INSERT INTO issue_drafts (id, project_id, state, body, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    draft.id,
                    draft.project_id,
                    state_str(draft.state),
                    serde_json::to_string(&draft)?,
                    draft.created_at,
                    draft.updated_at
                ],
            )?;
            append_event(
                tx,
                events,
                Some(project_id),
                EventType::IssueDraftUpdated,
                serde_json::to_value(&draft)?,
            )?;
            Ok(draft)
        })
    }

    pub fn get_issue_draft(&self, id: &str) -> Result<IssueDraft> {
        self.read(|c| {
            let body: Option<String> = c
                .query_row("SELECT body FROM issue_drafts WHERE id = ?1", [id], |r| {
                    r.get(0)
                })
                .optional()?;
            Ok(serde_json::from_str(&body.ok_or(StoreError::NotFound)?)?)
        })
    }

    /// A Project's drafts in `state`, oldest first.
    pub fn list_issue_drafts(
        &self,
        project_id: &str,
        state: IssueDraftState,
    ) -> Result<Vec<IssueDraft>> {
        self.read(|c| {
            get_project(c, project_id)?;
            let mut stmt = c.prepare(
                "SELECT body FROM issue_drafts WHERE project_id = ?1 AND state = ?2
                 ORDER BY created_at, id",
            )?;
            let rows = stmt.query_map(params![project_id, state_str(state)], |r| {
                r.get::<_, String>(0)
            })?;
            let mut out = Vec::new();
            for body in rows {
                out.push(serde_json::from_str(&body?)?);
            }
            Ok(out)
        })
    }

    /// Changes an open draft with `f` and emits `issue_draft.updated`. A
    /// draft that is no longer open is a conflict.
    pub fn update_issue_draft(
        &self,
        id: &str,
        f: impl FnOnce(&mut IssueDraft) -> Result<()>,
    ) -> Result<IssueDraft> {
        self.write(|tx, events| {
            let body: Option<String> = tx
                .query_row("SELECT body FROM issue_drafts WHERE id = ?1", [id], |r| {
                    r.get(0)
                })
                .optional()?;
            let mut draft: IssueDraft = serde_json::from_str(&body.ok_or(StoreError::NotFound)?)?;
            if draft.state != IssueDraftState::Open {
                return Err(StoreError::Conflict("the draft is already closed".into()));
            }
            f(&mut draft)?;
            draft.updated_at = now_rfc3339();
            tx.execute(
                "UPDATE issue_drafts SET state = ?2, body = ?3, updated_at = ?4 WHERE id = ?1",
                params![
                    id,
                    state_str(draft.state),
                    serde_json::to_string(&draft)?,
                    draft.updated_at
                ],
            )?;
            append_event(
                tx,
                events,
                Some(&draft.project_id),
                EventType::IssueDraftUpdated,
                serde_json::to_value(&draft)?,
            )?;
            Ok(draft)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quark_systems::CreateProject;

    fn store_with_project() -> (Store, String) {
        let s = Store::open_in_memory().unwrap();
        let p = s
            .create_project(CreateProject {
                name: "p".into(),
                ..Default::default()
            })
            .unwrap();
        (s, p.id)
    }

    #[test]
    fn a_draft_opens_waiting_and_closes_once() {
        let (s, pid) = store_with_project();
        let d = s.create_issue_draft(&pid, "PRs that go red").unwrap();
        assert!(d.waiting);
        assert_eq!(
            s.list_issue_drafts(&pid, IssueDraftState::Open).unwrap(),
            vec![d.clone()]
        );

        let closed = s
            .update_issue_draft(&d.id, |d| {
                d.state = IssueDraftState::Discarded;
                Ok(())
            })
            .unwrap();
        assert_eq!(closed.state, IssueDraftState::Discarded);
        assert!(s
            .list_issue_drafts(&pid, IssueDraftState::Open)
            .unwrap()
            .is_empty());
        let again = s.update_issue_draft(&d.id, |_| Ok(()));
        assert!(matches!(again, Err(StoreError::Conflict(_))));
        assert!(matches!(
            s.get_issue_draft("nope"),
            Err(StoreError::NotFound)
        ));
    }
}
