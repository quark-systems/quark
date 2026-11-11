//! Kimi Code's plan usage, read the way quota-axi read it, without it.
//!
//! The login is the Kimi Code CLI's `credentials/kimi-code.json` in
//! `KIMI_CODE_HOME` (else `~/.kimi-code`); for the default login, a machine
//! with no Kimi Code sign-in but Pi signed in to Kimi (`kimi-coding` in
//! Pi's `auth.json`) is read with Pi's token instead. The token goes to Kimi
//! Code's `/usages` endpoint, which answers a five-hour, a weekly and a
//! monthly-total window (and, on older accounts, a principal weekly figure
//! with `limits[]`). Tokens are only read, never refreshed: the CLI renews
//! its own.
//!
//! The five-hour, weekly and monthly-total windows jointly bound
//! `all_models`. Where both shapes report a window, the more-used figure
//! wins, which never overstates headroom.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Map, Value};
use time::OffsetDateTime;

use super::support::{self, num, Cache, Http, PiLogin, Request};
use super::windows::{failure, reading, Window};
use super::{parse_time, LocalReading};

pub const PROVIDER: &str = "kimi";
const SOURCE: &str = "quarkd: Kimi Code usage";
const URL: &str = "https://api.kimi.com/coding/v1/usages";
const PI_ENTRY: &str = "kimi-coding";
const FIVE_HOURS: f64 = 18_000.0;
const SEVEN_DAYS: f64 = 604_800.0;
const BOUNDS: &[&str] = &["five_hour", "weekly", "month_total"];

/// Reads Kimi Code's plan usage.
#[derive(Debug, Clone)]
pub struct KimiUsage {
    pub user_home: Option<PathBuf>,
    /// `KIMI_CODE_HOME` in quarkd's own environment: the default login.
    pub default_dir: Option<PathBuf>,
    pub pi_dir: Option<PathBuf>,
    pub http: Http,
    pub url: String,
    pub cache: Cache,
}

impl KimiUsage {
    pub fn from_env() -> Self {
        let mut r = Self::new(
            support::user_home(),
            support::env("KIMI_CODE_HOME").and_then(|d| support::expand_home(&d)),
        );
        r.pi_dir = support::pi_dir();
        r
    }

    pub fn new(user_home: Option<PathBuf>, default_dir: Option<PathBuf>) -> Self {
        Self {
            user_home,
            default_dir,
            pi_dir: None,
            http: Http::default(),
            url: URL.into(),
            cache: Cache::default(),
        }
    }

    /// The reading for the login in `dir` (`None`: the default login, which
    /// may also be read through Pi's).
    pub async fn reading_for(&self, dir: Option<&Path>) -> Value {
        let home = dir.map(Path::to_path_buf).or_else(|| {
            self.default_dir
                .clone()
                .or_else(|| self.user_home.as_ref().map(|h| h.join(".kimi-code")))
        });
        let key = format!(
            "kimi\n{}",
            home.as_deref().unwrap_or(Path::new("")).display()
        );
        if let Some(v) = self.cache.get(&key) {
            return v;
        }
        let v = self.read_now(home.as_deref(), dir.is_none()).await;
        self.cache.put(&key, &v);
        v
    }

    async fn read_now(&self, home: Option<&Path>, pi_fallback: bool) -> Value {
        let now = OffsetDateTime::now_utc();
        let fail = |status: &str, error: &str| failure(PROVIDER, SOURCE, now, status, error, None);
        let file = home.map(|h| h.join("credentials").join("kimi-code.json"));
        let login = match file.as_deref().map(support::read_json) {
            Some(Ok(Some(v))) => match support::text(v.get("access_token")) {
                Some(t) => {
                    let expires = num(v.get("expires_at"));
                    let expired = expires.is_some_and(|e| e <= now.unix_timestamp() as f64);
                    Some((t, expired))
                }
                None => return fail("unavailable", "Kimi Code's credentials hold no token"),
            },
            Some(Err(e)) => return fail("unavailable", &e),
            _ => None,
        };
        let login = login.or_else(|| match pi_fallback {
            true => match support::pi_login(self.pi_dir.as_deref(), PI_ENTRY)? {
                PiLogin::OAuth {
                    access, expired, ..
                } => Some((access, expired)),
                PiLogin::Key(k) => Some((k, false)),
            },
            false => None,
        });
        let Some((token, expired)) = login else {
            return fail(
                "auth_required",
                "Kimi Code is not signed in here; run `kimi` and sign in",
            );
        };
        let req = Request::get(&self.url)
            .bearer(&token)
            .header("Accept", "application/json")
            .header("User-Agent", "quarkd");
        match self.http.send(&req).await {
            Ok(r) if r.ok() => match r.json().and_then(|v| usage_reading(&v, now)) {
                Some(v) => v,
                None => fail("unavailable", "Kimi Code reported no quota windows"),
            },
            Ok(r) if expired && matches!(r.status, 401 | 403) => fail(
                "auth_required",
                "Kimi Code's sign-in expired; it renews the next time `kimi` runs",
            ),
            Ok(r) => {
                let (s, e) = support::http_failure("Kimi Code", r.status);
                fail(s, &e)
            }
            Err(e) => fail("unavailable", &format!("Kimi Code quota unavailable: {e}")),
        }
    }
}

/// A window's used percent and reset: `used_ratio`, or `used`/`remaining`
/// of `limit`.
fn detail(v: &Value) -> Option<(f64, Option<OffsetDateTime>)> {
    let used = match num(v.get("used_ratio")) {
        Some(r) if r >= 0.0 => r * 100.0,
        Some(_) => return None,
        None => {
            let limit = num(v.get("limit")).filter(|l| *l > 0.0)?;
            let used = num(v.get("used"))
                .filter(|u| *u >= 0.0)
                .or_else(|| num(v.get("remaining")).map(|r| (limit - r).max(0.0)))?;
            used / limit * 100.0
        }
    };
    let reset = ["resetTime", "resetAt", "reset_time", "reset_at"]
        .iter()
        .find_map(|k| v.get(*k).and_then(parse_time));
    Some(((used * 1e10).round() / 1e10, reset))
}

fn window_seconds(w: &Value) -> Option<f64> {
    let d = num(w.get("duration")).filter(|d| *d > 0.0)?;
    let mult = match w.get("timeUnit")?.as_str()? {
        "TIME_UNIT_SECOND" | "SECOND" | "SECONDS" => 1.0,
        "TIME_UNIT_MINUTE" | "MINUTE" | "MINUTES" => 60.0,
        "TIME_UNIT_HOUR" | "HOUR" | "HOURS" => 3_600.0,
        "TIME_UNIT_DAY" | "DAY" | "DAYS" => 86_400.0,
        _ => return None,
    };
    Some(d * mult)
}

/// The reading for a `/usages` answer.
fn usage_reading(v: &Value, now: OffsetDateTime) -> Option<Value> {
    let mut windows: Vec<Window> = Vec::new();
    if let Some(u) = v.get("usages").filter(|u| u.is_object()) {
        for (key, id, label, kind, secs) in [
            (
                "limit_5h",
                "five_hour",
                "session",
                "session",
                Some(FIVE_HOURS),
            ),
            ("limit_7d", "weekly", "week", "weekly", Some(SEVEN_DAYS)),
            ("limit_month_total", "month_total", "month", "monthly", None),
        ] {
            if let Some((used, reset)) = u.get(key).and_then(detail) {
                let secs = secs.or_else(|| month_before(reset));
                windows.push(
                    Window::new(id, label, kind, used)
                        .resets(reset)
                        .lasting(secs),
                );
            }
        }
    }
    let mut legacy = Vec::new();
    if let Some((used, reset)) = v.get("usage").and_then(detail) {
        legacy.push(
            Window::new("weekly", "week", "weekly", used)
                .resets(reset)
                .lasting(Some(SEVEN_DAYS)),
        );
        let mut five_seen = false;
        for (i, e) in v
            .get("limits")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let Some((used, reset)) = e.get("detail").and_then(detail) else {
                continue;
            };
            let secs = e.get("window").and_then(window_seconds);
            let five = secs == Some(FIVE_HOURS) && !five_seen;
            five_seen |= five;
            let (id, label, kind) = if five {
                ("five_hour".to_string(), "session".to_string(), "session")
            } else {
                (
                    format!("limit:{}", i + 1),
                    format!("limit {}", i + 1),
                    "unknown",
                )
            };
            legacy.push(
                Window::new(&id, &label, kind, used)
                    .resets(reset)
                    .lasting(secs),
            );
        }
    }
    for l in legacy {
        match windows.iter_mut().find(|w| w.id == l.id) {
            Some(w) if l.percent_used > w.percent_used => {
                let reset = l.resets_at.or(w.resets_at);
                *w = l.resets(reset);
            }
            Some(w) => {
                if w.resets_at.is_none() {
                    w.resets_at = l.resets_at;
                }
            }
            None => windows.push(l),
        }
    }
    if windows.is_empty() {
        return None;
    }
    let bounds: Vec<&Window> = windows
        .iter()
        .filter(|w| BOUNDS.contains(&w.id.as_str()))
        .collect();
    Some(reading(
        PROVIDER,
        SOURCE,
        now,
        &windows,
        &[("all_models".to_string(), bounds)],
        Map::new(),
    ))
}

/// The length of the calendar month that ends at `reset`.
fn month_before(reset: Option<OffsetDateTime>) -> Option<f64> {
    let r = reset?;
    let (y, m) = match r.month() {
        time::Month::January => (r.year() - 1, time::Month::December),
        m => (r.year(), m.previous()),
    };
    let day = r.day().min(m.length(y));
    let start = r.replace_date(time::Date::from_calendar_date(y, m, day).ok()?);
    Some((r - start).as_seconds_f64())
}

#[async_trait]
impl LocalReading for KimiUsage {
    fn provider(&self) -> &str {
        PROVIDER
    }

    async fn read(&self) -> Value {
        self.reading_for(None).await
    }

    async fn read_account(&self, config_dir: Option<&Path>, _grant: bool) -> Option<Value> {
        Some(self.reading_for(config_dir).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use time::Duration;

    #[test]
    fn takes_the_more_used_figure() {
        let now = OffsetDateTime::now_utc();
        let reset = |d: Duration| super::super::utc_iso(now + d);
        let v = json!({
            "usage": {"limit": "100", "used": "92", "resetTime": reset(Duration::days(3))},
            "limits": [{"window": {"duration": 300, "timeUnit": "TIME_UNIT_MINUTE"},
                        "detail": {"limit": 100, "remaining": 90}}],
            "usages": {
                "limit_5h": {"used_ratio": 0.2, "reset_at": reset(Duration::hours(2))},
                "limit_7d": {"used_ratio": 0, "reset_at": reset(Duration::days(3))},
                "limit_month_total": {"used_ratio": 0.5, "reset_at": reset(Duration::days(10))},
            },
        });
        let r = usage_reading(&v, now).unwrap();
        assert!(crate::quota::valid(&r), "{r}");
        let p = &r["providers"][0];
        let ids: Vec<&str> = p["windows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["five_hour", "weekly", "month_total"]);
        assert_eq!(p["windows"][0]["percentUsed"], json!(20));
        assert_eq!(p["windows"][1]["percentUsed"], json!(92));
        let row = &p["quotaSemantics"]["effectiveAvailability"][0];
        assert_eq!(row["scope"], "all_models");
        assert_eq!(row["effectivePercentRemaining"], json!(8));
        assert!(usage_reading(&json!({}), now).is_none());
    }

    #[tokio::test]
    async fn reads_cli_then_pi_logins() {
        let tmp = tempfile::tempdir().unwrap();
        let body = br#"{"usages":{"limit_5h":{"used_ratio":0.1},"limit_7d":{"used_ratio":0.3}}}"#;
        let curl = support::stub_curl(tmp.path(), &[("kimi.com", 200, body)]);
        let home = tmp.path().join("kimi");
        let pi = tmp.path().join("pi");
        std::fs::create_dir_all(&pi).unwrap();
        std::fs::write(
            pi.join("auth.json"),
            r#"{"kimi-coding":{"type":"oauth","access":"a"}}"#,
        )
        .unwrap();
        let mut r = KimiUsage::new(None, Some(home.clone()));
        r.http.curl = curl;
        r.cache = Cache::none();
        let v = r.read().await;
        assert_eq!(v["providers"][0]["state"]["status"], "auth_required");
        r.pi_dir = Some(pi);
        let v = r.read().await;
        assert_eq!(v["providers"][0]["state"]["status"], "fresh", "{v}");
        std::fs::create_dir_all(home.join("credentials")).unwrap();
        std::fs::write(
            home.join("credentials/kimi-code.json"),
            r#"{"access_token":"t","expires_at":1}"#,
        )
        .unwrap();
        let v = r.reading_for(Some(&home)).await;
        assert_eq!(v["providers"][0]["state"]["status"], "fresh", "{v}");
    }
}
