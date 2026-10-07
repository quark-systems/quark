//! The Project dashboard's Metrics tab, computed from the event log.
//!
//! Each task's story is folded from its `firstmate.spawn` and
//! `firstmate.status` events (see `quark_eventlog::firstmate`): when its
//! first worker started, every status verb, and how often it was relaunched.
//! A task is finished when its latest `working`, `needs-decision`,
//! `blocked`, `done` or `failed` line is `done` or `failed`; a later
//! `resolved` or `paused` line does not reopen it.
//!
//! Tokens and spend come from `usage.turn` events ([`crate::usage`]), each
//! placed in time by when its turn started.

use std::collections::BTreeMap;

use quark_coordinator::{Efficiency, Side};
use quark_core::{Event, TaskId};
use quark_engine::meta::SpawnMeta;
use quark_eventlog::firstmate::{kinds, StatusPayload};
use quark_systems::{
    AccountFailover, CoordinatorMetrics, CoordinatorTurns, DayCount, Failovers, GateMetrics,
    Interventions, LeadTime, ModelSpend, ProjectMetrics, SpendMetrics, Throughput, TokenSpend,
    UnavailableMetric,
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

    let mut done_tasks: Vec<&TaskId> = Vec::new();
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
    for (task, story) in &tasks {
        let Some((verb, at)) = &story.last else {
            continue;
        };
        if !in_window(*at) {
            continue;
        }
        let day = (at.date() - first_day).whole_days() as usize;
        match verb.as_str() {
            "done" => {
                done_tasks.push(task);
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

    let mut coordinator = coordinator(events, in_window);
    let spend = spend(events, in_window, &done_tasks);
    // firstmate's coordinator transcript is read for the baseline only when
    // it runs Claude Code; its turns' tokens are known for any harness.
    if coordinator.baseline.turns == 0 && spend.coordinator.turns > 0 {
        let c = &spend.coordinator;
        let per = |n: f64| (coordinator.tasks > 0).then(|| n / f64::from(coordinator.tasks));
        coordinator.baseline = CoordinatorTurns {
            turns: c.turns,
            ack_turns: 0,
            input_tokens: c.input_tokens,
            output_tokens: c.output_tokens,
            cache_read_tokens: c.cache_read_tokens,
            turns_per_task: per(f64::from(c.turns)),
            tokens_per_task: per((c.input_tokens + c.output_tokens) as f64),
            ack_share: None,
        };
    }
    let mut unavailable = Vec::new();
    if spend.workers.turns == 0 && spend.coordinator.turns == 0 {
        unavailable.push(UnavailableMetric {
            metric: "spend".into(),
            reason: "No worker or coordinator turn with token counts is in the event log for this window yet. They are read from the agents' Claude Code, Codex and Pi session logs.".into(),
        });
    }
    if coordinator.baseline.turns == 0 && coordinator.native.turns == 0 {
        unavailable.push(UnavailableMetric {
            metric: "coordinator_tokens".into(),
            reason: "No coordinator turn is in the event log for this window yet. Turns are read from the coordinator's Claude Code transcript.".into(),
        });
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
        coordinator,
        spend,
        unavailable,
    }
}

/// When a `usage.turn` happened: when its turn started, else when it was
/// read.
fn turn_time(e: &Event, turn: &crate::usage::UsageTurn) -> OffsetDateTime {
    turn.turn
        .at
        .as_deref()
        .and_then(|a| OffsetDateTime::parse(a, &Rfc3339).ok())
        .unwrap_or(e.ts)
}

#[derive(Default)]
struct Tally {
    spend: TokenSpend,
}

impl Tally {
    fn add(&mut self, u: &quark_transcript::ModelUsage) {
        self.spend.input_tokens += u.input;
        self.spend.output_tokens += u.output;
        self.spend.cache_read_tokens += u.cache_read;
        match crate::spend::cost(u) {
            Some(c) => *self.spend.usd.get_or_insert(0.0) += c,
            None => self.spend.unpriced_tokens += u.input + u.output,
        }
    }
}

/// Tokens and spend of the turns in the window, and the lifetime worker
/// spend of `done` tasks.
fn spend(
    events: &[Event],
    in_window: impl Fn(OffsetDateTime) -> bool,
    done: &[&TaskId],
) -> SpendMetrics {
    use crate::usage::{Agent, UsageTurn, KIND};
    let mut workers = Tally::default();
    let mut coordinator = Tally::default();
    let mut models: BTreeMap<String, ModelSpend> = BTreeMap::new();
    let mut per_task: BTreeMap<&TaskId, f64> = BTreeMap::new();
    for e in events.iter().filter(|e| e.kind.as_str() == KIND) {
        let Ok(t) = e.decode::<UsageTurn>() else {
            continue;
        };
        if let (Agent::Worker, Some(task)) = (t.agent, &e.task) {
            let cost: f64 = t.turn.models.iter().filter_map(crate::spend::cost).sum();
            *per_task.entry(task).or_default() += cost;
        }
        if !in_window(turn_time(e, &t)) {
            continue;
        }
        let tally = match t.agent {
            Agent::Worker => &mut workers,
            Agent::Coordinator => &mut coordinator,
        };
        if !t.turn.continued {
            tally.spend.turns += 1;
        }
        for u in &t.turn.models {
            tally.add(u);
            let m = models.entry(u.model.clone()).or_insert_with(|| ModelSpend {
                model: u.model.clone(),
                ..Default::default()
            });
            m.input_tokens += u.input;
            m.output_tokens += u.output;
            if let Some(c) = crate::spend::cost(u) {
                *m.usd.get_or_insert(0.0) += c;
            }
        }
    }
    let mut by_model: Vec<ModelSpend> = models.into_values().collect();
    by_model.sort_by(|a, b| {
        b.usd
            .unwrap_or(0.0)
            .total_cmp(&a.usd.unwrap_or(0.0))
            .then(b.output_tokens.cmp(&a.output_tokens))
    });
    let read: Vec<f64> = done
        .iter()
        .filter_map(|t| per_task.get(t))
        .copied()
        .collect();
    let priced: Vec<f64> = read.iter().copied().filter(|c| *c > 0.0).collect();
    SpendMetrics {
        workers: workers.spend,
        coordinator: coordinator.spend,
        by_model,
        done_tasks: read.len() as u32,
        usd_per_done_task: (!priced.is_empty())
            .then(|| read.iter().sum::<f64>() / read.len() as f64),
    }
}

/// Turns and tokens of firstmate's coordinator (`coordinator.baseline`) and
/// the native one, from `quark_coordinator::efficiency`.
fn coordinator(events: &[Event], in_window: impl Fn(OffsetDateTime) -> bool) -> CoordinatorMetrics {
    let e = Efficiency::fold(events, |e| in_window(e.ts));
    let side = |s: &Side| {
        let per = s.per_task(e.tasks);
        CoordinatorTurns {
            turns: s.turns,
            ack_turns: s.acks,
            input_tokens: s.usage.input,
            output_tokens: s.usage.output,
            cache_read_tokens: s.usage.cache_read,
            turns_per_task: per.map(|p| p.0),
            tokens_per_task: per.map(|p| p.1),
            ack_share: s.ack_share(),
        }
    };
    CoordinatorMetrics {
        tasks: e.tasks,
        baseline: side(&e.baseline),
        native: side(&e.native),
        would_wake_turns: e.would_wake,
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

    #[test]
    fn spend_comes_from_usage_turns_by_when_they_started() {
        let mut log = Log(Vec::new());
        let usage = |agent: &str, id: &str, at: &str, model: &str, continued: bool| {
            serde_json::json!({"agent": agent, "harness": "claude", "id": id, "at": at,
                "continued": continued, "models": [{"model": model, "input": 1_000_000,
                "output": 100_000, "cache_read": 0, "cache_write": 0, "calls": 1}]})
        };
        log.spawn(datetime!(2026-09-01 09:00 UTC), "a");
        // Before the window, but its spend counts toward the task's cost.
        log.push(
            datetime!(2026-10-07 09:00 UTC),
            "a",
            crate::usage::KIND,
            usage(
                "worker",
                "w0",
                "2026-09-01T09:00:00Z",
                "claude-sonnet-5-5",
                false,
            ),
        );
        log.push(
            datetime!(2026-10-07 09:00 UTC),
            "a",
            crate::usage::KIND,
            usage(
                "worker",
                "w1",
                "2026-10-07T09:00:00Z",
                "claude-sonnet-5-5",
                false,
            ),
        );
        log.push(
            datetime!(2026-10-07 09:05 UTC),
            "a",
            crate::usage::KIND,
            usage("worker", "w1", "2026-10-07T09:00:00Z", "mystery-1", true),
        );
        log.status(datetime!(2026-10-07 10:00 UTC), "a", "done");
        let mut e = NewEvent::new(
            HostId::from("h"),
            ProjectId::from("p"),
            None,
            crate::usage::KIND,
            usage(
                "coordinator",
                "c1",
                "2026-10-07T08:00:00Z",
                "claude-opus-5-5",
                false,
            ),
        );
        e.ts = datetime!(2026-10-07 08:00 UTC);
        let seq = Seq(log.0.len() as u64 + 1);
        log.0.push(e.with_seq(seq));

        let m = compute("p", &log.0, &[], NOW, 7);
        let s = &m.spend;
        assert_eq!(s.workers.turns, 1);
        assert_eq!(s.workers.input_tokens, 2_000_000);
        // 1M in at 2 and 0.1M out at 10; the unknown model is not priced.
        assert!((s.workers.usd.unwrap() - 3.0).abs() < 1e-9);
        assert_eq!(s.workers.unpriced_tokens, 1_100_000);
        assert!((s.coordinator.usd.unwrap() - 6.0).abs() < 1e-9);
        assert_eq!(s.by_model[0].model, "claude-opus-5-5");
        assert_eq!(s.done_tasks, 1);
        assert!((s.usd_per_done_task.unwrap() - 6.0).abs() < 1e-9);
        // No transcript baseline: the coordinator's turns stand in for it.
        assert_eq!(m.coordinator.baseline.turns, 1);
        assert_eq!(m.coordinator.baseline.ack_share, None);
        assert!(m.unavailable.is_empty());
    }

    #[test]
    fn coordinator_turns_come_from_the_baseline_in_the_window() {
        let mut log = Log(Vec::new());
        log.spawn(datetime!(2026-10-07 09:00 UTC), "t1");
        log.spawn(datetime!(2026-10-07 09:30 UTC), "t2");
        let turn = |id: &str, cause: &str, acts: bool| {
            serde_json::json!({"type": "baseline", "turn": {"id": id, "at": "", "cause": cause,
                "usage": {"input": 900, "output": 100, "cache_read": 800, "calls": 2},
                "tool_calls": 1, "acts": acts}})
        };
        for (i, (cause, acts)) in [
            ("wake", false),
            ("wake", false),
            ("wake", true),
            ("user", true),
        ]
        .into_iter()
        .enumerate()
        {
            let mut e = NewEvent::new(
                HostId::from("h"),
                ProjectId::from("p"),
                None,
                "coordinator.baseline",
                turn(&i.to_string(), cause, acts),
            );
            e.ts = datetime!(2026-10-07 10:00 UTC);
            let seq = Seq(log.0.len() as u64 + 1);
            log.0.push(e.with_seq(seq));
        }
        let m = compute("p", &log.0, &[], NOW, 7);
        let b = &m.coordinator.baseline;
        assert_eq!(m.coordinator.tasks, 2);
        assert_eq!((b.turns, b.ack_turns), (4, 2));
        assert_eq!(
            (b.input_tokens, b.output_tokens, b.cache_read_tokens),
            (3600, 400, 3200)
        );
        assert_eq!(b.turns_per_task, Some(2.0));
        assert_eq!(b.tokens_per_task, Some(2000.0));
        assert_eq!(b.ack_share, Some(0.5));
        assert_eq!(m.coordinator.native.turns, 0);
        assert!(m
            .unavailable
            .iter()
            .all(|u| u.metric != "coordinator_tokens"));
    }
}
