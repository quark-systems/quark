//! Quota readings for providers `quota-axi` does not report: IBM Bob
//! ([`bob`]) and Kiro CLI ([`kiro`]), ported from firstmate's
//! `fm-bob-quota.sh` and `fm-kiro-quota.sh`.
//!
//! Each reading is one schema-5 snapshot holding just its provider, the
//! shape `quota-axi --json` prints, so [`crate::QuotaAxi`] merges it like any
//! other provider and dispatch ranks it the same way. A reading never fails:
//! anything it cannot read leaves the provider `unknown` with the reason in
//! a disclosure object beside the row.

pub mod bob;
pub mod kiro;

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

pub use bob::BobUsage;
pub use kiro::KiroUsage;

/// One provider's local quota reading.
#[async_trait]
pub trait LocalReading: Send + Sync {
    /// The provider id the reading reports, such as `bob`.
    fn provider(&self) -> &str;
    /// A schema-5 snapshot holding only [`Self::provider`].
    async fn read(&self) -> Value;
    /// The same reading for another account that signs in with `key`, or
    /// `None` when the provider has no per-key reading.
    async fn read_with_key(&self, _key: &str) -> Option<Value> {
        None
    }
}

/// The longest quota-axi lets a window run before its reset and still
/// measures its pace.
const MIN_TIME_REMAINING_PERCENT: f64 = 0.01;
const SELECTION_CLAMP: f64 = 100.0;

/// quota-axi's per-window `spendPriority` (its `computeWindowPace` and
/// `windowSelectionGap`, one window per scope), so a local reading ranks
/// against quota-axi's own rows:
///
/// `gap = percentRemaining / timeRemainingPercent - percentUsed / elapsedPercent`
///
/// clamped to ±100 and rounded to four places. Positive means the allowance
/// reaches its reset unused, negative that use runs ahead of the clock
/// (where overage lands). A window too close to its reset, or with use but
/// no elapsed time, is unmeasurable.
pub fn spend_priority(
    percent_remaining: f64,
    percent_used: f64,
    start: f64,
    end: f64,
    now: f64,
) -> Option<f64> {
    let cycle = end - start;
    if cycle <= 0.0 {
        return None;
    }
    let time_remaining = round4(100.0 * (end - now) / cycle);
    let elapsed = 100.0 * (now - start) / cycle;
    if time_remaining < MIN_TIME_REMAINING_PERCENT {
        return None;
    }
    let burn = if elapsed > 0.0 {
        round4(percent_used / elapsed)
    } else if percent_used == 0.0 {
        0.0
    } else {
        return None;
    };
    let gap = (percent_remaining / time_remaining - burn).clamp(-SELECTION_CLAMP, SELECTION_CLAMP);
    Some(round4(gap))
}

fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

/// `v` rounded to `places`, as a JSON number that prints `78` for `78.0`.
fn tidy(v: f64, places: i32) -> Value {
    let p = 10f64.powi(places);
    let v = (v * p).round() / p;
    if v.fract() == 0.0 && v.abs() < 1e15 {
        json!(v as i64)
    } else {
        json!(v)
    }
}

fn utc_iso(t: OffsetDateTime) -> String {
    t.to_offset(time::UtcOffset::UTC)
        .replace_nanosecond(0)
        .unwrap_or(t)
        .format(&Rfc3339)
        .unwrap_or_default()
}

fn epoch(t: OffsetDateTime) -> f64 {
    t.unix_timestamp_nanos() as f64 / 1e9
}

/// The selection object of a known row.
fn selection(sp: Option<f64>) -> Value {
    match sp {
        Some(sp) => json!({"status": "known", "spendPriority": tidy(sp, 4)}),
        None => json!({"status": "unknown"}),
    }
}

/// A schema-5 snapshot of one provider. `status` is the semantics status;
/// `disclosure` sits beside the row under the provider's own name.
fn snapshot(
    provider: &str,
    source: &str,
    now: OffsetDateTime,
    status: &str,
    rows: Vec<Value>,
    disclosure: Value,
) -> Value {
    let mut p = json!({
        "provider": provider,
        "state": {"status": if status == "unknown" { "unknown" } else { "fresh" }},
        "quotaSemantics": {"status": status, "effectiveAvailability": rows},
    });
    p[provider] = disclosure;
    json!({
        "schemaVersion": 5,
        "generatedAt": utc_iso(now),
        "source": source,
        "providers": [p],
    })
}

/// The first number in `s` (digits with `,` separators and one `.`), and
/// the rest of `s` after it.
fn leading_number(s: &str) -> Option<(f64, &str)> {
    let s = s.trim_start();
    let end = s
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_digit() || *c == ',' || *c == '.'))
        .map_or(s.len(), |(i, _)| i);
    let digits: String = s[..end].chars().filter(|c| *c != ',').collect();
    if !s.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().map(|n| (n, &s[end..]))
}

/// Text with ANSI CSI escapes removed.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Run `argv` in `cwd` with stdin closed, bounded by `timeout`; stdout and
/// stderr together, or why it did not run.
async fn run(
    argv: &[String],
    cwd: Option<&std::path::Path>,
    timeout: Duration,
) -> Result<(bool, String), String> {
    let (exe, args) = argv.split_first().ok_or("empty command")?;
    let mut cmd = tokio::process::Command::new(exe);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    let out = tokio::time::timeout(timeout, cmd.output())
        .await
        .map_err(|_| format!("{exe} did not answer within {}s", timeout.as_secs()))?
        .map_err(|e| format!("{exe}: {e}"))?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok((out.status.success(), text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spend_priority_matches_quota_axi() {
        // Half the month gone, a quarter used: on track to finish unused.
        assert_eq!(spend_priority(75.0, 25.0, 0.0, 100.0, 50.0), Some(1.0));
        // Use ahead of the clock goes negative.
        assert_eq!(spend_priority(0.0, 100.0, 0.0, 100.0, 50.0), Some(-2.0));
        assert_eq!(spend_priority(100.0, 0.0, 0.0, 100.0, 0.0), Some(1.0));
        assert_eq!(spend_priority(90.0, 10.0, 0.0, 100.0, 0.0), None);
        assert_eq!(spend_priority(50.0, 50.0, 0.0, 100.0, 100.0), None);
        assert_eq!(spend_priority(50.0, 50.0, 10.0, 10.0, 5.0), None);
    }

    #[test]
    fn parses_numbers_and_strips_escapes() {
        assert_eq!(leading_number(" 1,234.5 of"), Some((1234.5, " of")));
        assert_eq!(leading_number("x1"), None);
        assert_eq!(strip_ansi("\x1b[1mCredits\x1b[0m (1"), "Credits (1");
        assert_eq!(tidy(78.0, 2), json!(78));
        assert_eq!(tidy(0.123456, 4), json!(0.1235));
    }
}
