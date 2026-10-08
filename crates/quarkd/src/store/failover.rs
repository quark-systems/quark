//! Rate-limit failovers (ADR-11): what each task's worker hit and where it
//! went, and the decisions the daemon opens itself when a worker has nowhere
//! to go.
//!
//! A failover is kept on its task (`Task.failovers`) and in the task's
//! activity log, so the dispatch record can cite it.

use quark_systems::{
    AccountFailover, Decision, DecisionBrief, DecisionState, EventType, FailoverOutcome, Task,
    TaskEvent,
};
use rusqlite::{params, OptionalExtension, Transaction};

use super::{
    append_event, decision_from_row, insert_decision, later, new_id, task_from_row, Result, Store,
    StoreError, DECISION_SELECT, TASK_SELECT,
};
use crate::now_rfc3339;

/// Engine ids of decisions the daemon opened itself. An engine's own ids are
/// path-safe, so they never contain a slash.
const DAEMON_DECISION_PREFIX: &str = "quarkd/";

/// Kind of the activity-log entry a failover leaves.
pub const FAILOVER_EVENT_KIND: &str = "failover";

/// Whether a decision with this engine id was opened by the daemon, not
/// projected from an engine hold.
pub fn is_daemon_decision(engine_id: &str) -> bool {
    engine_id.starts_with(DAEMON_DECISION_PREFIX)
}

/// The engine id of the decision about a task's rate limit.
fn failover_decision_id(task_id: &str) -> String {
    format!("{DAEMON_DECISION_PREFIX}failover/{task_id}")
}

/// The task's latest failover that relaunched its worker and that no
/// dispatch record notes yet: one made after the task's last record.
pub(super) fn unrecorded(tx: &Transaction, task_id: &str) -> Result<Option<AccountFailover>> {
    let failovers: Option<String> = tx
        .query_row(
            "SELECT failovers FROM tasks WHERE id = ?1",
            [task_id],
            |r| r.get(0),
        )
        .optional()?;
    let failovers: Vec<AccountFailover> = failovers
        .map(|j| serde_json::from_str(&j).unwrap_or_default())
        .unwrap_or_default();
    let Some(last) = failovers
        .into_iter()
        .rev()
        .find(|f| f.outcome == FailoverOutcome::Relaunched)
    else {
        return Ok(None);
    };
    let recorded: Option<String> = tx.query_row(
        "SELECT MAX(recorded_at) FROM dispatch_records WHERE task_id = ?1",
        [task_id],
        |r| r.get(0),
    )?;
    Ok(later(&last.at, recorded.as_deref()).then_some(last))
}

impl Store {
    /// Records a failover on its task and in the task's activity log, moves
    /// the task to the account it was relaunched under, and, given a
    /// `question`, opens a decision about it unless one is already open.
    /// Emits `task.state_changed`, `task.event` and `decision.opened`.
    pub fn record_failover(
        &self,
        task_id: &str,
        failover: &AccountFailover,
        note: &str,
        question: Option<&str>,
    ) -> Result<Task> {
        self.write(|tx, events| {
            let old: Task = tx
                .query_row(
                    &format!("{TASK_SELECT} WHERE id = ?1"),
                    [task_id],
                    task_from_row,
                )
                .optional()?
                .ok_or(StoreError::NotFound)?;
            let mut task = old.clone();
            task.failovers.push(failover.clone());
            if let Some(to) = &failover.to_account_id {
                task.account_id = Some(to.clone());
            }
            task.updated_at = now_rfc3339();
            tx.execute(
                "UPDATE tasks SET account_id = ?2, failovers = ?3, updated_at = ?4 WHERE id = ?1",
                params![
                    task.id,
                    task.account_id,
                    serde_json::to_string(&task.failovers)?,
                    task.updated_at
                ],
            )?;
            append_event(
                tx,
                events,
                Some(&task.project_id),
                EventType::TaskStateChanged,
                serde_json::json!({ "task": task, "previous_state": old.state }),
            )?;

            tx.execute(
                "INSERT INTO task_events (task_id, project_id, kind, decision_key, note, raw, ts)
                 VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6)",
                params![
                    task.id,
                    task.project_id,
                    FAILOVER_EVENT_KIND,
                    note,
                    format!("{FAILOVER_EVENT_KIND}: {note}"),
                    failover.at
                ],
            )?;
            let event = TaskEvent {
                id: tx.last_insert_rowid(),
                task_id: task.id.clone(),
                project_id: task.project_id.clone(),
                kind: FAILOVER_EVENT_KIND.into(),
                decision_key: None,
                note: note.to_string(),
                ts: failover.at.clone(),
            };
            append_event(
                tx,
                events,
                Some(&task.project_id),
                EventType::TaskEvent,
                serde_json::to_value(&event)?,
            )?;

            let Some(question) = question else {
                return Ok(task);
            };
            let engine_id = failover_decision_id(&task.id);
            let open: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM decisions
                 WHERE project_id = ?1 AND engine_id = ?2 AND state = 'open')",
                params![task.project_id, engine_id],
                |r| r.get(0),
            )?;
            if !open {
                let mut decision = Decision {
                    id: new_id("dec"),
                    project_id: task.project_id.clone(),
                    task_id: Some(task.id.clone()),
                    question: question.to_string(),
                    state: DecisionState::Open,
                    brief: DecisionBrief {
                        asked_by: Some("quarkd".into()),
                        blocks: vec![task.id.clone()],
                        ..Default::default()
                    },
                    opened_at: now_rfc3339(),
                    ..Default::default()
                };
                insert_decision(tx, &engine_id, &mut decision)?;
                append_event(
                    tx,
                    events,
                    Some(&task.project_id),
                    EventType::DecisionOpened,
                    serde_json::to_value(&decision)?,
                )?;
            }
            Ok(task)
        })
    }

    /// The open decision about a task's rate limit, if there is one.
    pub fn open_failover_decision(&self, task_id: &str) -> Result<Option<Decision>> {
        self.read(|c| {
            Ok(c.query_row(
                &format!("{DECISION_SELECT} WHERE engine_id = ?1 AND state = 'open'"),
                [failover_decision_id(task_id)],
                decision_from_row,
            )
            .optional()?)
        })
    }

    /// Accounts some task's worker reported a rate limit on after `since`
    /// (RFC 3339).
    pub fn rate_limited_since(&self, since: &str) -> Result<Vec<String>> {
        self.read(|c| {
            let mut stmt = c.prepare("SELECT failovers FROM tasks WHERE failovers != '[]'")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            let mut out: Vec<String> = Vec::new();
            for row in rows {
                let failovers: Vec<AccountFailover> =
                    serde_json::from_str(&row?).unwrap_or_default();
                for f in failovers {
                    if later(&f.at, Some(since)) && !out.contains(&f.from_account_id) {
                        out.push(f.from_account_id);
                    }
                }
            }
            Ok(out)
        })
    }
}
