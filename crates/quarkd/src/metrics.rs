//! The Project dashboard's Metrics tab, computed from the event log.
//!
//! Each task's story is folded from its `firstmate.spawn` and
//! `firstmate.status` events (see `quark_eventlog::firstmate`): when its
//! first worker started, every status verb, and how often it was relaunched.
//! A task is finished when its latest `working`, `needs-decision`,
//! `blocked`, `done` or `failed` line is `done` or `failed`; a later
//! `resolved` or `paused` line does not reopen it.

use std::collections::BTreeMap;

use quark_core::{Event, TaskId};
use quark_engine::meta::SpawnMeta;
use quark_eventlog::firstmate::{kinds, StatusPayload};
use quark_systems::{
    AccountFailover, DayCount, Failovers, GateMetrics, Interventions, LeadTime, ProjectMetrics,
    Throughput, UnavailableMetric,
};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

/// Days the tab shows unless asked otherwise, and the most it will cover.
pub const DEFAULT_DAYS: u32 = 7;
pub const MAX_DAYS: u32 = 90;

#[derive(Default)]
struct TaskStory {
    first_spawn: Option<OffsetDateTime>,
    first_event: Option<OffsetDateTime>,
    spawns: u32,
    saw_failed: bool,
    saw_blocked: bool,
    /// The latest line that says where the task stands.
    last: Option<(String, OffsetDateTime)>,
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_default()
}

/// Nearest-rank percentile of sorted `v`.
fn percentile(v: &[u64], p: f64) -> Option<u64> {
    if v.is_empty() {
        return None;
    }
    let rank = ((p * v.len() as f64).ceil() as usize).clamp(1, v.len());
    Some(v[rank - 1])
}

fn ratio(n: u32, d: u32) -> Option<f64> {
    (d > 0).then(|| f64::from(n) / f64::from(d))
}

/// Metrics for `project_id` over the `days` ending at `now`.
///
/// `events` are the Project's events in log order; `failovers` every
/// failover its tasks recorded. Accounts are left empty for the caller.
pub fn compute(
    project_id: &str,
    events: &[Event],
    failovers: &[AccountFailover],
    now: OffsetDateTime,
    days: u32,
) -> ProjectMetrics {
    let days = days.clamp(1, MAX_DAYS);
    let today = now.date();
    let first_day = today - Duration::days(i64::from(days) - 1);
    let from = first_day.midnight().assume_utc();
    let in_window = |t: OffsetDateTime| t >= from && t <= now;

    let mut tasks: BTreeMap<&TaskId, TaskStory> = BTreeMap::new();
    let mut interventions = Interventions::default();
    for e in events {
        let Some(task) = &e.task else { continue };
        let story = tasks.entry(task).or_default();
        story.first_event.get_or_insert(e.ts);
        match e.kind.as_str() {
            kinds::SPAWN => {
                let at = e
                    .decode::<SpawnMeta>()
                    .ok()
                    .and_then(|m| m.spawned_at())
                    .and_then(|s| OffsetDateTime::from_unix_timestamp(s).ok())
                    .unwrap_or(e.ts);
                story.first_spawn = Some(story.first_spawn.map_or(at, |f| f.min(at)));
                story.spawns += 1;
                if story.spawns > 1 && in_window(e.ts) {
                    interventions.relaunches += 1;
                }
            }
            kinds::STATUS => {
                let Ok(line) = e.decode::<StatusPayload>() else {
                    continue;
                };
                match line.verb.as_str() {
                    "failed" => story.saw_failed = true,
                    "blocked" => story.saw_blocked = true,
                    _ => {}
                }
                if in_window(e.ts) {
                    match line.verb.as_str() {
                        "needs-decision" => interventions.decisions += 1,
                        "blocked" => interventions.blockers += 1,
                        _ => {}
                    }
                }
                if matches!(
                    line.verb.as_str(),
                    "working" | "needs-decision" | "blocked" | "done" | "failed"
                ) {
                    story.last = Some((line.verb, e.ts));
                }
            }
            _ => {}
        }
    }

    let mut per_day: Vec<DayCount> = (0..days)
        .map(|i| DayCount {
            date: (first_day + Duration::days(i64::from(i))).to_string(),
            done: 0,
            failed: 0,
        })
        .collect();
    let mut throughput = Throughput::default();
    let mut lead = Vec::new();
    let mut first_time_green = 0;
    for story in tasks.values() {
        let Some((verb, at)) = &story.last else {
            continue;
        };
        if !in_window(*at) {
            continue;
        }
        let day = (at.date() - first_day).whole_days() as usize;
        match verb.as_str() {
            "done" => {
                throughput.done += 1;
                if let Some(d) = per_day.get_mut(day) {
                    d.done += 1;
                }
                let start = story.first_spawn.or(story.first_event).unwrap_or(*at);
                lead.push((*at - start).whole_seconds().max(0) as u64);
                if !story.saw_failed && !story.saw_blocked && story.spawns <= 1 {
                    first_time_green += 1;
                }
            }
            "failed" => {
                throughput.failed += 1;
                if let Some(d) = per_day.get_mut(day) {
                    d.failed += 1;
                }
            }
            _ => {}
        }
    }
    throughput.per_day = per_day;
    lead.sort_unstable();
    let finished = throughput.done + throughput.failed;
    interventions.per_finished_task =
        ratio(interventions.decisions + interventions.blockers, finished).filter(|_| finished > 0);

    let mut fo = Failovers::default();
    for f in failovers {
        let at = OffsetDateTime::parse(&f.at, &Rfc3339).ok();
        if at.is_some_and(in_window) {
            fo.add(f.outcome);
        }
    }

    ProjectMetrics {
        project_id: project_id.to_string(),
        days,
        from: rfc3339(from),
        to: rfc3339(now),
        log_started_at: events.first().map(|e| rfc3339(e.ts)),
        gates: GateMetrics {
            pass_rate: ratio(throughput.done, finished),
            first_time_green,
            first_time_green_rate: ratio(first_time_green, throughput.done),
        },
        lead_time: LeadTime {
            tasks: lead.len() as u32,
            median_s: percentile(&lead, 0.5),
            p90_s: percentile(&lead, 0.9),
        },
        throughput,
        interventions,
        failovers: fo,
        accounts: Vec::new(),
        unavailable: vec![
            UnavailableMetric {
                metric: "spend".into(),
                reason: "Workers' token use and cost are not recorded yet; quota per account is shown instead.".into(),
            },
            UnavailableMetric {
                metric: "coordinator_tokens".into(),
                reason: "The coordinator's token use is not recorded in the event log yet.".into(),
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use quark_core::{HostId, NewEvent, ProjectId, Seq};
    use quark_systems::FailoverOutcome;
    use time::macros::datetime;

    use super::*;

    const NOW: OffsetDateTime = datetime!(2026-10-07 12:00 UTC);

    struct Log(Vec<Event>);

    impl Log {
        fn push(&mut self, at: OffsetDateTime, task: &str, kind: &str, payload: serde_json::Value) {
            let mut e = NewEvent::new(
                HostId::from("h"),
                ProjectId::from("p"),
                Some(TaskId::from(task)),
                kind,
                payload,
            );
            e.ts = at;
            let seq = Seq(self.0.len() as u64 + 1);
            self.0.push(e.with_seq(seq));
        }

        fn spawn(&mut self, at: OffsetDateTime, task: &str) {
            let generation = format!("s{}.1.1", at.unix_timestamp());
            self.push(
                at,
                task,
                kinds::SPAWN,
                serde_json::json!({"generation": generation, "harness": "claude", "model": null, "effort": null, "project": null, "kind": "ship"}),
            );
        }

        fn status(&mut self, at: OffsetDateTime, task: &str, verb: &str) {
            self.push(
                at,
                task,
                kinds::STATUS,
                serde_json::json!({"verb": verb, "key": null, "corr": null, "note": "", "raw": verb, "offset": 0}),
            );
        }
    }

    #[test]
    fn folds_outcomes_lead_time_and_interventions() {
        let mut log = Log(Vec::new());
        // a: clean, 1h.
        log.spawn(datetime!(2026-10-06 09:00 UTC), "a");
        log.status(datetime!(2026-10-06 09:01 UTC), "a", "working");
        log.status(datetime!(2026-10-06 10:00 UTC), "a", "done");
        // b: asked a decision, was relaunched, done after 3h.
        log.spawn(datetime!(2026-10-07 06:00 UTC), "b");
        log.status(datetime!(2026-10-07 07:00 UTC), "b", "needs-decision");
        log.status(datetime!(2026-10-07 07:30 UTC), "b", "resolved");
        log.spawn(datetime!(2026-10-07 08:00 UTC), "b");
        log.status(datetime!(2026-10-07 09:00 UTC), "b", "done");
        log.status(datetime!(2026-10-07 09:05 UTC), "b", "resolved");
        // c: blocked, then failed.
        log.spawn(datetime!(2026-10-05 09:00 UTC), "c");
        log.status(datetime!(2026-10-05 10:00 UTC), "c", "blocked");
        log.status(datetime!(2026-10-05 11:00 UTC), "c", "failed");
        // d: done, then picked up again; not finished.
        log.spawn(datetime!(2026-10-07 01:00 UTC), "d");
        log.status(datetime!(2026-10-07 02:00 UTC), "d", "done");
        log.status(datetime!(2026-10-07 03:00 UTC), "d", "working");
        // e: done before the window.
        log.spawn(datetime!(2026-09-01 01:00 UTC), "e");
        log.status(datetime!(2026-09-01 02:00 UTC), "e", "done");

        let failovers = [
            AccountFailover {
                from_account_id: "x".into(),
                to_account_id: Some("y".into()),
                pool: None,
                outcome: FailoverOutcome::Relaunched,
                signal: "s".into(),
                detail: None,
                at: "2026-10-07T07:59:00Z".into(),
            },
            AccountFailover {
                from_account_id: "y".into(),
                to_account_id: None,
                pool: None,
                outcome: FailoverOutcome::NoHealthyAccount,
                signal: "s".into(),
                detail: None,
                at: "2026-08-01T00:00:00Z".into(),
            },
        ];

        let m = compute("p", &log.0, &failovers, NOW, 7);
        assert_eq!(m.from, "2026-10-01T00:00:00Z");
        assert_eq!(m.log_started_at.as_deref(), Some("2026-10-06T09:00:00Z"));
        assert_eq!((m.throughput.done, m.throughput.failed), (2, 1));
        assert_eq!(m.throughput.per_day.len(), 7);
        let last = m.throughput.per_day.last().unwrap();
        assert_eq!((last.date.as_str(), last.done), ("2026-10-07", 1));
        assert_eq!(m.throughput.per_day[4].failed, 1);
        assert_eq!(m.lead_time.tasks, 2);
        assert_eq!(m.lead_time.median_s, Some(3600));
        assert_eq!(m.lead_time.p90_s, Some(3 * 3600));
        assert_eq!(m.gates.pass_rate, Some(2.0 / 3.0));
        assert_eq!(m.gates.first_time_green, 1);
        assert_eq!(m.gates.first_time_green_rate, Some(0.5));
        assert_eq!(m.interventions.decisions, 1);
        assert_eq!(m.interventions.blockers, 1);
        assert_eq!(m.interventions.relaunches, 1);
        assert_eq!(m.interventions.per_finished_task, Some(2.0 / 3.0));
        assert_eq!(m.failovers.total(), 1);
        assert_eq!(m.failovers.relaunched, 1);
    }

    #[test]
    fn empty_log_has_no_rates() {
        let m = compute("p", &[], &[], NOW, 500);
        assert_eq!(m.days, MAX_DAYS);
        assert_eq!(m.log_started_at, None);
        assert_eq!(m.gates.pass_rate, None);
        assert_eq!(m.lead_time.median_s, None);
        assert_eq!(m.interventions.per_finished_task, None);
        assert_eq!(m.unavailable.len(), 2);
    }
}
