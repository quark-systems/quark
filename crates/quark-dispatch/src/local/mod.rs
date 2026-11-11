//! quarkd's own quota readers, one per provider, each ported from the
//! readers firstmate and quota-axi used (Gemini CLI's is quarkd's own):
//!
//! | provider | reader | reads |
//! |---|---|---|
//! | `claude` | [`claude`] | Claude Code's OAuth login (Keychain or `.credentials.json`), then Anthropic's usage endpoint |
//! | `codex` | [`codex`] | Codex's `auth.json` (or Pi's `openai-codex` login), then ChatGPT's usage endpoint |
//! | `bob` | [`bob`] | IBM Bob's local ledger and live API (`fm-bob-quota.sh`) |
//! | `kiro` | [`kiro`] | `kiro-cli /usage` (`fm-kiro-quota.sh`) |
//! | `grok` | [`grok`] | the Grok CLI's `auth.json` (or Pi's `xai` login), then grok.com's credits call |
//! | `kimi` | [`kimi`] | Kimi Code's credentials (or Pi's `kimi-coding` login), then Kimi Code's `/usages` |
//! | `cursor` | [`cursor`] | Cursor's `state.vscdb` (`sqlite3`) or CLI login, then Cursor's dashboard service |
//! | `agy` | [`agy`] | `agy -p /quota --output-format json` |
//! | `muse` | [`muse`] | the Muse CLI's `auth.json`, Keychain item or `META_API_KEY`, then Meta's `/muse-code/key` |
//! | `gemini` | [`gemini`] | Gemini CLI's `oauth_creds.json`, then Code Assist's `retrieveUserQuota` |
//! | `copilot` | [`copilot`] | the Copilot CLI's, editors' or `gh`'s login, then GitHub's `copilot_internal/user` |
//!
//! Each reading is one schema-5 snapshot holding just its provider, so
//! [`crate::LocalQuota`] puts them side by side and dispatch ranks them the
//! same way. A reading never fails: anything it cannot read leaves the
//! provider `unknown` with the reason in its state or in a disclosure object
//! beside the row. [`support`] holds what the readers share.

pub mod agy;
pub mod bob;
pub mod claude;
pub mod codex;
pub mod copilot;
pub mod cursor;
pub mod gemini;
pub mod grok;
pub mod kimi;
pub mod kiro;
pub mod muse;
pub mod support;
pub mod windows;

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

pub use agy::AgyUsage;
pub use bob::BobUsage;
pub use claude::ClaudeUsage;
pub use codex::CodexUsage;
pub use copilot::CopilotUsage;
pub use cursor::CursorUsage;
pub use gemini::GeminiUsage;
pub use grok::GrokUsage;
pub use kimi::KimiUsage;
pub use kiro::KiroUsage;
pub use muse::MuseUsage;

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
    /// The reading for the account whose config directory is `config_dir`
    /// (`None` for the default login), or `None` when the provider has only
    /// its default login. `grant` lets the read wait on a macOS Keychain
    /// prompt the person answers ("Always Allow"), where a background read
    /// gives up on one.
    async fn read_account(
        &self,
        config_dir: Option<&std::path::Path>,
        _grant: bool,
    ) -> Option<Value> {
        match config_dir {
            None => Some(self.read().await),
            Some(_) => None,
        }
    }
}

/// The least of a window's time that may remain before its reset and still
/// have its pace measured (as quota-axi did).
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

/// Run `argv` in `cwd` with `env` added to the daemon's own environment and
/// stdin closed, bounded by `timeout`; stdout and stderr together, or why it
/// did not run.
async fn run_with(
    argv: &[String],
    cwd: Option<&std::path::Path>,
    env: &[(&str, &str)],
    timeout: Duration,
) -> Result<(bool, String), String> {
    let (exe, args) = argv.split_first().ok_or("empty command")?;
    let mut cmd = tokio::process::Command::new(exe);
    cmd.args(args)
        .envs(env.iter().copied())
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

/// GET `url` with curl, the bearer token in a config line on curl's stdin
/// so it never shows in the process list. The status code and body, or why
/// curl did not answer.
async fn curl_get(
    curl: &str,
    url: &str,
    bearer: &str,
    headers: &[(&str, &str)],
    timeout: Duration,
) -> Result<(u16, String), String> {
    use tokio::io::AsyncWriteExt;
    let mut cmd = tokio::process::Command::new(curl);
    cmd.args(["-sS", "--max-time"])
        .arg(timeout.as_secs().max(1).to_string())
        .args(["-w", "\n%{http_code}", "-H", "Accept: application/json"]);
    for (k, v) in headers {
        cmd.arg("-H").arg(format!("{k}: {v}"));
    }
    let mut child = cmd
        .args(["-K", "-"])
        .arg(url)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("{curl}: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("curl stdin")?;
    let token: String = bearer
        .chars()
        .filter(|c| !c.is_control() && *c != '"' && *c != '\\')
        .collect();
    stdin
        .write_all(format!("header = \"Authorization: Bearer {token}\"\n").as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    drop(stdin);
    let out = tokio::time::timeout(timeout + Duration::from_secs(5), child.wait_with_output())
        .await
        .map_err(|_| format!("no answer within {}s", timeout.as_secs()))?
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err.lines().next().unwrap_or("").trim();
        return Err(if line.is_empty() {
            format!("curl exited {}", out.status)
        } else {
            line.to_string()
        });
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, code) = text.rsplit_once('\n').unwrap_or(("", &text));
    Ok((code.trim().parse().unwrap_or(0), body.to_string()))
}

/// RFC 3339 or epoch seconds (or milliseconds, also as a numeric string),
/// as providers spell a reset.
fn parse_time(v: &Value) -> Option<OffsetDateTime> {
    match v {
        Value::String(s) => OffsetDateTime::parse(s.trim(), &Rfc3339).ok().or_else(|| {
            let n: f64 = s.trim().parse().ok()?;
            parse_time(&serde_json::json!(n))
        }),
        Value::Number(n) => {
            let secs = n.as_f64()?;
            // Some fields are milliseconds.
            let secs = if secs > 1e11 { secs / 1000.0 } else { secs };
            OffsetDateTime::from_unix_timestamp(secs as i64).ok()
        }
        _ => None,
    }
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
