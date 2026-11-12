//! Codex's plan usage, read the way quota-axi read it, without it.
//!
//! The login is Codex's `auth.json` in `CODEX_HOME` (else `~/.codex`): its
//! ChatGPT access token and account id go to ChatGPT's usage endpoint
//! (`/backend-api/wham/usage`, then `/backend-api/codex/usage`), which
//! answers the five-hour and weekly rate-limit windows, named model limits,
//! code-review limits and a workspace credit cap. For the default login,
//! a machine with no Codex sign-in but Pi signed in to Codex (`openai-codex`
//! in Pi's `auth.json`) is read with Pi's token instead. Tokens are only
//! read, never refreshed: Codex and Pi renew their own.
//!
//! The base windows bound `all_models`; a named model limit adds a bound for
//! `model:<id>`; code review is its own `code_review` scope. A credit cap
//! bounds `all_models` only when it is the only evidence.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine;
use serde_json::{json, Map, Value};
use time::OffsetDateTime;

use super::windows::{failure, reading, Window};
use super::{curl_get, parse_time};

pub const PROVIDER: &str = "codex";
const SOURCE: &str = "quarkd: Codex usage";
const USAGE_URLS: &[&str] = &[
    "https://chatgpt.com/backend-api/wham/usage",
    "https://chatgpt.com/backend-api/codex/usage",
];
const PI_ENTRY: &str = "openai-codex";
const FIVE_HOURS: f64 = 18_000.0;
const SEVEN_DAYS: f64 = 604_800.0;

/// Last reading per Codex home.
type Cache = Arc<Mutex<HashMap<PathBuf, (Instant, Value)>>>;

/// Reads Codex's plan usage for any of its logins.
#[derive(Clone)]
pub struct CodexUsage {
    pub user_home: Option<PathBuf>,
    /// `CODEX_HOME` in quarkd's own environment: the default login.
    pub default_dir: Option<PathBuf>,
    /// Pi's agent directory (`PI_CODING_AGENT_DIR`, else `~/.pi/agent`).
    pub pi_dir: Option<PathBuf>,
    pub curl: String,
    pub urls: Vec<String>,
    pub api_timeout: Duration,
    pub cache_for: Duration,
    cache: Cache,
}

impl std::fmt::Debug for CodexUsage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexUsage")
            .field("default_dir", &self.default_dir)
            .field("pi_dir", &self.pi_dir)
            .finish_non_exhaustive()
    }
}

fn shared_cache() -> Cache {
    static CACHE: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default).clone()
}

#[derive(Debug, Clone, PartialEq)]
struct Credentials {
    token: String,
    account_id: Option<String>,
}

#[derive(Debug, PartialEq)]
enum Login {
    Found(Credentials),
    Missing,
    /// Signed in with an API key: billed per token, no plan windows.
    ApiKey,
    Invalid(String),
}

impl CodexUsage {
    /// The reader for this machine: the person's home, quarkd's own
    /// `CODEX_HOME` and Pi's directory.
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let pi = std::env::var("PI_CODING_AGENT_DIR")
            .ok()
            .filter(|d| !d.trim().is_empty())
            .map(|d| match d.strip_prefix("~/") {
                Some(rest) => home.clone().unwrap_or_default().join(rest),
                None => PathBuf::from(d),
            })
            .or_else(|| home.as_ref().map(|h| h.join(".pi").join("agent")));
        let mut r = Self::new(
            home,
            std::env::var_os("CODEX_HOME")
                .filter(|d| !d.is_empty())
                .map(PathBuf::from),
        );
        r.pi_dir = pi;
        r
    }

    pub fn new(user_home: Option<PathBuf>, default_dir: Option<PathBuf>) -> Self {
        Self {
            user_home,
            default_dir,
            pi_dir: None,
            curl: "curl".into(),
            urls: USAGE_URLS.iter().map(|u| u.to_string()).collect(),
            api_timeout: Duration::from_secs(20),
            cache_for: Duration::from_secs(45),
            cache: shared_cache(),
        }
    }

    /// A reader with its own cache, for tests.
    pub fn isolated(mut self) -> Self {
        self.cache = Default::default();
        self
    }

    fn default_home(&self) -> Option<PathBuf> {
        self.default_dir
            .clone()
            .or_else(|| self.user_home.as_ref().map(|h| h.join(".codex")))
    }

    /// The reading for the login in `codex_home`; `pi_fallback` lets a
    /// missing Codex login be read through Pi's.
    pub async fn reading_for(&self, codex_home: &Path, pi_fallback: bool) -> Value {
        if let Some((at, v)) = self.cache.lock().unwrap().get(codex_home) {
            if at.elapsed() < self.cache_for {
                return v.clone();
            }
        }
        let v = self.read_now(codex_home, pi_fallback).await;
        self.cache
            .lock()
            .unwrap()
            .insert(codex_home.to_path_buf(), (Instant::now(), v.clone()));
        v
    }

    async fn read_now(&self, codex_home: &Path, pi_fallback: bool) -> Value {
        let now = OffsetDateTime::now_utc();
        let fail = |status: &str, error: &str| failure(PROVIDER, SOURCE, now, status, error, None);
        let mut login = codex_login(&codex_home.join("auth.json"));
        if pi_fallback && matches!(login, Login::Missing) {
            if let Some(pi) = &self.pi_dir {
                login = pi_login(&pi.join("auth.json"));
            }
        }
        let creds = match login {
            Login::Found(c) => c,
            Login::Missing => {
                return fail(
                    "auth_required",
                    "Codex is not signed in here; run `codex login`",
                )
            }
            Login::ApiKey => {
                return fail(
                    "unavailable",
                    "Codex signs in with an API key, which has no plan quota",
                )
            }
            Login::Invalid(e) => return fail("unavailable", &e),
        };
        self.fetch(&creds, now).await
    }

    /// The reading for a ChatGPT access token from another tool's login
    /// (Pi's `openai-codex` or OpenCode's `openai` entry). The account id
    /// is taken from the token's claims when not given.
    pub async fn read_token(&self, token: &str, account_id: Option<&str>) -> Value {
        let account_id = account_id.map(str::to_string).or_else(|| {
            jwt_claims(token)?
                .get("https://api.openai.com/auth")?
                .get("chatgpt_account_id")?
                .as_str()
                .map(str::to_string)
        });
        let creds = Credentials {
            token: token.to_string(),
            account_id,
        };
        self.fetch(&creds, OffsetDateTime::now_utc()).await
    }

    async fn fetch(&self, creds: &Credentials, now: OffsetDateTime) -> Value {
        let fail = |status: &str, error: &str| failure(PROVIDER, SOURCE, now, status, error, None);
        let mut rejected = false;
        let mut last = "Codex quota unavailable".to_string();
        for url in &self.urls {
            let mut headers = vec![("User-Agent", "quarkd")];
            if let Some(id) = &creds.account_id {
                headers.push(("ChatGPT-Account-Id", id.as_str()));
            }
            match curl_get(&self.curl, url, &creds.token, &headers, self.api_timeout).await {
                Ok((200..=299, body)) => {
                    match serde_json::from_str::<Value>(&body)
                        .ok()
                        .and_then(|v| usage_reading(&v, now))
                    {
                        Some(r) => return r,
                        None => last = "Codex reported no quota windows".into(),
                    }
                }
                Ok((401 | 403, _)) => rejected = true,
                Ok((429, _)) => return fail("rate_limited", "Codex quota endpoint rate limited"),
                Ok((c, _)) => last = format!("Codex quota unavailable (HTTP {c})"),
                Err(e) => last = format!("Codex quota unavailable: {e}"),
            }
        }
        if rejected {
            fail(
                "auth_required",
                "Codex sign-in required; run `codex` once to renew it, or `codex login`",
            )
        } else {
            fail("unavailable", &last)
        }
    }
}

fn read_json(path: &Path) -> Result<Option<Value>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|_| format!("{} is not JSON", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

fn text(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The claims of a JWT, unverified.
fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn codex_login(path: &Path) -> Login {
    let v = match read_json(path) {
        Ok(Some(v)) => v,
        Ok(None) => return Login::Missing,
        Err(e) => return Login::Invalid(e),
    };
    let tokens = v.get("tokens").filter(|t| t.is_object());
    let Some(token) = tokens.and_then(|t| text(t.get("access_token").or(t.get("accessToken"))))
    else {
        return if text(v.get("OPENAI_API_KEY")).is_some() {
            Login::ApiKey
        } else {
            Login::Missing
        };
    };
    let tokens = tokens.unwrap_or(&Value::Null);
    let claims = text(tokens.get("id_token").or(tokens.get("idToken")))
        .and_then(|t| jwt_claims(&t))
        .or_else(|| jwt_claims(&token));
    let account_id = text(tokens.get("account_id").or(tokens.get("accountId"))).or_else(|| {
        let c = claims.as_ref()?;
        text(
            c.get("https://api.openai.com/auth")
                .and_then(|a| a.get("chatgpt_account_id")),
        )
        .or_else(|| text(c.get("https://api.openai.com/auth/account_id")))
        .or_else(|| text(c.get("account_id")))
    });
    Login::Found(Credentials { token, account_id })
}

fn pi_login(path: &Path) -> Login {
    let v = match read_json(path) {
        Ok(Some(v)) => v,
        Ok(None) => return Login::Missing,
        Err(e) => return Login::Invalid(e),
    };
    let Some(entry) = v.get(PI_ENTRY) else {
        return Login::Missing;
    };
    if entry.get("type").and_then(Value::as_str) != Some("oauth") {
        return Login::Missing;
    }
    match text(entry.get("access")) {
        Some(token) => Login::Found(Credentials {
            token,
            account_id: text(entry.get("accountId")),
        }),
        None => Login::Invalid("Pi's Codex login has no access token".into()),
    }
}

/// A rate-limit window, named by its length (five hours is the session,
/// seven days the week).
fn rate_window(
    raw: Option<&Value>,
    ids: (&str, &str, &str),
    weekly: (&str, &str, &str),
    other: &dyn Fn(f64) -> (String, String, String),
    now: OffsetDateTime,
) -> Option<Window> {
    let raw = raw.filter(|r| r.is_object())?;
    let used = raw
        .get("used_percent")
        .or_else(|| raw.get("usedPercent"))
        .and_then(Value::as_f64)?;
    let seconds = raw
        .get("limit_window_seconds")
        .and_then(Value::as_f64)
        .or_else(|| {
            raw.get("windowDurationMins")
                .and_then(Value::as_f64)
                .map(|m| m * 60.0)
        });
    let (id, label, kind) = match seconds {
        None => (ids.0.into(), ids.1.into(), ids.2.into()),
        Some(s) if s == FIVE_HOURS => (ids.0.into(), ids.1.into(), ids.2.into()),
        Some(s) if s == SEVEN_DAYS => (weekly.0.into(), weekly.1.into(), weekly.2.into()),
        Some(s) => other(s),
    };
    let reset = raw
        .get("reset_at")
        .or_else(|| raw.get("resetsAt"))
        .and_then(parse_time)
        .or_else(|| {
            raw.get("reset_after_seconds")
                .and_then(Value::as_f64)
                .map(|s| now + time::Duration::seconds_f64(s))
        });
    Some(
        Window::new(&id, &label, &kind, used)
            .resets(reset)
            .lasting(seconds),
    )
}

fn hours(s: f64) -> String {
    let h = s / 3600.0;
    if h.fract() == 0.0 {
        format!("{h}h")
    } else {
        format!("{:.2}h", h)
    }
}

fn pair(container: Option<&Value>, prefix: &str, label: &str, now: OffsetDateTime) -> Vec<Window> {
    let Some(c) = container.filter(|c| c.is_object()) else {
        return Vec::new();
    };
    let (session, week) = if prefix.is_empty() {
        (
            ("five_hour".to_string(), "session".to_string(), "session"),
            ("weekly".to_string(), "week".to_string(), "weekly"),
        )
    } else {
        (
            (
                format!("{prefix}_five_hour"),
                format!("{label} session"),
                "session",
            ),
            (
                format!("{prefix}_weekly"),
                format!("{label} week"),
                "weekly",
            ),
        )
    };
    let other = |s: f64| {
        let p = if prefix.is_empty() {
            "window"
        } else {
            "code_review_window"
        };
        (
            format!("{p}:{}", hours(s)),
            format!("{} window", hours(s)),
            "unknown".to_string(),
        )
    };
    [
        c.get("primary_window").or_else(|| c.get("primary")),
        c.get("secondary_window").or_else(|| c.get("secondary")),
    ]
    .into_iter()
    .filter_map(|w| {
        rate_window(
            w,
            (&session.0, &session.1, session.2),
            (&week.0, &week.1, week.2),
            &other,
            now,
        )
    })
    .collect()
}

fn model_windows(id: &str, label: &str, c: &Value, now: OffsetDateTime) -> Vec<Window> {
    let (s_id, s_label) = (format!("model:{id}:5h"), format!("{label} session"));
    let (w_id, w_label) = (format!("model:{id}:7d"), format!("{label} week"));
    let other = |s: f64| {
        (
            format!("model:{id}:window:{}", hours(s)),
            format!("{label} {} window", hours(s)),
            "model".to_string(),
        )
    };
    [
        c.get("primary_window").or_else(|| c.get("primary")),
        c.get("secondary_window").or_else(|| c.get("secondary")),
    ]
    .into_iter()
    .filter_map(|w| {
        rate_window(
            w,
            (&s_id, &s_label, "model"),
            (&w_id, &w_label, "model"),
            &other,
            now,
        )
    })
    .collect()
}

fn spend_cap(v: Option<&Value>, now: OffsetDateTime) -> Option<Window> {
    let control = v?;
    let cap = control.get("individual_limit")?;
    let num = |k: &str| cap.get(k).and_then(Value::as_f64);
    let used = if control.get("reached") == Some(&Value::Bool(true)) {
        100.0
    } else {
        match (num("limit"), num("used")) {
            (Some(l), Some(u)) if l > 0.0 && u >= 0.0 => 100.0 * u / l,
            _ => num("used_percent")?,
        }
    };
    let reset = cap
        .get("reset_at")
        .and_then(parse_time)
        .or_else(|| num("reset_after_seconds").map(|s| now + time::Duration::seconds_f64(s)));
    Some(Window::new("spend_control", "workspace credit cap", "credits", used).resets(reset))
}

/// A schema-5 reading from ChatGPT's usage answer, or `None` when it lists
/// no window.
fn usage_reading(v: &Value, now: OffsetDateTime) -> Option<Value> {
    let rate = ["rate_limit", "rateLimits", "rate_limits"]
        .iter()
        .find_map(|k| v.get(*k).filter(|r| r.is_object()))
        .unwrap_or(v);
    let mut windows = pair(Some(rate), "", "", now);
    windows.extend(pair(
        v.get("code_review_rate_limit"),
        "code_review",
        "code review",
        now,
    ));
    let mut models: Vec<String> = Vec::new();
    for e in v
        .get("additional_rate_limits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let id = text(e.get("metered_feature")).or_else(|| text(e.get("limit_name")));
        let label = text(e.get("limit_name")).or_else(|| id.clone());
        if let (Some(id), Some(label), Some(c)) = (id, label, e.get("rate_limit")) {
            let w = model_windows(&id, &label, c, now);
            if !w.is_empty() {
                models.push(id);
            }
            windows.extend(w);
        }
    }
    windows.extend(spend_cap(v.get("spend_control"), now));
    if windows.is_empty() {
        return None;
    }
    let cap: Vec<&Window> = windows.iter().filter(|w| w.id == "spend_control").collect();
    let base: Vec<&Window> = windows
        .iter()
        .filter(|w| w.id == "five_hour" || w.id == "weekly")
        .collect();
    let account = if base.is_empty() && cap.len() == windows.len() {
        cap
    } else {
        base
    };
    let review: Vec<&Window> = windows
        .iter()
        .filter(|w| w.id.starts_with("code_review_"))
        .collect();
    let mut scopes = vec![
        ("all_models".to_string(), account.clone()),
        ("code_review".to_string(), review),
    ];
    for m in models {
        let prefix = format!("model:{m}:");
        let mut bound = account.clone();
        bound.extend(windows.iter().filter(|w| w.id.starts_with(&prefix)));
        scopes.push((format!("model:{m}"), bound));
    }
    let mut extra = Map::new();
    if let Some(plan) = text(v.get("plan_type")).or_else(|| text(v.get("planType"))) {
        extra.insert("plan".into(), json!(plan));
    }
    if let Some(c) = v.get("credits").or_else(|| rate.get("credits")) {
        if c.get("balance").is_some() || c.get("unlimited").is_some() {
            extra.insert(
                "credits".into(),
                json!({"remaining": c.get("balance"), "unlimited": c.get("unlimited"), "unit": "credits"}),
            );
        }
    }
    Some(reading(PROVIDER, SOURCE, now, &windows, &scopes, extra))
}

#[async_trait]
impl super::LocalReading for CodexUsage {
    fn provider(&self) -> &str {
        PROVIDER
    }

    async fn read(&self) -> Value {
        match self.default_home() {
            Some(home) => self.reading_for(&home, true).await,
            None => failure(
                PROVIDER,
                SOURCE,
                OffsetDateTime::now_utc(),
                "auth_required",
                "no home directory to find Codex's login in",
                None,
            ),
        }
    }

    async fn read_account(&self, config_dir: Option<&Path>, _grant: bool) -> Option<Value> {
        Some(match config_dir {
            Some(dir) => self.reading_for(dir, false).await,
            None => self.read().await,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::LocalReading;

    fn now() -> OffsetDateTime {
        OffsetDateTime::parse(
            "2026-11-11T12:00:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap()
    }

    #[test]
    fn reads_rate_limits_and_model_limits() {
        let v = json!({
            "plan_type": "plus",
            "rate_limit": {
                "primary_window": {"used_percent": 20, "limit_window_seconds": 18000, "reset_after_seconds": 3600},
                "secondary_window": {"used_percent": 35.5, "limit_window_seconds": 604800, "reset_at": 1763380800},
            },
            "code_review_rate_limit": {"primary_window": {"used_percent": 5, "limit_window_seconds": 604800, "reset_after_seconds": 9000}},
            "additional_rate_limits": [{"limit_name": "GPT-5.5", "metered_feature": "gpt-5.5",
                "rate_limit": {"primary_window": {"used_percent": 90, "limit_window_seconds": 18000, "reset_after_seconds": 600}}}],
            "credits": {"balance": "0", "unlimited": false},
        });
        let r = usage_reading(&v, now()).unwrap();
        assert!(crate::quota::valid(&r), "{r}");
        let p = &r["providers"][0];
        assert_eq!(p["plan"], "plus");
        let rows = p["quotaSemantics"]["effectiveAvailability"]
            .as_array()
            .unwrap();
        let scopes: Vec<_> = rows.iter().map(|r| r["scope"].as_str().unwrap()).collect();
        assert_eq!(scopes, ["all_models", "code_review", "model:gpt-5.5"]);
        assert_eq!(rows[0]["effectivePercentRemaining"], json!(64.5));
        assert_eq!(rows[2]["effectivePercentRemaining"], json!(10));
        let ids: Vec<_> = p["windows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            [
                "five_hour",
                "weekly",
                "code_review_weekly",
                "model:gpt-5.5:5h"
            ]
        );
        assert!(usage_reading(&json!({}), now()).is_none());
    }

    #[test]
    fn finds_the_account_id() {
        let tmp = tempfile::tempdir().unwrap();
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acc-1"}}"#);
        let auth = tmp.path().join("auth.json");
        std::fs::write(
            &auth,
            format!(r#"{{"tokens":{{"access_token":"tok","id_token":"h.{claims}.s"}}}}"#),
        )
        .unwrap();
        assert_eq!(
            codex_login(&auth),
            Login::Found(Credentials {
                token: "tok".into(),
                account_id: Some("acc-1".into())
            })
        );
        std::fs::write(&auth, r#"{"OPENAI_API_KEY":"sk-x","tokens":null}"#).unwrap();
        assert_eq!(codex_login(&auth), Login::ApiKey);
        assert_eq!(codex_login(&tmp.path().join("none.json")), Login::Missing);
    }

    #[tokio::test]
    async fn the_default_login_falls_back_to_pi() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let pi = dir.join("pi");
        std::fs::create_dir_all(&pi).unwrap();
        std::fs::write(
            pi.join("auth.json"),
            r#"{"openai-codex":{"type":"oauth","access":"pitok","refresh":"r","expires":1,"accountId":"a"}}"#,
        )
        .unwrap();
        let curl = dir.join("curl");
        std::fs::write(
            &curl,
            "#!/bin/sh\ncat | grep -q 'Bearer pitok' && printf '{\"rate_limit\":{\"primary_window\":{\"used_percent\":10}}}\\n200' || printf 'x\\n401'\n",
        )
        .unwrap();
        std::fs::set_permissions(&curl, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let mut r = CodexUsage::new(Some(dir.to_path_buf()), None).isolated();
        r.pi_dir = Some(pi);
        r.curl = curl.to_string_lossy().into_owned();
        r.urls = vec!["http://x/usage".into()];
        let v = r.read().await;
        assert_eq!(v["providers"][0]["state"]["status"], "fresh", "{v}");
        // An added account never borrows Pi's login.
        let other = dir.join("other");
        let v = r.read_account(Some(&other), false).await.unwrap();
        assert_eq!(v["providers"][0]["state"]["status"], "auth_required", "{v}");
    }
}
