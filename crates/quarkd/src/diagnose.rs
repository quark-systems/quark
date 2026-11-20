//! What went wrong lately in a Quark home, as one report.
//!
//! `quarkd diagnose` and `GET /v1/diagnostics` read the daemon's log, the
//! store and the event log for a window (a day by default) and fold the
//! trouble they find into findings: warnings and errors the daemon logged,
//! engine reads that failed, supervisor and dispatch trouble, workers that
//! went quiet, decisions left waiting, and a store that grew too big. Each
//! finding has a fingerprint that stays the same while the trouble does
//! (numbers and ids are masked), so a triage pass can file it once and see
//! later passes as the same thing.
//!
//! It opens both databases read-only, so it also works with quarkd down.
//! It reports counts, kinds, ids and log messages, never terminal output or
//! transcripts.
//!
//! # Run by another quarkd than the home's
//!
//! `quarkd diagnose` and `quarkd triage` are whichever binary was started,
//! which need not be the quarkd that runs the home: a build left in a
//! source checkout, or one a script still names after an update. One too
//! old for the home (the home's format, `quark.db` or the event log moved on
//! in a way it can't read, see [`crate::home_format`] and
//! `store/migrations.rs`) refuses with [`Unreadable`] instead of reporting
//! what it misreads, so triage files nothing from it. A schema that differs
//! but is usable is a fact about the reader, not trouble in the home: it is
//! reported as [`Diagnostics::quark_db_schema`], never as a finding.

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags};
use serde::Serialize;
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};
use utoipa::ToSchema;

/// The window a report covers by default.
pub const DEFAULT_WINDOW: Duration = Duration::hours(24);
/// An unfinished worker silent this long is reported as quiet.
const QUIET_AFTER: Duration = Duration::hours(6);
/// A decision open this long is reported as waiting.
const WAITING_AFTER: Duration = Duration::hours(12);
/// A store bigger than this is reported.
const BIG_STORE: u64 = 500 * 1024 * 1024;
/// Event-log kinds that mean trouble, with the severity they report at.
const TROUBLE_KINDS: &[(&str, Severity)] = &[
    // A quiet worker is the fleet check's to act on (it nudges, then wakes
    // the coordinator); one that stays quiet for hours is the task finding.
    ("supervisor.stale", Severity::Info),
    ("supervisor.exited", Severity::Warn),
    ("dispatch.spawn_failed", Severity::Error),
    ("dispatch.host_unhealthy", Severity::Warn),
    ("dispatch.escalated", Severity::Info),
    ("shadow.divergence", Severity::Warn),
    ("verify.red_main", Severity::Warn),
];
/// Samples kept per finding.
const SAMPLES: usize = 3;
/// Longest sample text.
const SAMPLE_CHARS: usize = 400;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, ToSchema, clap::ValueEnum,
)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warn,
    Error,
}

/// One kind of trouble seen in the window.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Finding {
    /// Stable while the trouble is the same: masks numbers and ids.
    pub fingerprint: String,
    pub severity: Severity,
    /// Where it was seen: `log`, `adapter_call`, `event`, `task`,
    /// `decision` or `store`.
    pub source: String,
    /// What it is, with numbers and ids masked.
    pub summary: String,
    pub count: u64,
    pub first_seen: String,
    pub last_seen: String,
    /// Projects it was seen in.
    pub projects: Vec<String>,
    /// A few instances, as they were.
    pub samples: Vec<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StoreSizes {
    pub quark_db_bytes: u64,
    pub events_db_bytes: u64,
    pub logs_bytes: u64,
    /// The event types and kinds holding the most payload, biggest first.
    pub largest: Vec<KindSize>,
}

/// How much of a database one event type (quark.db) or kind (events.db)
/// takes.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct KindSize {
    /// `quark.db` or `events.db`.
    pub db: String,
    pub kind: String,
    pub events: u64,
    pub payload_bytes: u64,
}

/// Kinds listed per database in [`StoreSizes::largest`].
const LARGEST_KINDS: usize = 6;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Diagnostics {
    pub home: String,
    pub host: String,
    /// The quarkd that made the report, which is the daemon for
    /// `GET /v1/diagnostics` and whichever binary ran `quarkd diagnose`.
    pub version: String,
    /// `quark.db`'s schema version when it is not the one this quarkd
    /// writes: another quarkd migrated the home last (an update this one
    /// predates, or one rolled back).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quark_db_schema: Option<i64>,
    pub generated_at: String,
    pub since: String,
    /// When the daemon last logged, if it logs to the home.
    pub last_log_at: Option<String>,
    pub sizes: StoreSizes,
    /// Worst first, then most frequent.
    pub findings: Vec<Finding>,
}

/// Why a quarkd won't report on a home: it can't read what it finds there.
/// The message says what to run instead.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Unreadable(String);

/// A database a newer quarkd moved past this one.
fn too_new(db: &Path, why: &str) -> Unreadable {
    Unreadable(format!(
        "{}: {why}; quarkd {} is too old to report on this home. \
         Run the newer Quark, or give this one another home with --home",
        db.display(),
        env!("CARGO_PKG_VERSION")
    ))
}

/// Builds the report for `home` covering everything since `since`, or
/// refuses a home this quarkd can't read (see the module docs).
pub fn diagnose(
    home: &Path,
    since: OffsetDateTime,
    now: OffsetDateTime,
) -> Result<Diagnostics, Unreadable> {
    crate::home_format::check_readable(home).map_err(|e| Unreadable(format!("{e:#}")))?;
    let since_s = fmt(since);
    let mut f = Findings::default();
    let logs = home.join("logs");
    let last_log_at = read_logs(&logs, &since_s, &mut f);
    let quark_db = home.join("quark.db");
    let mut largest = Vec::new();
    let mut quark_db_schema = None;
    if let Some(c) = open(&quark_db) {
        let ours = crate::store::SCHEMA_VERSION;
        // A database that can't be read at all is trouble in the home; one
        // a newer quarkd moved past this one is this quarkd's to refuse.
        let schema = match crate::store::usable_schema(&c) {
            Ok(v) => Some(v),
            Err(crate::store::StoreError::Invalid(m)) => return Err(too_new(&quark_db, &m)),
            Err(_) => None,
        };
        quark_db_schema = schema.filter(|v| *v != ours);
        if let Err(e) = read_store(&c, &since_s, now, &mut f) {
            // An older database lacks what this quarkd reads, and a reader
            // can't migrate it.
            if let Some(v) = schema.filter(|v| *v < ours) {
                return Err(Unreadable(format!(
                    "{} is at schema {v}, older than quarkd {} reads ({ours}), and reading it \
                     failed: {e}. Run the quarkd that runs this home, or start this one on it \
                     once to migrate it",
                    quark_db.display(),
                    env!("CARGO_PKG_VERSION")
                )));
            }
            f.add_error("store", &format!("could not read quark.db: {e}"));
        }
        largest.extend(kind_sizes(&c, "quark.db", "type").unwrap_or_default());
    }
    let events_db = home.join(quark_eventlog::FILE_NAME);
    if let Some(c) = open(&events_db) {
        if let Err(quark_core::CoreError::Unsupported(m)) = quark_eventlog::usable_version(&c) {
            return Err(too_new(&events_db, &m));
        }
        if let Err(e) = read_events(&c, &since_s, &mut f) {
            f.add_error("event", &format!("could not read the event log: {e}"));
        }
        largest.extend(kind_sizes(&c, "events.db", "kind").unwrap_or_default());
    }
    let sizes = StoreSizes {
        quark_db_bytes: db_size(&quark_db),
        events_db_bytes: db_size(&events_db),
        logs_bytes: dir_size(&logs),
        largest,
    };
    for (name, bytes) in [
        ("quark.db", sizes.quark_db_bytes),
        ("events.db", sizes.events_db_bytes),
    ] {
        if bytes > BIG_STORE {
            f.add(
                Severity::Warn,
                "store",
                &format!("{name} is over {} MB", BIG_STORE / 1024 / 1024),
                &fmt(now),
                None,
                format!("{name} is {} MB", bytes / 1024 / 1024),
            );
        }
    }
    Ok(Diagnostics {
        home: home.display().to_string(),
        host: hostname(),
        version: env!("CARGO_PKG_VERSION").into(),
        quark_db_schema,
        generated_at: fmt(now),
        since: since_s,
        last_log_at,
        sizes,
        findings: f.into_sorted(),
    })
}

/// Plain-text report for a terminal.
pub fn render(d: &Diagnostics) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let mb = |b: u64| b as f64 / 1024.0 / 1024.0;
    let _ = writeln!(out, "quarkd {} on {} ({})", d.version, d.host, d.home);
    if let Some(schema) = d.quark_db_schema {
        let _ = writeln!(
            out,
            "quark.db is at schema {schema}; this quarkd writes {}: another quarkd migrated it last",
            crate::store::SCHEMA_VERSION
        );
    }
    let _ = writeln!(out, "since {}", d.since);
    let _ = writeln!(
        out,
        "last log line: {}",
        d.last_log_at.as_deref().unwrap_or("none")
    );
    let _ = writeln!(
        out,
        "quark.db {:.0} MB, events.db {:.0} MB, logs {:.1} MB",
        mb(d.sizes.quark_db_bytes),
        mb(d.sizes.events_db_bytes),
        mb(d.sizes.logs_bytes)
    );
    for k in &d.sizes.largest {
        let _ = writeln!(
            out,
            "  {} {}: {} events, {:.1} MB",
            k.db,
            k.kind,
            k.events,
            mb(k.payload_bytes)
        );
    }
    if d.findings.is_empty() {
        out.push_str("\nnothing wrong found\n");
    }
    for f in &d.findings {
        let _ = writeln!(
            out,
            "\n[{:?}] {} x{} ({}) {}\n  {} .. {}",
            f.severity, f.fingerprint, f.count, f.source, f.summary, f.first_seen, f.last_seen
        );
        if !f.projects.is_empty() {
            let _ = writeln!(out, "  projects: {}", f.projects.join(", "));
        }
        for s in &f.samples {
            let _ = writeln!(out, "  > {s}");
        }
    }
    out
}

#[derive(Default)]
struct Findings(BTreeMap<String, Finding>);

impl Findings {
    fn add(
        &mut self,
        severity: Severity,
        source: &str,
        summary: &str,
        ts: &str,
        project: Option<&str>,
        sample: String,
    ) {
        let summary = mask(summary);
        let fingerprint = fingerprint(source, &summary);
        let f = self
            .0
            .entry(fingerprint.clone())
            .or_insert_with(|| Finding {
                fingerprint,
                severity,
                source: source.into(),
                summary,
                count: 0,
                first_seen: ts.into(),
                last_seen: ts.into(),
                projects: Vec::new(),
                samples: Vec::new(),
            });
        f.count += 1;
        f.severity = f.severity.max(severity);
        if ts < f.first_seen.as_str() {
            f.first_seen = ts.into();
        }
        if ts > f.last_seen.as_str() {
            f.last_seen = ts.into();
        }
        if let Some(p) = project {
            if !f.projects.iter().any(|q| q == p) {
                f.projects.push(p.into());
            }
        }
        if f.samples.len() < SAMPLES {
            f.samples.push(truncate(&sample));
        }
    }

    fn add_error(&mut self, source: &str, msg: &str) {
        let now = fmt(OffsetDateTime::now_utc());
        self.add(Severity::Error, source, msg, &now, None, msg.into());
    }

    fn into_sorted(self) -> Vec<Finding> {
        let mut v: Vec<Finding> = self.0.into_values().collect();
        v.sort_by(|a, b| b.severity.cmp(&a.severity).then(b.count.cmp(&a.count)));
        v
    }
}

/// Warnings and errors from `<home>/logs/quarkd.*.jsonl` since `since`.
/// Returns the newest log timestamp seen in any file.
fn read_logs(dir: &Path, since: &str, f: &mut Findings) -> Option<String> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("quarkd.") && n.ends_with(".jsonl"))
        })
        .collect();
    files.sort();
    let mut last: Option<String> = None;
    for path in files {
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            let ts = v["timestamp"].as_str().unwrap_or_default().to_string();
            if last.as_deref().is_none_or(|l| ts.as_str() > l) {
                last = Some(ts.clone());
            }
            let severity = match v["level"].as_str() {
                Some("ERROR") => Severity::Error,
                Some("WARN") => Severity::Warn,
                _ => continue,
            };
            if ts.as_str() < since {
                continue;
            }
            let target = v["target"].as_str().unwrap_or("quarkd");
            let fields = &v["fields"];
            let message = fields["message"].as_str().unwrap_or_default();
            // `operation` splits otherwise identical messages usefully.
            let summary = match fields["operation"].as_str() {
                Some(op) => format!("{target}: {message} ({op})"),
                None => format!("{target}: {message}"),
            };
            let project = fields["project"].as_str();
            f.add(severity, "log", &summary, &ts, project, fields.to_string());
        }
    }
    last
}

fn read_store(
    c: &Connection,
    since: &str,
    now: OffsetDateTime,
    f: &mut Findings,
) -> rusqlite::Result<()> {
    let mut stmt = c.prepare(
        "SELECT ts, project_id, operation, COALESCE(detail, '') FROM adapter_calls
         WHERE ok = 0 AND ts >= ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map([since], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    for row in rows {
        let (ts, project, op, detail) = row?;
        // Scripts print banners and warnings before the error that matters.
        let first = detail
            .lines()
            .find(|l| l.trim_start().to_lowercase().starts_with("error"))
            .or_else(|| detail.lines().find(|l| !l.trim().is_empty()))
            .unwrap_or("");
        f.add(
            Severity::Error,
            "adapter_call",
            &format!("{op} failed: {first}"),
            &ts,
            project.as_deref(),
            detail,
        );
    }

    let quiet_before = fmt(now - QUIET_AFTER);
    // An older home has no `cleared_at`; nothing in it was cleared.
    let has_cleared: bool = c.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name = 'cleared_at'",
        [],
        |r| r.get(0),
    )?;
    let not_cleared = if has_cleared {
        "cleared_at IS NULL"
    } else {
        "1"
    };
    let mut stmt = c.prepare(&format!(
        "SELECT updated_at, project_id, id, state, COALESCE(state_note, '') FROM tasks
         WHERE {not_cleared}
           AND ((state = 'failed' AND updated_at >= ?1)
             OR (state IN ('running', 'blocked', 'unknown') AND updated_at < ?2))"
    ))?;
    let rows = stmt.query_map(params![since, quiet_before], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
        ))
    })?;
    for row in rows {
        let (ts, project, id, state, note) = row?;
        let (severity, summary) = if state == "failed" {
            (Severity::Warn, "a worker failed".to_string())
        } else {
            (
                Severity::Warn,
                format!(
                    "a {state} worker has not changed in {} hours",
                    QUIET_AFTER.whole_hours()
                ),
            )
        };
        f.add(
            severity,
            "task",
            &summary,
            &ts,
            Some(&project),
            format!("{id} {state}: {note}"),
        );
    }

    let waiting_before = fmt(now - WAITING_AFTER);
    let mut stmt = c.prepare(
        "SELECT opened_at, project_id, id FROM decisions
         WHERE state = 'open' AND opened_at < ?1",
    )?;
    let rows = stmt.query_map([&waiting_before], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (ts, project, id) = row?;
        f.add(
            Severity::Info,
            "decision",
            &format!(
                "a decision has waited over {} hours",
                WAITING_AFTER.whole_hours()
            ),
            &ts,
            Some(&project),
            id,
        );
    }
    Ok(())
}

fn read_events(c: &Connection, since: &str, f: &mut Findings) -> rusqlite::Result<()> {
    let mut stmt = c.prepare(
        "SELECT ts, project, COALESCE(task, ''), payload FROM events
         WHERE kind = ?1 AND ts >= ?2 ORDER BY seq",
    )?;
    for (kind, severity) in TROUBLE_KINDS {
        let rows = stmt.query_map([kind, &since], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (ts, project, task, payload) = row?;
            let v: serde_json::Value = serde_json::from_str(&payload).unwrap_or_default();
            // A clean exit is how a worker ends; only a failed one is trouble.
            if *kind == "supervisor.exited" && v["code"].as_i64() == Some(0) {
                continue;
            }
            let detail = [
                v["slice"].as_str(),
                v["operation"].as_str(),
                v["reason"].as_str(),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
            let summary = if detail.is_empty() {
                kind.to_string()
            } else {
                format!("{kind}: {detail}")
            };
            f.add(
                *severity,
                "event",
                &summary,
                &ts,
                Some(&project),
                format!("{task} {payload}"),
            );
        }
    }
    Ok(())
}

/// The [`LARGEST_KINDS`] event types or kinds (`column`) with the most
/// payload in `db`'s `events` table.
fn kind_sizes(c: &Connection, db: &str, column: &str) -> rusqlite::Result<Vec<KindSize>> {
    let mut stmt = c.prepare(&format!(
        "SELECT {column}, COUNT(*), COALESCE(SUM(LENGTH(payload)), 0) FROM events
         GROUP BY {column} ORDER BY 3 DESC LIMIT ?1"
    ))?;
    let rows = stmt.query_map([LARGEST_KINDS as i64], |r| {
        Ok(KindSize {
            db: db.to_string(),
            kind: r.get(0)?,
            events: r.get::<_, i64>(1)? as u64,
            payload_bytes: r.get::<_, i64>(2)? as u64,
        })
    })?;
    rows.collect()
}

fn open(path: &Path) -> Option<Connection> {
    if !path.exists() {
        return None;
    }
    let c = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let _ = c.busy_timeout(std::time::Duration::from_secs(5));
    Some(c)
}

/// The database and its WAL.
fn db_size(path: &Path) -> u64 {
    let wal = PathBuf::from(format!("{}-wal", path.display()));
    [path, wal.as_path()]
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
}

fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok()?.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

fn hostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

fn fmt(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).expect("UTC formats")
}

fn truncate(s: &str) -> String {
    match s.char_indices().nth(SAMPLE_CHARS) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// Masks what changes between instances of the same trouble: Quark ids
/// (`prj_…`, `tsk_…`), hex runs, numbers and quoted text.
pub fn mask(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let word_start = i == 0 || !chars[i - 1].is_ascii_alphanumeric();
        if word_start && c.is_ascii_alphanumeric() {
            let end = (i..chars.len())
                .find(|&j| !(chars[j].is_ascii_alphanumeric() || chars[j] == '_'))
                .unwrap_or(chars.len());
            let word: String = chars[i..end].iter().collect();
            out.push_str(&mask_word(&word));
            i = end;
            continue;
        }
        if c == '"' {
            if let Some(close) = (i + 1..chars.len()).find(|&j| chars[j] == '"') {
                out.push_str("\"…\"");
                i = close + 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out.trim().to_string()
}

fn mask_word(w: &str) -> String {
    let id_prefix = ["prj_", "tsk_", "dec_", "acc_", "cmsg_"];
    if id_prefix.iter().any(|p| w.starts_with(p)) {
        return format!("{}<id>", &w[..w.find('_').unwrap() + 1]);
    }
    let digits = w.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == w.len() {
        return "N".into();
    }
    // `10s`, `3rd`, `2xx`: a number with a unit.
    if digits > 0 && w[digits..].chars().all(|c| c.is_ascii_alphabetic()) && w.len() - digits <= 3 {
        return format!("N{}", &w[digits..]);
    }
    let hex = w.len() >= 8 && w.chars().all(|c| c.is_ascii_hexdigit());
    if hex && w.chars().any(|c| c.is_ascii_digit()) {
        return "<hex>".into();
    }
    w.into()
}

/// A short stable hash (FNV-1a) of where trouble was seen and what it is.
fn fingerprint(source: &str, summary: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in source.bytes().chain([0]).chain(summary.bytes()) {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("qd-{:012x}", h & 0xffff_ffff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_hides_what_varies() {
        assert_eq!(
            mask("refusing to merge https://github.com/o/r/pull/35 for tsk_01a1 after 10s"),
            "refusing to merge https://github.com/o/r/pull/N for tsk_<id> after Ns"
        );
        assert_eq!(
            mask("generation f461249e7b2d4c43 idle 901"),
            "generation <hex> idle N"
        );
        assert_eq!(mask("task \"m9 public site\" stuck"), "task \"…\" stuck");
        assert_eq!(
            fingerprint("log", &mask("x 1")),
            fingerprint("log", &mask("x 2"))
        );
    }

    #[test]
    fn findings_group_and_sort() {
        let mut f = Findings::default();
        f.add(
            Severity::Warn,
            "log",
            "a 1",
            "2026-01-02",
            Some("p"),
            "s1".into(),
        );
        f.add(
            Severity::Error,
            "log",
            "a 2",
            "2026-01-01",
            Some("p"),
            "s2".into(),
        );
        f.add(Severity::Warn, "log", "b", "2026-01-03", None, "s3".into());
        let v = f.into_sorted();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].summary, "a N");
        assert_eq!(v[0].count, 2);
        assert_eq!(v[0].severity, Severity::Error);
        assert_eq!(v[0].first_seen, "2026-01-01");
        assert_eq!(v[0].last_seen, "2026-01-02");
        assert_eq!(v[0].projects, ["p"]);
    }

    #[test]
    fn reads_a_home() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let store = crate::store::Store::open(&home.join("quark.db")).unwrap();
        store
            .record_adapter_call(None, "snapshot", false, 1, Some("banner\nerror: gone 12"))
            .unwrap();
        store
            .record_adapter_call(None, "snapshot", false, 1, Some("error: gone 13"))
            .unwrap();
        store
            .record_adapter_call(None, "snapshot", true, 1, None)
            .unwrap();
        drop(store);
        std::fs::create_dir(home.join("logs")).unwrap();
        std::fs::write(
            home.join("logs/quarkd.2026-10-26.jsonl"),
            concat!(
                r#"{"timestamp":"2099-01-01T00:00:00Z","level":"INFO","fields":{"message":"hi"},"target":"quarkd"}"#,
                "\n",
                r#"{"timestamp":"2099-01-01T00:00:01Z","level":"WARN","fields":{"message":"engine read failed","operation":"status","project":"prj_1"},"target":"quarkd::projector"}"#,
                "\nnot json\n",
            ),
        )
        .unwrap();

        let now = OffsetDateTime::now_utc();
        let d = diagnose(home, now - DEFAULT_WINDOW, now).unwrap();
        assert_eq!(d.quark_db_schema, None, "this quarkd migrated it");
        assert_eq!(d.last_log_at.as_deref(), Some("2099-01-01T00:00:01Z"));
        let summaries: Vec<(&str, u64)> = d
            .findings
            .iter()
            .map(|f| (f.summary.as_str(), f.count))
            .collect();
        assert_eq!(
            summaries,
            [
                ("snapshot failed: error: gone N", 2),
                ("quarkd::projector: engine read failed (status)", 1),
            ]
        );
        assert_eq!(d.findings[1].projects, ["prj_1"]);
    }

    /// A home with a migrated `quark.db`, then changed by `sql` the way
    /// another quarkd would leave it.
    fn home_after(sql: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("quark.db");
        drop(crate::store::Store::open(&db).unwrap());
        Connection::open(&db).unwrap().execute_batch(sql).unwrap();
        dir
    }

    fn report(home: &Path) -> Result<Diagnostics, Unreadable> {
        let now = OffsetDateTime::now_utc();
        diagnose(home, now - DEFAULT_WINDOW, now)
    }

    /// quark-5li: a daily trigger ran `quarkd triage` from a build older
    /// than the daemon, which filed "quark.db was last opened by a newer
    /// quarkd than this one" against a healthy home every day.
    #[test]
    fn a_quarkd_too_old_for_the_home_refuses_to_report() {
        let ours = crate::store::SCHEMA_VERSION;
        let dir = home_after(&format!(
            "PRAGMA user_version = {}; UPDATE schema_compat SET min_reader = {};",
            ours + 11,
            ours + 1
        ));
        let before = std::fs::read(dir.path().join("quark.db")).unwrap();
        let err = report(dir.path()).unwrap_err().to_string();
        assert!(
            err.contains("quark.db")
                && err.contains(&format!("schema {}", ours + 11))
                && err.contains("too old"),
            "{err}"
        );
        assert_eq!(
            std::fs::read(dir.path().join("quark.db")).unwrap(),
            before,
            "a reader leaves the database as it found it"
        );

        // The same for a newer event log, and a newer home format.
        let dir = tempfile::tempdir().unwrap();
        let events = dir.path().join(quark_eventlog::FILE_NAME);
        drop(quark_eventlog::SqliteEventLog::open(&events).unwrap());
        Connection::open(&events)
            .unwrap()
            .execute_batch("PRAGMA user_version = 99; UPDATE schema_compat SET min_reader = 99;")
            .unwrap();
        let err = report(dir.path()).unwrap_err().to_string();
        assert!(
            err.contains("events.db") && err.contains("too old"),
            "{err}"
        );

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(crate::home_format::FILE),
            r#"{"format": 99, "written_by": "9.0.0"}"#,
        )
        .unwrap();
        let err = report(dir.path()).unwrap_err().to_string();
        assert!(err.contains("format 99"), "{err}");
    }

    #[test]
    fn another_quarkds_usable_schema_is_a_fact_not_a_finding() {
        let ours = crate::store::SCHEMA_VERSION;
        // A newer quarkd added only what this one ignores (a rollback, or an
        // older build asked to diagnose).
        let dir = home_after(&format!("PRAGMA user_version = {}", ours + 2));
        let d = report(dir.path()).unwrap();
        assert_eq!(d.quark_db_schema, Some(ours + 2));
        assert!(d.findings.is_empty(), "{:?}", d.findings);
        assert!(render(&d).contains(&format!("quark.db is at schema {}", ours + 2)));
        // An older quarkd runs the home, and what this one reads is there.
        let dir = home_after(&format!("PRAGMA user_version = {}", ours - 1));
        let d = report(dir.path()).unwrap();
        assert_eq!(d.quark_db_schema, Some(ours - 1));
        assert!(d.findings.is_empty(), "{:?}", d.findings);
    }

    #[test]
    fn an_older_database_this_quarkd_cannot_read_is_refused_not_filed() {
        let ours = crate::store::SCHEMA_VERSION;
        let dir = home_after(&format!(
            "PRAGMA user_version = {}; ALTER TABLE decisions RENAME TO decisions_then;",
            ours - 1
        ));
        let err = report(dir.path()).unwrap_err().to_string();
        assert!(err.contains("older than quarkd"), "{err}");
        // At this quarkd's own schema, the same failure is trouble in the home.
        let dir = home_after("ALTER TABLE decisions RENAME TO decisions_then;");
        let d = report(dir.path()).unwrap();
        assert_eq!(d.findings.len(), 1, "{:?}", d.findings);
        assert!(d.findings[0].summary.starts_with("could not read quark.db"));
    }
}
