//! Messages sent to a coordinator, recorded by the daemon itself.
//!
//! A message is a row of `coordinator_outbox` from the moment it is sent.
//! It waits there as `queued` while the coordinator works (and can be
//! edited or cancelled), then is typed into the coordinator's session. Once
//! typed in, the daemon appends it to the coordinator's transcript as a
//! `user` entry carrying the message's id, so it sits where it was sent
//! whatever the harness's own log does. The harness's copy, read later from
//! its session log, is matched to the row and not shown twice
//! ([`Store::claim_logged_message`]).

use quark_systems::{EventType, OutboxMessage, OutboxState, TranscriptEntry, TranscriptRole};
use rusqlite::{params, OptionalExtension, Row, Transaction};

use super::{append_event, new_id, Event, Result, Store, StoreError};
use crate::now_rfc3339;

const SELECT: &str = "SELECT id, project_id, text, state, error, entry_seq, created_at, updated_at
                      FROM coordinator_outbox";

/// How many unmatched delivered messages a harness entry is compared with.
const MATCH_WINDOW: i64 = 20;

fn from_row(r: &Row) -> rusqlite::Result<OutboxMessage> {
    let state: String = r.get(3)?;
    Ok(OutboxMessage {
        id: r.get(0)?,
        coordinator_id: r.get(1)?,
        text: r.get(2)?,
        state: OutboxState::parse(&state).unwrap_or(OutboxState::Failed),
        error: r.get(4)?,
        entry_id: r.get(5)?,
        created_at: r.get(6)?,
        updated_at: r.get(7)?,
    })
}

fn get(tx: &rusqlite::Connection, id: &str) -> Result<OutboxMessage> {
    tx.query_row(&format!("{SELECT} WHERE id = ?1"), [id], from_row)
        .optional()?
        .ok_or(StoreError::NotFound)
}

fn emit(tx: &Transaction, events: &mut Vec<Event>, m: &OutboxMessage) -> Result<()> {
    append_event(
        tx,
        events,
        Some(&m.coordinator_id),
        EventType::CoordinatorOutbox,
        serde_json::to_value(m)?,
    )
}

fn set_state(
    tx: &Transaction,
    events: &mut Vec<Event>,
    id: &str,
    state: OutboxState,
    error: Option<&str>,
) -> Result<OutboxMessage> {
    tx.execute(
        "UPDATE coordinator_outbox SET state = ?2, error = ?3, updated_at = ?4 WHERE id = ?1",
        params![id, state.as_str(), error, now_rfc3339()],
    )?;
    let m = get(tx, id)?;
    emit(tx, events, &m)?;
    Ok(m)
}

/// Appends the transcript entry recording a typed-in message, and returns
/// its seq.
fn record(
    tx: &Transaction,
    events: &mut Vec<Event>,
    m: &OutboxMessage,
    entry: &TranscriptEntry,
) -> Result<i64> {
    append_event(
        tx,
        events,
        Some(&m.coordinator_id),
        EventType::CoordinatorMessage,
        serde_json::json!({
            "coordinator_id": m.coordinator_id,
            "entry": entry,
            "message_id": m.id,
        }),
    )?;
    let seq = events.last().map(|e| e.seq).unwrap_or_default();
    tx.execute(
        "UPDATE coordinator_outbox SET entry_seq = ?2 WHERE id = ?1",
        params![m.id, seq],
    )?;
    Ok(seq)
}

/// The first line of a message, which a harness's log of it starts with.
fn first_line(text: &str) -> &str {
    text.trim().lines().next().unwrap_or("").trim_end()
}

/// What a harness's copy of an outbox message did to the transcript.
pub(crate) enum Logged {
    /// Not a message the daemon sent: record the entry as it is.
    Unknown,
    /// The daemon's message, not recorded yet: record it with this id.
    Record(OutboxMessage),
    /// The daemon's message, already recorded: skip the copy.
    Duplicate,
}

impl Store {
    /// Records a message for the Project's coordinator: `queued` to wait for
    /// the current turn to end, or `sending` when it is typed in now.
    pub fn new_outbox_message(
        &self,
        project_id: &str,
        text: &str,
        state: OutboxState,
    ) -> Result<OutboxMessage> {
        self.write(|tx, events| {
            let now = now_rfc3339();
            let id = new_id("msg");
            tx.execute(
                "INSERT INTO coordinator_outbox (id, project_id, text, state, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![id, project_id, text, state.as_str(), now],
            )?;
            let m = get(tx, &id)?;
            emit(tx, events, &m)?;
            Ok(m)
        })
    }

    pub fn outbox_message(&self, id: &str) -> Result<OutboxMessage> {
        self.read(|c| get(c, id))
    }

    /// The Project's messages not in its transcript yet: queued, being
    /// typed in, or failed. Oldest first.
    pub fn coordinator_outbox(&self, project_id: &str) -> Result<Vec<OutboxMessage>> {
        self.read(|c| {
            let mut stmt = c.prepare(&format!(
                "{SELECT} WHERE project_id = ?1 AND state IN ('queued', 'sending', 'failed')
                 ORDER BY created_at, rowid"
            ))?;
            let rows = stmt.query_map([project_id], from_row)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }

    /// New text for a queued message. A message already typed in can't change.
    pub fn edit_outbox_message(&self, id: &str, text: &str) -> Result<OutboxMessage> {
        self.write(|tx, events| {
            let m = get(tx, id)?;
            if m.state != OutboxState::Queued {
                return Err(StoreError::Conflict(format!(
                    "the message is {}, not queued",
                    m.state.as_str()
                )));
            }
            tx.execute(
                "UPDATE coordinator_outbox SET text = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, text, now_rfc3339()],
            )?;
            let m = get(tx, id)?;
            emit(tx, events, &m)?;
            Ok(m)
        })
    }

    /// Cancels a queued or failed message, so it is never typed in.
    pub fn cancel_outbox_message(&self, id: &str) -> Result<OutboxMessage> {
        self.write(|tx, events| {
            let m = get(tx, id)?;
            match m.state {
                OutboxState::Queued | OutboxState::Failed => {
                    set_state(tx, events, id, OutboxState::Cancelled, None)
                }
                OutboxState::Cancelled => Ok(m),
                s => Err(StoreError::Conflict(format!(
                    "the message is already {}",
                    s.as_str()
                ))),
            }
        })
    }

    /// Takes a queued or failed message for typing in: it moves to
    /// `sending`. Fails with a conflict when someone else took it first.
    pub fn claim_outbox_message(&self, id: &str) -> Result<OutboxMessage> {
        self.write(|tx, events| {
            let m = get(tx, id)?;
            match m.state {
                OutboxState::Queued | OutboxState::Failed => {
                    set_state(tx, events, id, OutboxState::Sending, None)
                }
                s => Err(StoreError::Conflict(format!(
                    "the message is {}",
                    s.as_str()
                ))),
            }
        })
    }

    /// Takes the Project's oldest queued message for typing in, unless one
    /// of its messages is being typed in already.
    pub fn claim_next_queued(&self, project_id: &str) -> Result<Option<OutboxMessage>> {
        self.write(|tx, events| {
            let busy: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM coordinator_outbox
                                WHERE project_id = ?1 AND state = 'sending')",
                [project_id],
                |r| r.get(0),
            )?;
            if busy {
                return Ok(None);
            }
            let id: Option<String> = tx
                .query_row(
                    "SELECT id FROM coordinator_outbox WHERE project_id = ?1 AND state = 'queued'
                     ORDER BY created_at, rowid LIMIT 1",
                    [project_id],
                    |r| r.get(0),
                )
                .optional()?;
            match id {
                Some(id) => Ok(Some(set_state(
                    tx,
                    events,
                    &id,
                    OutboxState::Sending,
                    None,
                )?)),
                None => Ok(None),
            }
        })
    }

    /// Drops a message typed in at once whose delivery failed: the sender
    /// got the error and keeps the text, so it does not wait in the outbox.
    pub fn abandon_outbox_message(&self, id: &str, error: &str) -> Result<OutboxMessage> {
        self.write(|tx, events| set_state(tx, events, id, OutboxState::Cancelled, Some(error)))
    }

    /// Projects with a queued message.
    pub fn projects_with_queued_messages(&self) -> Result<Vec<String>> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT DISTINCT project_id FROM coordinator_outbox WHERE state = 'queued'",
            )?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }

    /// When the Project's coordinator transcript last changed, as the
    /// RFC 3339 time of its newest entry.
    pub fn coordinator_last_activity(&self, project_id: &str) -> Result<Option<String>> {
        self.read(|c| {
            Ok(c.query_row(
                "SELECT ts FROM events WHERE type = ?1 AND project_id = ?2
                 ORDER BY seq DESC LIMIT 1",
                params![EventType::CoordinatorMessage.as_str(), project_id],
                |r| r.get(0),
            )
            .optional()?)
        })
    }

    /// Records how typing a message in went: `Ok(confirmed)` or the error.
    /// A message typed in joins the transcript now, unless the harness's
    /// copy was already read.
    pub fn finish_outbox_delivery(
        &self,
        id: &str,
        outcome: std::result::Result<bool, String>,
    ) -> Result<OutboxMessage> {
        self.write(|tx, events| {
            let (state, error) = match &outcome {
                Ok(true) => (OutboxState::Sent, None),
                Ok(false) => (OutboxState::Unconfirmed, None),
                Err(e) => (OutboxState::Failed, Some(e.as_str())),
            };
            let m = get(tx, id)?;
            if outcome.is_ok() && m.entry_id.is_none() {
                let entry = TranscriptEntry {
                    role: TranscriptRole::User,
                    text: m.text.clone(),
                    tool_name: None,
                    tool_call_id: None,
                    is_error: false,
                    truncated: false,
                    ts: Some(now_rfc3339()),
                    tool: None,
                };
                record(tx, events, &m, &entry)?;
            }
            set_state(tx, events, id, state, error)
        })
    }
}

/// Matches a user entry read from the coordinator's session log with the
/// message the daemon typed in, oldest first. Called inside
/// `apply_transcript`'s transaction.
pub(crate) fn claim_logged_message(
    tx: &Transaction,
    project_id: &str,
    entry: &TranscriptEntry,
) -> Result<Logged> {
    if entry.role != TranscriptRole::User {
        return Ok(Logged::Unknown);
    }
    let mut stmt = tx.prepare(&format!(
        "{SELECT} WHERE project_id = ?1 AND seen = 0
           AND state IN ('sending', 'sent', 'unconfirmed')
         ORDER BY created_at, rowid LIMIT ?2"
    ))?;
    let rows = stmt
        .query_map(params![project_id, MATCH_WINDOW], from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let logged = entry.text.trim_start();
    let Some(m) = rows.into_iter().find(|m| {
        let want = first_line(&m.text);
        !want.is_empty() && logged.starts_with(want)
    }) else {
        return Ok(Logged::Unknown);
    };
    tx.execute(
        "UPDATE coordinator_outbox SET seen = 1 WHERE id = ?1",
        [&m.id],
    )?;
    Ok(if m.entry_id.is_some() {
        Logged::Duplicate
    } else {
        Logged::Record(m)
    })
}

/// Records a harness entry as the daemon's message `m`.
pub(crate) fn record_logged(
    tx: &Transaction,
    events: &mut Vec<Event>,
    m: &OutboxMessage,
    entry: &TranscriptEntry,
) -> Result<()> {
    record(tx, events, m, entry).map(|_| ())
}

#[cfg(test)]
mod tests {
    use quark_systems::{CreateProject, TranscriptItem};

    use super::*;
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

    fn entry(role: TranscriptRole, text: &str) -> TranscriptEntry {
        TranscriptEntry {
            role,
            text: text.into(),
            tool_name: None,
            tool_call_id: None,
            is_error: false,
            truncated: false,
            ts: None,
            tool: None,
        }
    }

    fn chat(store: &Store, p: &str) -> Vec<TranscriptItem> {
        store
            .transcript(
                &TranscriptSource::Coordinator {
                    project_id: p.into(),
                },
                Some(0),
                100,
            )
            .unwrap()
    }

    fn log(store: &Store, p: &str, entries: &[TranscriptEntry]) {
        let source = TranscriptSource::Coordinator {
            project_id: p.into(),
        };
        store
            .apply_transcript(p, &source, "/log", 1, entries)
            .unwrap();
    }

    #[test]
    fn a_sent_message_is_recorded_where_it_was_sent_and_its_log_copy_skipped() {
        let store = Store::open_in_memory().unwrap();
        let p = project(&store);
        log(
            &store,
            &p,
            &[entry(TranscriptRole::Assistant, "Working on it")],
        );
        let m = store
            .new_outbox_message(&p, "Also add a test\nplease", OutboxState::Sending)
            .unwrap();
        let done = store.finish_outbox_delivery(&m.id, Ok(true)).unwrap();
        assert_eq!(done.state, OutboxState::Sent);
        // The reply to the step before lands after it, and the harness's
        // copy of the message is not shown twice.
        log(
            &store,
            &p,
            &[
                entry(TranscriptRole::Assistant, "Done with the step"),
                entry(TranscriptRole::User, "Also add a test\nplease"),
                entry(TranscriptRole::Assistant, "Adding the test"),
            ],
        );
        let items = chat(&store, &p);
        let texts: Vec<_> = items.iter().map(|i| i.entry.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "Working on it",
                "Also add a test\nplease",
                "Done with the step",
                "Adding the test"
            ]
        );
        assert_eq!(items[1].message_id.as_deref(), Some(m.id.as_str()));
        assert_eq!(
            store.outbox_message(&m.id).unwrap().entry_id,
            Some(items[1].id)
        );
        // The same text sent later by someone else is recorded as it is.
        log(
            &store,
            &p,
            &[entry(TranscriptRole::User, "Also add a test")],
        );
        let items = chat(&store, &p);
        assert_eq!(items.len(), 5);
        assert_eq!(items[4].message_id, None);
    }

    #[test]
    fn a_log_copy_read_before_delivery_finishes_is_the_record() {
        let store = Store::open_in_memory().unwrap();
        let p = project(&store);
        let m = store
            .new_outbox_message(&p, "Ship it", OutboxState::Sending)
            .unwrap();
        log(&store, &p, &[entry(TranscriptRole::User, "Ship it")]);
        store.finish_outbox_delivery(&m.id, Ok(false)).unwrap();
        let items = chat(&store, &p);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].message_id.as_deref(), Some(m.id.as_str()));
        assert_eq!(
            store.outbox_message(&m.id).unwrap().state,
            OutboxState::Unconfirmed
        );
    }

    #[test]
    fn queued_messages_can_be_edited_cancelled_and_claimed_once() {
        let store = Store::open_in_memory().unwrap();
        let p = project(&store);
        let a = store
            .new_outbox_message(&p, "one", OutboxState::Queued)
            .unwrap();
        let b = store
            .new_outbox_message(&p, "two", OutboxState::Queued)
            .unwrap();
        assert_eq!(
            store.projects_with_queued_messages().unwrap(),
            std::slice::from_ref(&p)
        );

        assert_eq!(store.edit_outbox_message(&a.id, "uno").unwrap().text, "uno");
        store.cancel_outbox_message(&b.id).unwrap();
        assert!(matches!(
            store.edit_outbox_message(&b.id, "dos"),
            Err(StoreError::Conflict(_))
        ));
        assert!(matches!(
            store.claim_outbox_message(&b.id),
            Err(StoreError::Conflict(_))
        ));

        let claimed = store.claim_outbox_message(&a.id).unwrap();
        assert_eq!(claimed.state, OutboxState::Sending);
        assert!(store.claim_outbox_message(&a.id).is_err());
        assert!(store.edit_outbox_message(&a.id, "x").is_err());

        // A failed delivery stays in the outbox and can be sent again.
        let failed = store
            .finish_outbox_delivery(&a.id, Err("no session".into()))
            .unwrap();
        assert_eq!(failed.state, OutboxState::Failed);
        assert_eq!(failed.error.as_deref(), Some("no session"));
        assert!(chat(&store, &p).is_empty());
        let out: Vec<_> = store
            .coordinator_outbox(&p)
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(out, std::slice::from_ref(&a.id));
        store.claim_outbox_message(&a.id).unwrap();
        store.finish_outbox_delivery(&a.id, Ok(true)).unwrap();
        assert!(store.coordinator_outbox(&p).unwrap().is_empty());
        assert_eq!(chat(&store, &p)[0].entry.text, "uno");
        assert!(store.projects_with_queued_messages().unwrap().is_empty());
    }

    #[test]
    fn the_queue_goes_one_message_at_a_time_in_order() {
        let store = Store::open_in_memory().unwrap();
        let p = project(&store);
        let a = store
            .new_outbox_message(&p, "one", OutboxState::Queued)
            .unwrap();
        let b = store
            .new_outbox_message(&p, "two", OutboxState::Queued)
            .unwrap();
        assert_eq!(store.claim_next_queued(&p).unwrap().unwrap().id, a.id);
        // Not while the first is being typed in.
        assert!(store.claim_next_queued(&p).unwrap().is_none());
        store.finish_outbox_delivery(&a.id, Ok(true)).unwrap();
        assert_eq!(store.claim_next_queued(&p).unwrap().unwrap().id, b.id);
        store.finish_outbox_delivery(&b.id, Ok(true)).unwrap();
        assert!(store.claim_next_queued(&p).unwrap().is_none());
        let texts: Vec<_> = chat(&store, &p).into_iter().map(|i| i.entry.text).collect();
        assert_eq!(texts, ["one", "two"]);
    }
}
