//! Keeps the store and the event log from growing without bound.
//!
//! The projector records every engine read in `adapter_calls`, a few rows
//! per Project every refresh, and a finished task's terminal keeps the
//! output it had when it stopped. Left alone both grow for as long as
//! quarkd runs: on a month-old home they were most of a 350 MB quark.db.
//! Housekeeping drops successful adapter calls after a day, failures after
//! a month, and trims a finished task's terminal to its last screens.
//!
//! What it keeps is the projections: the tables of tasks, decisions, pull
//! requests and the rest, transcripts, and every event-log kind a read
//! model folds into state. What goes, by age:
//!
//! - quark.db: change notices (every event but terminal output and
//!   transcripts) after [`NOTICES_KEPT`]; all terminal output of a task
//!   finished [`FINISHED_OUTPUT_KEPT`] ago, and of a worker terminal whose
//!   task is gone.
//! - events.db: host telemetry and pool reports after [`TELEMETRY_KEPT`]
//!   (each host's newest stays, as its current reading), and the legacy
//!   `firstmate.*` and `shadow.*` events nothing reads since the
//!   firstmate engine was retired.
//!
//! When that leaves a quarter of a file free, it compacts the file.

use std::sync::Arc;
use std::time::Duration;

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use quark_eventlog::{Prune, SqliteEventLog};

use crate::store::Store;

/// How often housekeeping runs. The first run waits [`FIRST_RUN`] so
/// startup is not slowed.
pub const INTERVAL: Duration = Duration::from_secs(60 * 60);
const FIRST_RUN: Duration = Duration::from_secs(60);
/// Successful adapter calls only feed "recent" reads.
const OK_CALLS_KEPT: time::Duration = time::Duration::days(1);
/// Failures stay for diagnosis.
const FAILED_CALLS_KEPT: time::Duration = time::Duration::days(30);
/// A finished task's terminal is trimmed once it has been still this long.
const FINISHED_AFTER: time::Duration = time::Duration::days(1);
/// Output a finished task's terminal keeps: its last few screens.
pub const FINISHED_RETAIN_BYTES: u64 = 256 * 1024;
/// A finished task's terminal output is dropped this long after it ended;
/// its transcript stays. On a five-week-old home finished tasks' output was
/// a third of quark.db.
const FINISHED_OUTPUT_KEPT: time::Duration = time::Duration::days(7);
/// A worker terminal whose task is gone is dropped once quiet this long.
const ORPHAN_OUTPUT_AFTER: time::Duration = time::Duration::days(1);
/// Change notices in quark.db: a client offline longer refetches anyway.
const NOTICES_KEPT: time::Duration = time::Duration::days(30);
/// Host telemetry and pool reports: the Hosts view shows the last 48 hours
/// ([`crate::hosts::RETAIN`]).
const TELEMETRY_KEPT: time::Duration = time::Duration::days(3);
/// Event-log kinds kept for [`TELEMETRY_KEPT`].
const TELEMETRY_KINDS: &[&str] = &[quark_hosts::recorder::SAMPLE, crate::hosts::WORKTREES];
/// Event-log kinds of the retired firstmate engine and its shadows.
const LEGACY_PREFIXES: &[&str] = &["firstmate.", "shadow."];
/// Rows deleted per store lock, so a backlog never stalls other writers.
const BATCH: u32 = 5_000;
/// The store is compacted once at least this much of it is free.
const VACUUM_MIN_FREE: u64 = 32 * 1024 * 1024;

pub struct Housekeeping {
    store: Arc<Store>,
    events: Option<SqliteEventLog>,
}

/// What one pass removed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pass {
    pub adapter_calls: usize,
    pub terminal_events: usize,
    pub notices: usize,
    pub vacuumed_bytes: u64,
    /// Event-log events deleted.
    pub log_events: usize,
    pub log_vacuumed_bytes: u64,
}

impl Housekeeping {
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            events: None,
        }
    }

    /// Also apply retention to the native engine's event log.
    pub fn with_events(mut self, events: SqliteEventLog) -> Self {
        self.events = Some(events);
        self
    }

    pub async fn run(self, interval: Duration) {
        tokio::time::sleep(FIRST_RUN).await;
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            let store = self.store.clone();
            let now = OffsetDateTime::now_utc();
            let mut p = match tokio::task::spawn_blocking(move || pass(&store, now)).await {
                Ok(Ok(p)) => p,
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "housekeeping failed");
                    Pass::default()
                }
                Err(e) => {
                    tracing::error!(error = %e, "housekeeping panicked");
                    Pass::default()
                }
            };
            if let Some(log) = &self.events {
                match log_pass(log, now).await {
                    Ok((n, freed)) => {
                        p.log_events = n;
                        p.log_vacuumed_bytes = freed;
                    }
                    Err(e) => tracing::warn!(error = %e, "event log housekeeping failed"),
                }
            }
            if p != Pass::default() {
                tracing::info!(
                    adapter_calls = p.adapter_calls,
                    terminal_events = p.terminal_events,
                    notices = p.notices,
                    vacuumed_bytes = p.vacuumed_bytes,
                    log_events = p.log_events,
                    log_vacuumed_bytes = p.log_vacuumed_bytes,
                    "housekeeping pruned the store"
                );
            }
        }
    }
}

/// One housekeeping pass as of `now`.
pub fn pass(store: &Store, now: OffsetDateTime) -> crate::store::Result<Pass> {
    let at = |d: time::Duration| (now - d).format(&Rfc3339).expect("UTC formats");
    let (ok_before, failed_before) = (at(OK_CALLS_KEPT), at(FAILED_CALLS_KEPT));
    let mut out = Pass::default();
    loop {
        let n = store.prune_adapter_calls(&ok_before, &failed_before, BATCH)?;
        out.adapter_calls += n;
        if n < BATCH as usize {
            break;
        }
    }
    for id in store.finished_terminals_before(&at(FINISHED_OUTPUT_KEPT))? {
        out.terminal_events += store.drop_terminal_output(&id)?;
    }
    for id in store.orphan_worker_terminals(&at(ORPHAN_OUTPUT_AFTER))? {
        out.terminal_events += store.drop_terminal_output(&id)?;
    }
    for id in store.finished_terminals_over(&at(FINISHED_AFTER), FINISHED_RETAIN_BYTES)? {
        out.terminal_events += store.trim_finished_terminal(&id, FINISHED_RETAIN_BYTES)?;
    }
    let notices_before = at(NOTICES_KEPT);
    loop {
        let n = store.prune_notices(&notices_before, BATCH)?;
        out.notices += n;
        if n < BATCH as usize {
            break;
        }
    }
    out.vacuumed_bytes = store.vacuum_if_mostly_free(VACUUM_MIN_FREE)?;
    Ok(out)
}

/// One event-log pass as of `now`: returns the events deleted and the bytes
/// a compaction freed.
pub async fn log_pass(
    log: &SqliteEventLog,
    now: OffsetDateTime,
) -> quark_core::Result<(usize, u64)> {
    let mut rules: Vec<Prune> = LEGACY_PREFIXES
        .iter()
        .map(|p| Prune::KindPrefix(p.to_string()))
        .collect();
    rules.extend(TELEMETRY_KINDS.iter().map(|k| Prune::OlderThan {
        kind: k.to_string(),
        before: now - TELEMETRY_KEPT,
    }));
    let mut deleted = 0;
    for rule in rules {
        loop {
            let n = log.prune(rule.clone(), BATCH as usize).await?;
            deleted += n;
            if n < BATCH as usize {
                break;
            }
        }
    }
    let freed = log.vacuum_if_mostly_free(VACUUM_MIN_FREE).await?;
    Ok((deleted, freed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use quark_core::{EventLog, HostId, NewEvent, ProjectId, Seq};

    #[tokio::test]
    async fn the_log_pass_drops_old_telemetry_and_legacy_events() {
        let dir = tempfile::tempdir().unwrap();
        let log = SqliteEventLog::open(dir.path().join("events.db")).unwrap();
        let now = OffsetDateTime::now_utc();
        let ev = |kind: &str, age: time::Duration| {
            let mut e = NewEvent::new(
                HostId::from("mac"),
                ProjectId::from("p"),
                None,
                kind,
                serde_json::json!({}),
            );
            e.ts = now - age;
            e
        };
        let day = time::Duration::days(1);
        for kind in [
            quark_hosts::recorder::SAMPLE,
            quark_hosts::recorder::SAMPLE,
            quark_hosts::recorder::SAMPLE,
            "firstmate.ingest",
            "shadow.divergence",
            "task.transition",
            crate::hosts::WORKTREES,
        ] {
            log.append(ev(kind, day * 10)).await.unwrap();
        }
        log.append(ev(quark_hosts::recorder::SAMPLE, day))
            .await
            .unwrap();
        log.append(ev("task.transition", day)).await.unwrap();

        let (deleted, _) = log_pass(&log, now).await.unwrap();
        // Three old samples (the newest is recent) and both legacy events;
        // the only pool report is its host's newest.
        assert_eq!(deleted, 5);
        let kinds: Vec<String> = log
            .read(Seq::ZERO, 100)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.kind.as_str().to_string())
            .collect();
        assert_eq!(
            kinds,
            [
                "task.transition",
                crate::hosts::WORKTREES,
                quark_hosts::recorder::SAMPLE,
                "task.transition"
            ]
        );
        assert_eq!(log_pass(&log, now).await.unwrap().0, 0);
    }
}
