//! Keeps each Project's decisions and their `decision` beads in step.
//!
//! Quark's store stays the record the API serves. Every decision gets a
//! bead (with a human gate while it is open); an answer given in Quark
//! closes the bead, and closing the bead or resolving its gate in Beads
//! answers the decision in Quark. Standing rules in force are kept as
//! `quark-rule-*` memories, so agents see them in `bd prime`.
//!
//! [`reconcile`] is idempotent: it reads both sides and writes only what
//! differs, so it runs after any change on either side and on a timer.

use std::collections::HashMap;
use std::time::Duration;

use quark_systems::{AnswerDecision, Decision, DecisionState, EventType};
use tokio::sync::broadcast::error::RecvError;

use super::{db, routes, AppState};
use crate::beads::{BeadsError, DecisionBead};

/// How long a burst of changes settles before a reconcile.
const DEBOUNCE: Duration = Duration::from_millis(500);

/// How often every Project is reconciled even when nothing announced a
/// change, to pick up edits the journal missed.
const SWEEP: Duration = Duration::from_secs(5 * 60);

/// Who an answer given in Beads is recorded as; `bd` does not say who
/// closed a bead.
const BEADS_ANSWERER: &str = "Beads";

/// Starts the mirror: reconciles every Project now, after each decision,
/// rule or Beads change, and every few minutes.
pub fn spawn(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut rx = state.store.subscribe();
        let mut due: HashMap<String, tokio::time::Instant> = HashMap::new();
        let mut sweep = tokio::time::interval(SWEEP);
        loop {
            let next = due.values().min().copied();
            tokio::select! {
                _ = sweep.tick() => {
                    for p in state.store.list_projects().unwrap_or_default() {
                        due.entry(p.id).or_insert_with(tokio::time::Instant::now);
                    }
                }
                ev = rx.recv() => match ev {
                    Ok(ev) => {
                        if wakes(ev.event_type) {
                            if let Some(pid) = ev.project_id {
                                due.insert(pid, tokio::time::Instant::now() + DEBOUNCE);
                            }
                        }
                    }
                    Err(RecvError::Lagged(_)) => {
                        for p in state.store.list_projects().unwrap_or_default() {
                            due.insert(p.id, tokio::time::Instant::now() + DEBOUNCE);
                        }
                    }
                    Err(RecvError::Closed) => return,
                },
                _ = sleep_until(next) => {
                    let now = tokio::time::Instant::now();
                    let ready: Vec<String> = due
                        .iter()
                        .filter(|(_, at)| **at <= now)
                        .map(|(p, _)| p.clone())
                        .collect();
                    for pid in ready {
                        due.remove(&pid);
                        if let Err(e) = reconcile(&state, &pid).await {
                            tracing::info!(project = %pid, error = %e, "mirroring decisions to Beads failed");
                        }
                    }
                }
            }
        }
    })
}

async fn sleep_until(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

fn wakes(t: EventType) -> bool {
    matches!(
        t,
        EventType::DecisionOpened
            | EventType::DecisionAnswered
            | EventType::DecisionActed
            | EventType::RuleUpdated
            | EventType::BeadsChanged
            | EventType::BeadsStatus
    )
}

/// Brings one Project's decision beads and rule memories in line with its
/// decisions. Does nothing when the Project has no Beads database.
pub async fn reconcile(state: &AppState, project_id: &str) -> Result<(), BeadsError> {
    let home = state.layout.home.clone();
    let beads = state.beads.clone();
    let mine = match beads.decision_beads(&home, project_id).await {
        Err(BeadsError::Missing) => return Ok(()),
        r => r?,
    };
    let decisions: Vec<Decision> = {
        let pid = project_id.to_string();
        db(state, move |s| s.list_decisions(None))
            .await
            .map_err(|e| BeadsError::Invalid(e.message().to_string()))?
            .into_iter()
            .filter(|d| d.project_id == pid)
            .collect()
    };
    let by_decision: HashMap<&str, &DecisionBead> =
        mine.iter().map(|b| (b.decision_id.as_str(), b)).collect();

    for d in &decisions {
        let Some(bead) = by_decision.get(d.id.as_str()) else {
            let id = beads.create_decision_bead(&home, d).await?;
            set_bead(state, &d.id, &id).await;
            continue;
        };
        if d.bead_id.as_deref() != Some(bead.id.as_str()) {
            set_bead(state, &d.id, &bead.id).await;
        }
        if d.state == DecisionState::Open {
            if let Some(answer) = bead.beads_answer() {
                answer_from_beads(state, d, answer).await;
            }
            continue;
        }
        let gate = bead.gate.as_deref().map(|g| (g, bead.gate_closed));
        if !bead.closed {
            beads.close_decision_bead(&home, d, &bead.id, gate).await?;
        }
        if let (Some(outcome), false) = (d.outcome.as_deref(), bead.outcome_noted) {
            beads
                .note_decision_outcome(&home, project_id, &bead.id, outcome)
                .await?;
        }
        if d.made_rule_id.is_some() && !bead.labels.iter().any(|l| l == "standing-rule") {
            beads
                .label_rule_decision(&home, project_id, &bead.id)
                .await?;
        }
    }

    let rules: Vec<(String, String)> = {
        let pid = project_id.to_string();
        db(state, move |s| s.list_rules(Some(&pid), false))
            .await
            .map_err(|e| BeadsError::Invalid(e.message().to_string()))?
            .into_iter()
            .map(|r| (r.id, r.text))
            .collect()
    };
    if beads.sync_rule_memories(&home, project_id, &rules).await? {
        crate::beads::memories_changed(&state.store, project_id);
    }
    Ok(())
}

async fn set_bead(state: &AppState, decision: &str, bead: &str) {
    let (d, b) = (decision.to_string(), bead.to_string());
    if let Err(e) = db(state, move |s| s.set_decision_bead(&d, &b)).await {
        tracing::warn!(
            decision,
            bead,
            error = e.message(),
            "recording a decision's bead failed"
        );
    }
}

/// Answers `d` in Quark with what was given in Beads, as any other answer:
/// through the engine, so the asker is unblocked.
async fn answer_from_beads(state: &AppState, d: &Decision, answer: String) {
    let input = AnswerDecision {
        answer,
        answered_by: Some(BEADS_ANSWERER.into()),
        ..Default::default()
    };
    match routes::answer(state, d.id.clone(), input, "beads".into()).await {
        Ok(_) => tracing::info!(decision = %d.id, "decision answered in Beads"),
        // Answered in Quark meanwhile; the next pass closes the bead.
        Err(e) if e.code() == "already_answered" => {}
        Err(e) => {
            tracing::warn!(decision = %d.id, error = e.message(), "answering a decision from Beads failed")
        }
    }
}
