//! Every shadow at once, and how ready each slice is to switch on.
//!
//! Each native slice's shadow has its own setting. `QUARK_SHADOWS=all`
//! ([`ENV`]) turns on every one that only observes: it becomes the default
//! for the opt-in flags ([`opt_in`]) and for `QUARK_ENGINE_SLICES`, which
//! then puts every slice quarkd can shadow in `shadow`. A flag set
//! explicitly still wins, so `QUARK_SHADOWS=all QUARK_NATIVE_DISPATCH=0`
//! leaves dispatch off. The native supervisor (`QUARK_NATIVE_SUPERVISOR`)
//! is not a shadow, so `all` leaves it alone.
//!
//! On every start the daemon records which shadows it ran as one
//! `shadow.started` event ([`STARTED`]). [`ReadinessModel`] folds those and
//! every `shadow.divergence` into a per-slice [`ShadowReadiness`], served
//! at `GET /v1/shadows` and printed by `quarkd shadows`.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use quark_core::event::kinds;
use quark_core::slice::Divergence;
use quark_core::{
    Event, EventLog, NewEvent, ProjectId, Result, Seq, Slice, SliceMode, SliceSwitch,
};
use quark_eventlog::SqliteEventLog;
use quark_systems::{
    DivergenceExample, OperationCount, ShadowReadiness, ShadowStart, ShadowStatus, SliceReadiness,
};
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

/// Set to `all` to turn on every shadow.
pub const ENV: &str = "QUARK_SHADOWS";
/// Which shadows a daemon start ran. Payload: [`Started`].
pub const STARTED: &str = "shadow.started";
/// A daemon start, recorded before it writes anything else, so the run
/// before it ended with the last event ahead of it. No payload.
pub const DAEMON_STARTED: &str = "shadow.daemon_started";
/// Default window of `quarkd shadows` and `GET /v1/shadows`.
pub const DEFAULT_DAYS: u32 = 7;
/// Most examples listed per slice.
pub const EXAMPLES: usize = 5;
/// Longest preview of either side of an example, in characters.
pub const PREVIEW: usize = 300;
const READ_BATCH: usize = 1000;

/// Whether [`ENV`] asks for every shadow.
pub fn all() -> bool {
    std::env::var(ENV).is_ok_and(|v| v.trim() == "all")
}

/// An opt-in shadow flag: on when set to `1`, off when set to anything
/// else, and when unset, on only under `QUARK_SHADOWS=all`.
pub fn opt_in(var: &str) -> bool {
    resolve_opt_in(std::env::var(var).ok().as_deref(), all())
}

fn resolve_opt_in(value: Option<&str>, all: bool) -> bool {
    match value {
        Some(v) => v == "1",
        None => all,
    }
}

/// `QUARK_ENGINE_SLICES` when it is unset and every shadow is asked for:
/// every slice quarkd can shadow, in `shadow`.
pub fn all_slices() -> SliceSwitch {
    let mut s = SliceSwitch::new();
    for slice in crate::config::SHADOWABLE {
        s.set(slice, SliceMode::Shadow, ENV)
            .expect("shadowable slices are a prefix of the switch-on order");
    }
    s
}

/// Payload of a [`STARTED`] event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Started {
    pub slices: Vec<Slice>,
    /// Slices running native.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native: Vec<Slice>,
}

/// Record that a daemon started. Call before anything else appends.
pub async fn record_daemon_started(log: &dyn EventLog, host: quark_core::HostId) {
    let event = NewEvent::typed(
        host,
        ProjectId::engine(),
        None,
        DAEMON_STARTED,
        &serde_json::json!({}),
    );
    if let Err(e) = match event {
        Ok(e) => log.append(e).await.map(drop),
        Err(e) => Err(e),
    } {
        tracing::warn!(error = %e, "could not record the daemon start");
    }
}

/// Record that this start runs the shadows of `slices`, and runs `native`
/// natively.
pub async fn record_started(
    log: &dyn EventLog,
    host: quark_core::HostId,
    slices: Vec<Slice>,
    native: Vec<Slice>,
) {
    let names: Vec<&str> = slices.iter().map(|s| s.as_str()).collect();
    let natives: Vec<&str> = native.iter().map(|s| s.as_str()).collect();
    tracing::info!(shadows = ?names, native = ?natives, "shadows running");
    let event = NewEvent::typed(
        host,
        ProjectId::engine(),
        None,
        STARTED,
        &Started { slices, native },
    );
    if let Err(e) = match event {
        Ok(e) => log.append(e).await.map(drop),
        Err(e) => Err(e),
    } {
        tracing::warn!(error = %e, "could not record which shadows are running");
    }
}

/// Each slice's shadow: how to turn it on, whether it compares, and what.
struct Spec {
    slice: Slice,
    switch: Option<&'static str>,
    compares: bool,
    note: &'static str,
}

const SPECS: [Spec; 9] = [
    Spec {
        slice: Slice::EventLog,
        switch: Some("QUARK_ENGINE_SLICES=1=shadow"),
        compares: true,
        note: "reads the fleet, status tails and open decisions back from the event log and compares them with firstmate's",
    },
    Spec {
        slice: Slice::Verification,
        switch: Some("QUARK_ENGINE_SLICES=1=shadow,2=shadow"),
        compares: true,
        note: "replays firstmate's merge-guard and dispatch-pause decisions with native rules",
    },
    Spec {
        slice: Slice::WorkerProtocol,
        switch: Some("QUARK_ENGINE_SLICES=1=shadow,2=shadow,3=shadow"),
        compares: true,
        note: "reads every firstmate status line with the native file protocol and compares the reading with firstmate's",
    },
    Spec {
        slice: Slice::Supervision,
        switch: Some("QUARK_ENGINE_SLICES=1=shadow,2=shadow,3=shadow,4=shadow"),
        compares: true,
        note: "replays firstmate's spawns and status lines through the native supervisor's rules and compares each task's state with firstmate's",
    },
    Spec {
        slice: Slice::Dispatch,
        switch: Some("QUARK_NATIVE_DISPATCH=1"),
        compares: true,
        note: "resolves each dispatch natively and compares with firstmate's resolution",
    },
    Spec {
        slice: Slice::Coordinator,
        switch: Some("QUARK_NATIVE_COORDINATOR (on unless 0)"),
        compares: true,
        note: "checks that every wake turn in which firstmate's coordinator acted had a native would-wake within 10 minutes",
    },
    Spec {
        slice: Slice::SubCoordinators,
        switch: Some("QUARK_NATIVE_TRIGGERS (on unless 0)"),
        compares: true,
        note: "compares the native away classifier's routing with firstmate's",
    },
    Spec {
        slice: Slice::Sandbox,
        switch: Some("QUARK_SHADOW_DASHBOARD=1"),
        compares: true,
        note: "compares the Overview's live task states with firstmate's fleet",
    },
    Spec {
        slice: Slice::WorktreePool,
        switch: Some("QUARK_NATIVE_WORKTREES=1"),
        compares: true,
        note: "asks the native pool what it would do on each treehouse call",
    },
];

/// The fold of one event log's shadow events.
pub struct ReadinessModel {
    log: SqliteEventLog,
    after: Seq,
    starts: Vec<(OffsetDateTime, BTreeSet<Slice>)>,
    /// The slices the latest start ran native.
    native: BTreeSet<Slice>,
    slices: HashMap<Slice, SliceFold>,
}

#[derive(Default)]
struct SliceFold {
    /// Time and operation of every divergence, oldest first.
    seen: Vec<(OffsetDateTime, String)>,
    /// The newest few, newest last.
    recent: VecDeque<(OffsetDateTime, DivergenceExample)>,
}

impl ReadinessModel {
    pub fn new(log: SqliteEventLog) -> Self {
        Self {
            log,
            after: Seq::ZERO,
            starts: Vec::new(),
            native: BTreeSet::new(),
            slices: HashMap::new(),
        }
    }

    /// Fold everything appended since the last call.
    pub async fn catch_up(&mut self) -> Result<()> {
        loop {
            let events = self.log.read(self.after, READ_BATCH).await?;
            let Some(last) = events.last() else {
                return Ok(());
            };
            self.after = last.seq;
            for e in &events {
                self.apply(e);
            }
        }
    }

    fn apply(&mut self, e: &Event) {
        match e.kind.as_str() {
            STARTED => {
                if let Ok(s) = e.decode::<Started>() {
                    self.starts.push((e.ts, s.slices.into_iter().collect()));
                    self.native = s.native.into_iter().collect();
                }
            }
            kinds::SHADOW_DIVERGENCE => {
                let Ok(d) = e.decode::<Divergence>() else {
                    return;
                };
                let f = self.slices.entry(d.slice).or_default();
                f.seen.push((e.ts, d.operation.clone()));
                f.recent.push_back((
                    e.ts,
                    DivergenceExample {
                        seq: e.seq.0,
                        at: rfc3339(e.ts),
                        project_id: e.project.as_str().to_string(),
                        task: e.task.as_ref().map(|t| t.as_str().to_string()),
                        operation: d.operation,
                        bash: preview(&d.bash),
                        native: preview(&d.native),
                    },
                ));
                if f.recent.len() > EXAMPLES {
                    f.recent.pop_front();
                }
            }
            _ => {}
        }
    }

    /// Every slice's readiness over the `days` before `now`.
    pub fn report(&self, days: u32, now: OffsetDateTime) -> ShadowReadiness {
        let from = now - Duration::days(days.into());
        let latest = self.starts.last();
        let slices = SPECS
            .iter()
            .map(|spec| {
                let fold = self.slices.get(&spec.slice);
                let in_window: Vec<&String> = fold
                    .map(|f| {
                        f.seen
                            .iter()
                            .filter(|(at, _)| *at >= from)
                            .map(|(_, op)| op)
                            .collect()
                    })
                    .unwrap_or_default();
                let mut ops: BTreeMap<&str, u64> = BTreeMap::new();
                for op in &in_window {
                    *ops.entry(op.as_str()).or_default() += 1;
                }
                let mut operations: Vec<OperationCount> = ops
                    .into_iter()
                    .map(|(operation, count)| OperationCount {
                        operation: operation.to_string(),
                        count,
                    })
                    .collect();
                operations.sort_by_key(|o| std::cmp::Reverse(o.count));
                let on_since = self.on_since(spec.slice);
                let on = latest.is_some_and(|(_, s)| s.contains(&spec.slice));
                let divergences = in_window.len() as u64;
                let status = if self.native.contains(&spec.slice) {
                    ShadowStatus::Native
                } else if !spec.compares {
                    ShadowStatus::NoCheck
                } else if divergences > 0 {
                    ShadowStatus::Diverging
                } else if !on {
                    ShadowStatus::Off
                } else if on_since.is_some_and(|t| t <= from) {
                    ShadowStatus::Agreeing
                } else {
                    ShadowStatus::Watching
                };
                SliceReadiness {
                    number: spec.slice.number(),
                    slice: spec.slice.as_str().to_string(),
                    status,
                    switch: spec.switch.map(str::to_string),
                    note: spec.note.to_string(),
                    on_since: on_since.map(rfc3339),
                    agrees_at: (status == ShadowStatus::Watching)
                        .then(|| on_since.map(|t| rfc3339(t + Duration::days(days.into()))))
                        .flatten(),
                    divergences,
                    operations,
                    last_divergence_at: fold.and_then(|f| f.seen.last()).map(|(t, _)| rfc3339(*t)),
                    examples: fold
                        .map(|f| {
                            f.recent
                                .iter()
                                .rev()
                                .filter(|(at, _)| *at >= from)
                                .map(|(_, x)| x.clone())
                                .collect()
                        })
                        .unwrap_or_default(),
                }
            })
            .collect();
        ShadowReadiness {
            days,
            latest_start: latest.map(|(at, s)| ShadowStart {
                at: rfc3339(*at),
                slices: s.iter().map(|x| x.as_str().to_string()).collect(),
                native: self.native.iter().map(|x| x.as_str().to_string()).collect(),
            }),
            slices,
            error: None,
        }
    }

    /// The first of the unbroken run of latest starts that ran `slice`.
    fn on_since(&self, slice: Slice) -> Option<OffsetDateTime> {
        self.starts
            .iter()
            .rev()
            .take_while(|(_, s)| s.contains(&slice))
            .last()
            .map(|(at, _)| *at)
    }
}

/// The shared model for `log`'s file, so each request folds only what is
/// new.
pub fn shared(log: &SqliteEventLog) -> Arc<tokio::sync::Mutex<ReadinessModel>> {
    type Models = Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<ReadinessModel>>>>;
    static MODELS: OnceLock<Models> = OnceLock::new();
    let fresh = || Arc::new(tokio::sync::Mutex::new(ReadinessModel::new(log.clone())));
    let path = log.path();
    if path.as_os_str().is_empty() || path == Path::new(":memory:") {
        return fresh();
    }
    MODELS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .entry(path.to_path_buf())
        .or_insert_with(fresh)
        .clone()
}

/// `r` as the text `quarkd shadows` prints.
pub fn render(r: &ShadowReadiness) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(out, "Shadow readiness, last {} days", r.days);
    match &r.latest_start {
        Some(s) if s.slices.is_empty() => {
            let _ = writeln!(out, "Latest daemon start {}: no shadows on", s.at);
        }
        Some(s) => {
            let _ = writeln!(out, "Latest daemon start {}: {}", s.at, s.slices.join(", "));
        }
        None => {
            let _ = writeln!(out, "No daemon start recorded which shadows ran");
        }
    }
    if let Some(s) = r.latest_start.as_ref().filter(|s| !s.native.is_empty()) {
        let _ = writeln!(out, "Running native: {}", s.native.join(", "));
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "{:>2}  {:<16}  {:<9}  {:>11}  {:<20}",
        "#", "slice", "status", "divergences", "on since"
    );
    for s in &r.slices {
        let divergences = if s.status == ShadowStatus::NoCheck {
            "-".to_string()
        } else {
            s.divergences.to_string()
        };
        let _ = writeln!(
            out,
            "{:>2}  {:<16}  {:<9}  {:>11}  {:<20}",
            s.number,
            s.slice,
            status_label(s.status),
            divergences,
            s.on_since.as_deref().map(short_time).unwrap_or("-")
        );
    }
    let _ = writeln!(out);
    for s in &r.slices {
        let _ = writeln!(out, "{} {}: {}", s.number, s.slice, s.note);
        if let Some(sw) = &s.switch {
            let _ = writeln!(out, "  on with: {sw}");
        }
        if let Some(at) = &s.agrees_at {
            let _ = writeln!(
                out,
                "  agreeing from: {} if it keeps running and nothing diverges",
                short_time(at)
            );
        }
        if !s.operations.is_empty() {
            let ops: Vec<String> = s
                .operations
                .iter()
                .map(|o| format!("{} x{}", o.operation, o.count))
                .collect();
            let _ = writeln!(out, "  divergences: {}", ops.join(", "));
        }
        for x in &s.examples {
            let task = x
                .task
                .as_deref()
                .map(|t| format!(" task {t}"))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "  #{} {} {} project {}{task}",
                x.seq,
                short_time(&x.at),
                x.operation,
                x.project_id
            );
            let _ = writeln!(out, "    bash:   {}", x.bash);
            let _ = writeln!(out, "    native: {}", x.native);
        }
    }
    out
}

fn status_label(s: ShadowStatus) -> &'static str {
    match s {
        ShadowStatus::NoCheck => "no check",
        ShadowStatus::Off => "off",
        ShadowStatus::Watching => "watching",
        ShadowStatus::Diverging => "diverging",
        ShadowStatus::Agreeing => "agreeing",
        ShadowStatus::Native => "native",
    }
}

/// `2026-10-07T05:33:27.123Z` as `2026-10-07 05:33Z`.
fn short_time(t: &str) -> &str {
    t.get(..16).unwrap_or(t)
}

fn preview(v: &serde_json::Value) -> String {
    let s = v.to_string();
    match s.char_indices().nth(PREVIEW) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s,
    }
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use quark_core::{HostId, TaskId};

    fn at(minutes_ago: i64, now: OffsetDateTime) -> OffsetDateTime {
        now - Duration::minutes(minutes_ago)
    }

    async fn append(
        log: &SqliteEventLog,
        ts: OffsetDateTime,
        kind: &str,
        payload: serde_json::Value,
    ) {
        let mut e = NewEvent::new(
            HostId::new("h"),
            ProjectId::new("p1"),
            Some(TaskId::from("t1")),
            kind,
            payload,
        );
        e.ts = ts;
        log.append(e).await.unwrap();
    }

    fn started(slices: &[Slice]) -> serde_json::Value {
        serde_json::to_value(Started {
            slices: slices.to_vec(),
            native: vec![],
        })
        .unwrap()
    }

    fn divergence(slice: Slice, op: &str) -> serde_json::Value {
        serde_json::to_value(Divergence {
            slice,
            operation: op.into(),
            bash: serde_json::json!({ "status": "clear" }),
            native: serde_json::json!({ "status": "escalate", "long": "x".repeat(400) }),
        })
        .unwrap()
    }

    fn slice(r: &ShadowReadiness, s: Slice) -> &SliceReadiness {
        r.slices.iter().find(|x| x.slice == s.as_str()).unwrap()
    }

    #[test]
    fn an_explicit_flag_beats_all() {
        assert!(resolve_opt_in(None, true));
        assert!(!resolve_opt_in(None, false));
        assert!(!resolve_opt_in(Some("0"), true));
        assert!(resolve_opt_in(Some("1"), false));
    }

    #[test]
    fn all_puts_every_shadowable_slice_in_shadow() {
        let s = all_slices();
        assert_eq!(s.mode(Slice::EventLog), SliceMode::Shadow);
        assert_eq!(s.mode(Slice::Verification), SliceMode::Shadow);
        assert_eq!(s.mode(Slice::WorkerProtocol), SliceMode::Shadow);
        assert_eq!(s.mode(Slice::Supervision), SliceMode::Shadow);
        assert_eq!(s.mode(Slice::Dispatch), SliceMode::Bash);
        crate::config::check_slices(&s).unwrap();
    }

    #[tokio::test]
    async fn readiness_reads_starts_and_divergences() {
        let dir = tempfile::tempdir().unwrap();
        let log = SqliteEventLog::open(dir.path().join("events.db")).unwrap();
        let now = OffsetDateTime::now_utc();
        let day = 24 * 60;
        // Verification on for ten days and quiet; dispatch on for ten days
        // but diverging; worktrees on since yesterday only; triggers turned
        // off on the latest start.
        let long = [Slice::Verification, Slice::Dispatch, Slice::SubCoordinators];
        append(&log, at(10 * day, now), STARTED, started(&long)).await;
        append(
            &log,
            at(2 * day, now),
            kinds::SHADOW_DIVERGENCE,
            divergence(Slice::SubCoordinators, "away.route"),
        )
        .await;
        append(
            &log,
            at(day, now),
            STARTED,
            started(&[Slice::Verification, Slice::Dispatch, Slice::WorktreePool]),
        )
        .await;
        // Outside the window: counts toward last_divergence_at only.
        append(
            &log,
            at(9 * day, now),
            kinds::SHADOW_DIVERGENCE,
            divergence(Slice::Dispatch, "old"),
        )
        .await;
        for _ in 0..3 {
            append(
                &log,
                at(60, now),
                kinds::SHADOW_DIVERGENCE,
                divergence(Slice::Dispatch, "resolve_dispatch"),
            )
            .await;
        }
        append(
            &log,
            at(30, now),
            kinds::SHADOW_DIVERGENCE,
            divergence(Slice::Dispatch, "other"),
        )
        .await;

        let mut model = ReadinessModel::new(log);
        model.catch_up().await.unwrap();
        let r = model.report(7, now);

        assert_eq!(r.slices.len(), 9);
        assert_eq!(r.latest_start.as_ref().unwrap().slices.len(), 3);

        let v = slice(&r, Slice::Verification);
        assert_eq!(v.status, ShadowStatus::Agreeing);
        assert_eq!(v.divergences, 0);

        let d = slice(&r, Slice::Dispatch);
        assert_eq!(d.status, ShadowStatus::Diverging);
        assert_eq!(d.divergences, 4);
        assert_eq!(d.operations[0].operation, "resolve_dispatch");
        assert_eq!(d.operations[0].count, 3);
        assert_eq!(d.examples.len(), 4);
        assert_eq!(d.examples[0].operation, "other");
        assert!(d.examples[0].native.ends_with('…'));
        assert_eq!(d.examples[0].task.as_deref(), Some("t1"));

        let w = slice(&r, Slice::WorktreePool);
        assert_eq!(w.status, ShadowStatus::Watching);
        let on = OffsetDateTime::parse(w.on_since.as_deref().unwrap(), &Rfc3339).unwrap();
        let agrees = OffsetDateTime::parse(w.agrees_at.as_deref().unwrap(), &Rfc3339).unwrap();
        assert_eq!(agrees - on, Duration::days(7));
        assert!(v.agrees_at.is_none());
        // Diverged inside the window even though it is off now.
        assert_eq!(
            slice(&r, Slice::SubCoordinators).status,
            ShadowStatus::Diverging
        );
        assert_eq!(slice(&r, Slice::Supervision).status, ShadowStatus::Off);

        let text = render(&r);
        assert!(text.contains("resolve_dispatch x3"), "{text}");
        assert!(text.contains("diverging"), "{text}");
    }

    #[tokio::test]
    async fn a_quiet_shadow_that_is_off_reads_off() {
        let dir = tempfile::tempdir().unwrap();
        let log = SqliteEventLog::open(dir.path().join("events.db")).unwrap();
        let now = OffsetDateTime::now_utc();
        append(&log, at(60, now), STARTED, started(&[])).await;
        let mut model = ReadinessModel::new(log);
        model.catch_up().await.unwrap();
        let r = model.report(DEFAULT_DAYS, now);
        assert_eq!(slice(&r, Slice::WorktreePool).status, ShadowStatus::Off);
        assert!(render(&r).contains("no shadows on"));
    }

    #[tokio::test]
    async fn a_slice_switched_native_reads_native() {
        let dir = tempfile::tempdir().unwrap();
        let log = SqliteEventLog::open(dir.path().join("events.db")).unwrap();
        let now = OffsetDateTime::now_utc();
        let payload = serde_json::to_value(Started {
            slices: vec![Slice::Verification],
            native: vec![Slice::EventLog],
        })
        .unwrap();
        append(&log, at(60, now), STARTED, payload).await;
        let mut model = ReadinessModel::new(log);
        model.catch_up().await.unwrap();
        let r = model.report(DEFAULT_DAYS, now);
        assert_eq!(slice(&r, Slice::EventLog).status, ShadowStatus::Native);
        assert_eq!(
            slice(&r, Slice::Verification).status,
            ShadowStatus::Watching
        );
        let text = render(&r);
        assert!(text.contains("Running native: event_log"), "{text}");
        assert!(text.contains("agreeing from:"), "{text}");
    }
}
