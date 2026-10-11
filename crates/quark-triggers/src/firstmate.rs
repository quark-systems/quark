//! Today's bash engine, as far as slice 7 is concerned.
//!
//! - [`read_home`] reads a firstmate home's inbox notes
//!   (`state/inbox/*.note`, handled ones under `handled/`) and its away
//!   posture (`state/.afk`, `state/.afk-contract`), so the engine can
//!   mirror them into the log while firstmate still owns them.
//! - [`bash_escalates`] is firstmate's away classifier for a status line
//!   (`status_is_captain_relevant` in `bin/fm-classify-lib.sh`), the
//!   decision shadow mode compares the native route against.
//! - [`read_watches`] reads its when-then watches (`fm-procevent-when.sh`:
//!   specs under `state/when/`, outcomes captured under
//!   `state/procevent-inbox/`), [`WhenSpec::rule`] turns one into the
//!   equivalent native [`Rule`], and [`compare_watch`] judges whether the
//!   native rule and firstmate's watch ended the same way.

use std::path::Path;

use quark_core::{CoreError, Result};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::away::{Occasion, Posture};
use crate::channel::{Inbound, INBOX};
use crate::trigger::{Action, Condition, Rule};

/// The event kind the slice 1 bridge (`quark_eventlog::firstmate`) gives
/// each status line. Its payload carries at least `verb` and `raw`.
pub const STATUS_KIND: &str = "firstmate.status";

/// The fields of a `firstmate.status` payload routing needs.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct StatusLine {
    pub verb: String,
    #[serde(default)]
    pub note: String,
    pub raw: String,
}

/// Free-text phrases firstmate treats as captain-relevant on a line whose
/// verb it does not know (`FM_CLASSIFY_CAPTAIN_RE_DEFAULT`, matched
/// anywhere, ignoring case).
const LEGACY_PHRASES: [&str; 8] = [
    "done:",
    "needs-decision:",
    "blocked:",
    "failed:",
    "pr ready",
    "checks green",
    "ready in branch",
    "merged",
];

/// Whether firstmate's away daemon escalates this status line to the
/// coordinator. A port of `status_is_captain_relevant` with the default
/// pattern.
pub fn bash_escalates(verb: &str, raw: &str) -> bool {
    match verb {
        "working" | "resolved" | "captain-held" | "paused" => false,
        "done" | "needs-decision" | "blocked" | "failed" => true,
        _ => {
            let lower = raw.to_lowercase();
            LEGACY_PHRASES.iter().any(|p| lower.contains(p))
        }
    }
}

/// The occasion a firstmate status line stands for.
pub fn status_occasion(verb: &str, raw: &str) -> Occasion {
    match verb {
        "done" => Occasion::Done,
        "needs-decision" => Occasion::Decision,
        "blocked" => Occasion::Blocked,
        "failed" => Occasion::Failed,
        "paused" => Occasion::Paused,
        "working" | "resolved" | "captain-held" => Occasion::Progress,
        // firstmate's legacy free-text lines (`PR ready ...`) announce a
        // finished deliverable; anything else is progress.
        _ if bash_escalates(verb, raw) => Occasion::Done,
        _ => Occasion::Progress,
    }
}

/// What one firstmate home holds for slice 7.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Home {
    /// Notes still waiting in `state/inbox/`.
    pub pending: Vec<Inbound>,
    /// Notes moved to `state/inbox/handled/`.
    pub handled: Vec<Inbound>,
    pub posture: Posture,
    /// `expected_return` from the away record, when it has one.
    pub expected_return: Option<OffsetDateTime>,
    /// Its when-then watches, armed or with a captured outcome.
    pub watches: Vec<Watch>,
}

fn io(path: &Path, e: impl std::fmt::Display) -> CoreError {
    CoreError::Backend(format!("{}: {e}", path.display()))
}

/// Read the inbox and away posture of the firstmate home at `home`.
pub fn read_home(home: &Path) -> Result<Home> {
    let state = home.join("state");
    let inbox = state.join("inbox");
    let mut out = Home {
        pending: read_notes(&inbox)?,
        handled: read_notes(&inbox.join("handled"))?,
        watches: read_watches(home)?,
        ..Home::default()
    };
    // A note in both places is mid-ack; it counts as handled.
    out.pending
        .retain(|n| !out.handled.iter().any(|h| h.id == n.id));
    let afk = state.join(".afk");
    match std::fs::read_to_string(&afk) {
        Ok(s) => {
            out.posture = match s.lines().next().map(str::trim) {
                Some("quiet") => Posture::Quiet,
                // An empty flag file is away, as fm_afk_mode reads it.
                _ => Posture::Away,
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // The away record alone is the posture on harnesses without
            // the daemon.
            if state.join(".afk-contract").exists() {
                out.posture = Posture::Away;
            }
        }
        Err(e) => return Err(io(&afk, e)),
    }
    if let Ok(s) = std::fs::read_to_string(state.join(".afk-contract")) {
        out.expected_return = s
            .lines()
            .find_map(|l| l.strip_prefix("expected_return:"))
            .and_then(|v| OffsetDateTime::parse(v.trim(), &Rfc3339).ok());
    }
    Ok(out)
}

fn read_notes(dir: &Path) -> Result<Vec<Inbound>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io(dir, e)),
    };
    let mut notes = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| io(dir, e))?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("note") {
            continue;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            // Acked between the listing and the read.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(io(&path, e)),
        };
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        notes.push(parse_note(stem, &text));
    }
    notes.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
    Ok(notes)
}

/// Parse one `fm-inbox.sh` note: `key=value` header lines, `--`, then the
/// body. The file name is the id when the header lacks one.
pub fn parse_note(file_id: &str, text: &str) -> Inbound {
    let (head, body) = match text.split_once("\n--\n") {
        Some((h, b)) => (h, b),
        None => ("", text),
    };
    let field = |k: &str| {
        head.lines()
            .find_map(|l| l.strip_prefix(k).and_then(|r| r.strip_prefix('=')))
            .map(str::trim)
    };
    let at = field("at")
        .and_then(|v| OffsetDateTime::parse(v, &Rfc3339).ok())
        .unwrap_or(OffsetDateTime::UNIX_EPOCH);
    Inbound {
        id: field("id")
            .filter(|v| !v.is_empty())
            .unwrap_or(file_id)
            .to_string(),
        channel: INBOX.to_string(),
        from: field("source").unwrap_or("user").to_string(),
        body: body.strip_suffix('\n').unwrap_or(body).to_string(),
        at,
        task: None,
    }
}

/// The source-id prefix of every when-then watch.
pub const WHEN_PREFIX: &str = "when-";

/// The description a mirrored watch's rule carries; it records when
/// firstmate armed the watch, so a re-armed watch is a new rule.
const MIRRORED: &str = "firstmate when-watch, armed ";

/// One `fm-when-spec-v1` spec: a condition polled until it holds, then an
/// action run once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhenSpec {
    pub armed: OffsetDateTime,
    /// Seconds between polls, rounded up (firstmate allows decimals).
    pub interval_secs: u64,
    pub stable: u32,
    pub deadline_secs: u64,
    pub condition_timeout_secs: u64,
    pub action_timeout_secs: u64,
    pub error_budget: u32,
    pub condition: Vec<String>,
    pub action: Vec<String>,
}

/// How a watch ended, as firstmate's outcome document says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WhenStatus {
    Fired,
    ActionFailed,
    ConditionError,
    NeverTrue,
    Ambiguous,
    Rejected,
    Unknown,
}

impl WhenStatus {
    fn parse(s: &str) -> Self {
        match s.trim() {
            "fired" => Self::Fired,
            "action-failed" => Self::ActionFailed,
            "condition-error" => Self::ConditionError,
            "never-true" => Self::NeverTrue,
            "ambiguous" => Self::Ambiguous,
            "rejected" => Self::Rejected,
            _ => Self::Unknown,
        }
    }
}

/// One captured outcome document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhenOutcome {
    pub status: WhenStatus,
    pub detail: String,
    /// When it was captured (the result file's modification time).
    pub at: OffsetDateTime,
}

/// One when-then watch: its spec while armed, and its captured outcomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watch {
    /// `when-<name>`, which is also the native rule's id.
    pub source: String,
    /// `None` once firstmate retired the watch.
    pub spec: Option<WhenSpec>,
    /// The action was claimed (`state/when/<source>.fired`).
    pub fired: bool,
    /// Oldest first.
    pub outcomes: Vec<WhenOutcome>,
}

impl WhenSpec {
    /// Parse a spec file's text, as `spec_load` in `fm-procevent-when.sh`
    /// does; `None` for anything that script would refuse.
    pub fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != "fm-when-spec-v1" {
            return None;
        }
        let mut fields = std::collections::HashMap::new();
        for line in lines.by_ref() {
            if line == "argv:" {
                break;
            }
            let (k, v) = line.split_once('=')?;
            fields.insert(k, v);
        }
        let int = |k: &str| fields.get(k)?.parse::<u64>().ok().filter(|n| *n > 0);
        let interval: f64 = fields
            .get("interval")?
            .parse()
            .ok()
            .filter(|n: &f64| *n > 0.0)?;
        let cond_argc = int("condition_argc")? as usize;
        let act_argc = int("action_argc")? as usize;
        let argv: Vec<String> = lines.map(str::to_string).collect();
        if argv.len() != cond_argc + act_argc {
            return None;
        }
        let armed = fields.get("armed")?.parse::<i64>().ok()?;
        Some(Self {
            armed: OffsetDateTime::from_unix_timestamp(armed).ok()?,
            interval_secs: (interval.ceil() as u64).max(1),
            stable: int("stable")? as u32,
            deadline_secs: int("deadline")?,
            condition_timeout_secs: int("condition_timeout")?,
            action_timeout_secs: int("action_timeout")?,
            error_budget: int("error_budget")? as u32,
            condition: argv[..cond_argc].to_vec(),
            action: argv[cond_argc..].to_vec(),
        })
    }

    /// The native rule that does what this watch does.
    pub fn rule(&self, source: &str) -> Rule {
        let mut rule = Rule::new(
            source,
            Condition::Command {
                argv: self.condition.clone(),
                interval_secs: self.interval_secs,
                stable: self.stable,
                timeout_secs: self.condition_timeout_secs,
                expect: None,
                error_budget: self.error_budget,
                until: Some(self.armed + time::Duration::seconds(self.deadline_secs as i64)),
            },
            Action::Command {
                argv: self.action.clone(),
                timeout_secs: self.action_timeout_secs,
            },
        );
        rule.description = format!("{MIRRORED}{}", self.armed.unix_timestamp());
        rule
    }

    /// How long after one side ends the other may take before the two
    /// disagree: two full stable runs, a slow poll and five minutes for
    /// firstmate's runner to start on the watcher's next cycle.
    pub fn grace(&self) -> time::Duration {
        grace(self.interval_secs, self.stable, self.condition_timeout_secs)
    }
}

fn grace(interval_secs: u64, stable: u32, timeout_secs: u64) -> time::Duration {
    time::Duration::seconds((2 * interval_secs * u64::from(stable) + timeout_secs + 300) as i64)
}

/// When firstmate armed the watch a rule mirrors, and how long the two
/// sides may end apart ([`WhenSpec::grace`]), or `None` for a rule that is
/// not a mirrored watch.
pub fn mirrored(rule: &Rule) -> Option<(OffsetDateTime, time::Duration)> {
    let armed = rule.description.strip_prefix(MIRRORED)?.parse().ok()?;
    let armed = OffsetDateTime::from_unix_timestamp(armed).ok()?;
    let Condition::Command {
        interval_secs,
        stable,
        timeout_secs,
        ..
    } = &rule.when
    else {
        return None;
    };
    Some((armed, grace(*interval_secs, *stable, *timeout_secs)))
}

/// Read every when-then watch of the firstmate home at `home`.
pub fn read_watches(home: &Path) -> Result<Vec<Watch>> {
    let state = home.join("state");
    let mut watches: std::collections::BTreeMap<String, Watch> = Default::default();
    let entry = |w: &mut std::collections::BTreeMap<String, Watch>, source: &str| {
        w.entry(source.to_string())
            .or_insert_with(|| Watch {
                source: source.to_string(),
                spec: None,
                fired: false,
                outcomes: Vec::new(),
            })
            .source
            .clone()
    };
    let when = state.join("when");
    for path in list(&when)? {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Some(source) = name
            .strip_suffix(".spec")
            .filter(|s| s.starts_with(WHEN_PREFIX))
        {
            let Some(spec) = read_opt(&path)?.and_then(|t| WhenSpec::parse(&t)) else {
                continue;
            };
            let k = entry(&mut watches, source);
            watches.get_mut(&k).unwrap().spec = Some(spec);
        } else if let Some(source) = name
            .strip_suffix(".fired")
            .filter(|s| s.starts_with(WHEN_PREFIX))
        {
            let k = entry(&mut watches, source);
            watches.get_mut(&k).unwrap().fired = true;
        }
    }
    let inbox = state.join("procevent-inbox");
    for path in list(&inbox)? {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // `<source>.<seq>.result`
        let Some(source) = name
            .strip_suffix(".result")
            .and_then(|s| s.rsplit_once('.'))
            .filter(|(src, seq)| src.starts_with(WHEN_PREFIX) && seq.parse::<u64>().is_ok())
            .map(|(src, _)| src)
        else {
            continue;
        };
        let Some(text) = read_opt(&path)? else {
            continue;
        };
        let field = |k: &str| text.lines().find_map(|l| l.strip_prefix(k));
        if field("when: ").map(str::trim) != Some(source) {
            continue;
        }
        let at = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .map(OffsetDateTime::from)
            .map_err(|e| io(&path, e))?;
        let outcome = WhenOutcome {
            status: WhenStatus::parse(field("status: ").unwrap_or_default()),
            detail: field("detail: ").unwrap_or_default().trim().to_string(),
            at,
        };
        let k = entry(&mut watches, source);
        watches.get_mut(&k).unwrap().outcomes.push(outcome);
    }
    let mut out: Vec<Watch> = watches.into_values().collect();
    for w in &mut out {
        w.outcomes.sort_by_key(|o| o.at);
    }
    Ok(out)
}

fn list(dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|e| e.map(|e| e.path()).map_err(|e| io(dir, e)))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(io(dir, e)),
    }
}

fn read_opt(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io(path, e)),
    }
}

/// How the native rule mirroring a watch ended, by fire.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NativeEnd {
    /// Its condition held (`held` fire).
    pub held: Option<OffsetDateTime>,
    /// It used up its error budget (`error` fire).
    pub error: Option<OffsetDateTime>,
    /// It reached its `until` (`deadline` fire).
    pub deadline: Option<OffsetDateTime>,
}

impl NativeEnd {
    fn first(&self) -> Option<(&'static str, OffsetDateTime)> {
        [
            ("fired", self.held),
            ("condition-error", self.error),
            ("never-true", self.deadline),
        ]
        .into_iter()
        .filter_map(|(k, t)| t.map(|t| (k, t)))
        .min_by_key(|(_, t)| *t)
    }
}

/// What a comparison of one watch found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Neither side has ended, or the other side is still within its grace.
    Pending,
    /// Not comparable: firstmate refused the spec or lost the outcome.
    Skip,
    Agree,
    /// Each side's end, as a word: `fired`, `condition-error`,
    /// `never-true`, or `none` when that side has not ended in time.
    Diverge {
        bash: String,
        native: String,
    },
}

/// Compare firstmate's watch, armed at `armed`, with how its native rule
/// ended, at `now`; either side may end up to `grace` after the other.
/// Only an outcome captured after `armed` counts, so a re-armed watch is
/// compared afresh.
pub fn compare_watch(
    watch: &Watch,
    armed: OffsetDateTime,
    grace: time::Duration,
    native: NativeEnd,
    now: OffsetDateTime,
) -> Verdict {
    let outcome = watch.outcomes.iter().rev().find(|o| o.at >= armed);
    let native_end = native.first();
    let Some(outcome) = outcome else {
        // Firstmate has not ended. Native ending is a disagreement once
        // firstmate has had its grace, unless firstmate claimed the fire
        // and is still running the action.
        return match native_end {
            Some((n, t)) if now >= t + grace && !watch.fired => Verdict::Diverge {
                bash: "none".into(),
                native: n.into(),
            },
            _ => Verdict::Pending,
        };
    };
    let bash = match outcome.status {
        WhenStatus::Fired | WhenStatus::ActionFailed => "fired",
        WhenStatus::ConditionError => "condition-error",
        WhenStatus::NeverTrue => "never-true",
        WhenStatus::Ambiguous | WhenStatus::Rejected | WhenStatus::Unknown => return Verdict::Skip,
    };
    match native_end {
        Some((n, _)) if n == bash => Verdict::Agree,
        Some((n, _)) => Verdict::Diverge {
            bash: bash.into(),
            native: n.into(),
        },
        None if now >= outcome.at + grace => Verdict::Diverge {
            bash: bash.into(),
            native: "none".into(),
        },
        None => Verdict::Pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifier_matches_firstmate() {
        // (verb, line, escalates) from fm-classify-lib.sh's rules.
        let cases = [
            (
                "working",
                "working: merged the helper, PR ready soon",
                false,
            ),
            ("resolved", "resolved: key=a answered", false),
            ("paused", "paused: waiting on CI", false),
            ("captain-held", "captain-held: x", false),
            ("done", "done: PR https://x checks green", true),
            ("needs-decision", "needs-decision: key=k which?", true),
            ("blocked", "blocked: no creds", true),
            ("failed", "failed: tests", true),
            ("note", "note: PR ready for review", true),
            ("note", "note: Merged upstream", true),
            ("note", "note: still going", false),
        ];
        for (verb, line, want) in cases {
            assert_eq!(bash_escalates(verb, line), want, "{line}");
            assert_eq!(
                status_occasion(verb, line) != Occasion::Progress
                    && status_occasion(verb, line) != Occasion::Paused,
                want,
                "{line}"
            );
        }
    }

    #[test]
    fn reads_a_home() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("state/inbox");
        std::fs::create_dir_all(inbox.join("handled")).unwrap();
        std::fs::write(
            inbox.join("1700000000-aaaa.note"),
            "id=1700000000-aaaa\nat=2026-10-07T01:00:00Z\nsource=voice\n--\nship it\nplease\n",
        )
        .unwrap();
        std::fs::write(
            inbox.join("handled/1699999999-bbbb.note"),
            "id=1699999999-bbbb\nat=2026-10-06T01:00:00Z\nsource=cli\n--\nold\n",
        )
        .unwrap();
        std::fs::write(inbox.join(".staging-zz"), "half written").unwrap();
        let h = read_home(dir.path()).unwrap();
        assert_eq!(h.pending.len(), 1);
        assert_eq!(h.pending[0].id, "1700000000-aaaa");
        assert_eq!(h.pending[0].from, "voice");
        assert_eq!(h.pending[0].body, "ship it\nplease");
        assert_eq!(h.handled.len(), 1);
        assert_eq!(h.posture, Posture::Present);

        std::fs::write(dir.path().join("state/.afk"), "quiet\n").unwrap();
        assert_eq!(read_home(dir.path()).unwrap().posture, Posture::Quiet);
        std::fs::write(dir.path().join("state/.afk"), "").unwrap();
        std::fs::write(
            dir.path().join("state/.afk-contract"),
            "version: 1\nexpected_return: 2026-10-08T09:00:00Z\n",
        )
        .unwrap();
        let h = read_home(dir.path()).unwrap();
        assert_eq!(h.posture, Posture::Away);
        assert!(h.expected_return.is_some());
    }
}
