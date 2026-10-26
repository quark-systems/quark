//! Keeps the store from growing without bound.
//!
//! The projector records every engine read in `adapter_calls`, a few rows
//! per Project every refresh, and a finished task's terminal keeps the
//! output it had when it stopped. Left alone both grow for as long as
//! quarkd runs: on a month-old home they were most of a 350 MB quark.db.
//! Housekeeping drops successful adapter calls after a day, failures after
//! a month, and trims a finished task's terminal to its last screens. When that leaves most of
//! the file free, it compacts the file.

use std::sync::Arc;
use std::time::Duration;

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

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
/// Rows deleted per store lock, so a backlog never stalls other writers.
const BATCH: u32 = 5_000;
/// The store is compacted once at least this much of it is free.
const VACUUM_MIN_FREE: u64 = 64 * 1024 * 1024;

pub struct Housekeeping {
    store: Arc<Store>,
}

/// What one pass removed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pass {
    pub adapter_calls: usize,
    pub terminal_events: usize,
    pub vacuumed_bytes: u64,
}

impl Housekeeping {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    pub async fn run(self, interval: Duration) {
        tokio::time::sleep(FIRST_RUN).await;
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            let store = self.store.clone();
            let now = OffsetDateTime::now_utc();
            match tokio::task::spawn_blocking(move || pass(&store, now)).await {
                Ok(Ok(p)) if p != Pass::default() => {
                    tracing::info!(
                        adapter_calls = p.adapter_calls,
                        terminal_events = p.terminal_events,
                        vacuumed_bytes = p.vacuumed_bytes,
                        "housekeeping pruned the store"
                    );
                }
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::warn!(error = %e, "housekeeping failed"),
                Err(e) => tracing::error!(error = %e, "housekeeping panicked"),
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
    for id in store.finished_terminals_over(&at(FINISHED_AFTER), FINISHED_RETAIN_BYTES)? {
        out.terminal_events += store.trim_finished_terminal(&id, FINISHED_RETAIN_BYTES)?;
    }
    out.vacuumed_bytes = store.vacuum_if_mostly_free(VACUUM_MIN_FREE)?;
    Ok(out)
}
