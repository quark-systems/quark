//! Records samples into the event log as a time series.

use std::future::Future;
use std::time::Duration;

use quark_core::telemetry::HostSample;
use quark_core::{EventLog, NewEvent, ProjectId, Result, Seq, Telemetry};
use tokio::time::MissedTickBehavior;

/// Event kind of one host sample. Payload: [`HostSample`]. Recorded under
/// the engine project; per-Project slices are in `usage`.
pub const SAMPLE: &str = "telemetry.sample";

/// The event for `sample`, stamped with the sample's own time.
pub fn sample_event(sample: &HostSample) -> Result<NewEvent> {
    let mut event = NewEvent::typed(
        sample.host.clone(),
        ProjectId::engine(),
        None,
        SAMPLE,
        sample,
    )?;
    event.ts = sample.ts;
    Ok(event)
}

/// Takes one sample and appends it.
pub async fn record_once(telemetry: &dyn Telemetry, log: &dyn EventLog) -> Result<Seq> {
    let sample = telemetry.sample().await?;
    log.append(sample_event(&sample)?).await
}

/// Samples every `every` until `stop` resolves. A failed sample or append
/// is logged and the next tick tries again; a slow sample skips the ticks it
/// overran rather than bursting to catch up.
pub async fn run(
    telemetry: &dyn Telemetry,
    log: &dyn EventLog,
    every: Duration,
    stop: impl Future<Output = ()>,
) {
    let mut ticks = tokio::time::interval(every);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
    tokio::pin!(stop);
    loop {
        tokio::select! {
            _ = &mut stop => return,
            _ = ticks.tick() => {
                if let Err(e) = record_once(telemetry, log).await {
                    tracing::warn!(error = %e, "telemetry sample failed");
                }
            }
        }
    }
}
