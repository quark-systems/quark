//! Claude Code's plan usage, read the way quota-axi read it, without it.
//!
//! The login is Claude Code's own OAuth session: on macOS the Keychain item
//! `Claude Code-credentials` (suffixed `-<8 hex>`, the SHA-256 of the raw
//! `CLAUDE_CONFIG_DIR`, for any other config directory), else
//! `<config dir>/.credentials.json`. The Keychain item is read with
//! `security find-generic-password`, the same tool Claude Code writes it
//! with, so the item already trusts it and a quarkd started by launchd (the
//! QuarkBar LaunchAgent) reads it like a shell does. Should the item still
//! want a prompt, a background read gives up within seconds and remembers
//! the item until the person allows it from Accounts
//! ([`super::LocalReading::read_account`] with `grant`), so quarkd never
//! keeps raising dialogs.
//!
//! The session's access token is sent to Anthropic's usage endpoint
//! (`GET /api/oauth/usage`), which answers the five-hour session and weekly
//! windows, model-scoped weekly windows, and extra usage. The refresh token
//! is never used here: Anthropic rotates it on use, so spending it would
//! sign Claude Code out. When the access token has expired, quarkd lets
//! Claude Code renew its own session (`claude doctor`, at most every ten
//! minutes per login) and reads again.
//!
//! The account windows (session and week) bound `all_models`; a model's
//! window adds a bound for that model's scope.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::windows::{failure, reading, Window};
use super::{curl_get, parse_time, run_with};

pub const PROVIDER: &str = "claude";
/// Claude Code's Keychain service for the default config directory.
pub const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";
const SOURCE: &str = "quarkd: Claude Code OAuth usage";
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const OAUTH_BETA: &str = "oauth-2025-04-20";
const USER_AGENT: &str = "claude-code/2.1.202";
/// `security`'s exit status for an item that is not there.
const ITEM_NOT_FOUND: i32 = 44;
const FIVE_HOURS: f64 = 18_000.0;
const SEVEN_DAYS: f64 = 604_800.0;
/// The `state.reason` of a reading the Keychain withheld.
pub const KEYCHAIN_REASON: &str = "keychain_access_required";

/// The Keychain service Claude Code keeps a login in: `selector` is
/// `CLAUDE_CONFIG_DIR` exactly as set, `None` when it is unset.
pub fn keychain_service(selector: Option<&str>) -> String {
    match selector {
        None => KEYCHAIN_SERVICE.to_string(),
        Some(dir) => {
            let digest = Sha256::digest(dir.as_bytes());
            let hex: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
            format!("{KEYCHAIN_SERVICE}-{hex}")
        }
    }
}

/// What reads have learned, shared by every copy of a reader.
#[derive(Debug, Default)]
struct Memory {
    /// Keychain services that would prompt: skipped until granted.
    withheld: HashSet<String>,
    /// Last reading per login.
    cache: HashMap<String, (Instant, Value)>,
    /// Last `claude doctor` per login.
    renewed: HashMap<String, Instant>,
}

/// Reads Claude Code's plan usage for any of its logins.
#[derive(Clone)]
pub struct ClaudeUsage {
    /// The person's home, where `~/.claude` lives.
    pub user_home: Option<PathBuf>,
    /// `CLAUDE_CONFIG_DIR` in quarkd's own environment: the default login.
    pub default_dir: Option<String>,
    /// Read the macOS Keychain before the credential file.
    pub keychain: bool,
    pub security: String,
    pub curl: String,
    /// Claude Code, run to renew an expired session.
    pub claude: String,
    pub url: String,
    pub api_timeout: Duration,
    /// How long a background Keychain read may take before it counts as a
    /// prompt nobody will answer.
    pub keychain_timeout: Duration,
    /// How long a granting read waits for the person.
    pub grant_timeout: Duration,
    pub renew_timeout: Duration,
    pub renew_every: Duration,
    /// How long a reading is reused, so dispatch and Accounts reading
    /// together make one request.
    pub cache_for: Duration,
    memory: Arc<Mutex<Memory>>,
}

impl std::fmt::Debug for ClaudeUsage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaudeUsage")
            .field("user_home", &self.user_home)
            .field("default_dir", &self.default_dir)
            .field("keychain", &self.keychain)
            .finish_non_exhaustive()
    }
}

/// One process-wide memory, so the dispatch and Accounts readers share
/// their cache and what the Keychain withheld.
fn shared_memory() -> Arc<Mutex<Memory>> {
    static MEMORY: std::sync::OnceLock<Arc<Mutex<Memory>>> = std::sync::OnceLock::new();
    MEMORY.get_or_init(Default::default).clone()
}

#[derive(Debug, Clone, PartialEq)]
struct Credentials {
    token: String,
    refreshable: bool,
    expires_at_ms: Option<i64>,
    plan: Option<String>,
}

#[derive(Debug, PartialEq)]
enum Login {
    Found(Credentials),
    Missing,
    Invalid(String),
    /// The Keychain would prompt, or refused.
    Withheld(String),
}

impl ClaudeUsage {
    /// The reader for this machine: the person's home and quarkd's own
    /// `CLAUDE_CONFIG_DIR`.
    pub fn from_env() -> Self {
        Self::new(
            std::env::var_os("HOME").map(PathBuf::from),
            std::env::var("CLAUDE_CONFIG_DIR")
                .ok()
                .filter(|d| !d.is_empty()),
        )
    }

    pub fn new(user_home: Option<PathBuf>, default_dir: Option<String>) -> Self {
        Self {
            user_home,
            default_dir,
            keychain: cfg!(target_os = "macos"),
            security: "security".into(),
            curl: "curl".into(),
            claude: "claude".into(),
            url: USAGE_URL.into(),
            api_timeout: Duration::from_secs(20),
            keychain_timeout: Duration::from_secs(10),
            grant_timeout: Duration::from_secs(90),
            renew_timeout: Duration::from_secs(45),
            renew_every: Duration::from_secs(600),
            cache_for: Duration::from_secs(45),
            memory: shared_memory(),
        }
    }

    /// A reader with its own memory, for tests.
    pub fn isolated(mut self) -> Self {
        self.memory = Default::default();
        self
    }

    /// The login's config directory.
    fn config_dir(&self, selector: Option<&str>) -> Option<PathBuf> {
        match selector {
            Some(d) => Some(PathBuf::from(d)),
            None => self.user_home.as_ref().map(|h| h.join(".claude")),
        }
    }

    /// The reading for the login `selector` names (`CLAUDE_CONFIG_DIR` as
    /// it is set for it; `None` for the person's usual login).
    pub async fn reading_for(&self, selector: Option<&str>, grant: bool) -> Value {
        let key = selector.unwrap_or("").to_string();
        if !grant {
            let mem = self.memory.lock().unwrap();
            if let Some((at, v)) = mem.cache.get(&key) {
                if at.elapsed() < self.cache_for {
                    return v.clone();
                }
            }
        }
        let v = self.read_now(selector, grant).await;
        self.memory
            .lock()
            .unwrap()
            .cache
            .insert(key, (Instant::now(), v.clone()));
        v
    }

    async fn read_now(&self, selector: Option<&str>, grant: bool) -> Value {
        let now = OffsetDateTime::now_utc();
        let fail = |status: &str, error: &str, reason: Option<&str>| {
            failure(PROVIDER, SOURCE, now, status, error, reason)
        };
        let mut creds = match self.login(selector, grant).await {
            Login::Found(c) => c,
            Login::Missing => {
                return fail(
                    "auth_required",
                    "Claude Code is not signed in here; run `claude` and sign in",
                    None,
                )
            }
            Login::Invalid(e) => return fail("unavailable", &e, None),
            Login::Withheld(e) => return fail("unavailable", &e, Some(KEYCHAIN_REASON)),
        };
        let expired = |c: &Credentials| {
            c.expires_at_ms
                .is_some_and(|ms| ms <= (now.unix_timestamp_nanos() / 1_000_000) as i64)
        };
        if expired(&creds) && creds.refreshable && self.renew(selector).await {
            if let Login::Found(c) = self.login(selector, grant).await {
                creds = c;
            }
        }
        let (code, body) = match curl_get(
            &self.curl,
            &self.url,
            &creds.token,
            &[
                ("anthropic-beta", OAUTH_BETA),
                ("User-Agent", USER_AGENT),
                ("Content-Type", "application/json"),
            ],
            self.api_timeout,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                return fail(
                    "unavailable",
                    &format!("Claude quota unavailable: {e}"),
                    None,
                )
            }
        };
        match code {
            200..=299 => {}
            401 if expired(&creds) => {
                return fail(
                    "auth_required",
                    "Claude Code's login has expired; run `claude` once to renew it",
                    None,
                )
            }
            401 => return fail("auth_required", "Claude sign-in required", None),
            429 => return fail("rate_limited", "Claude quota endpoint rate limited", None),
            c => {
                return fail(
                    "unavailable",
                    &format!("Claude quota unavailable (HTTP {c})"),
                    None,
                )
            }
        }
        let Ok(usage) = serde_json::from_str::<Value>(&body) else {
            return fail("unavailable", "Claude quota response is not JSON", None);
        };
        usage_reading(&usage, creds.plan.as_deref(), now)
    }

    /// The login's credentials: the Keychain item first on macOS, then the
    /// credential file.
    async fn login(&self, selector: Option<&str>, grant: bool) -> Login {
        if self.keychain {
            let service = keychain_service(selector);
            match self.keychain_item(&service, grant).await {
                Ok(Some(text)) => return parse_credentials(&text),
                Ok(None) => {}
                Err(e) => return Login::Withheld(e),
            }
        }
        let Some(dir) = self.config_dir(selector) else {
            return Login::Missing;
        };
        match std::fs::read_to_string(dir.join(".credentials.json")) {
            Ok(text) => parse_credentials(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Login::Missing,
            Err(e) => Login::Invalid(format!("cannot read Claude Code's credentials: {e}")),
        }
    }

    /// The Keychain item's secret, `None` when there is no such item, or
    /// why it was withheld.
    async fn keychain_item(&self, service: &str, grant: bool) -> Result<Option<String>, String> {
        let withheld = || {
            "Keychain access needed: allow Quark to read Claude Code's login \
             (Accounts, Allow Keychain access)"
                .to_string()
        };
        if !grant && self.memory.lock().unwrap().withheld.contains(service) {
            return Err(withheld());
        }
        let argv = [
            self.security.clone(),
            "find-generic-password".into(),
            "-w".into(),
            "-s".into(),
            service.into(),
        ];
        let timeout = if grant {
            self.grant_timeout
        } else {
            self.keychain_timeout
        };
        let out = tokio::time::timeout(timeout, async {
            tokio::process::Command::new(&argv[0])
                .args(&argv[1..])
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output()
                .await
        })
        .await;
        let mark = |withhold: bool| {
            let mut mem = self.memory.lock().unwrap();
            if withhold {
                mem.withheld.insert(service.to_string());
            } else {
                mem.withheld.remove(service);
            }
        };
        match out {
            Ok(Ok(o)) if o.status.success() => {
                mark(false);
                Ok(Some(String::from_utf8_lossy(&o.stdout).trim().to_string()))
            }
            Ok(Ok(o)) if o.status.code() == Some(ITEM_NOT_FOUND) => {
                mark(false);
                Ok(None)
            }
            // No Keychain tool here: fall back to the file.
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Ok(Ok(_)) | Ok(Err(_)) | Err(_) => {
                mark(true);
                Err(withheld())
            }
        }
    }

    /// Let Claude Code renew the login's expired session, at most once per
    /// [`Self::renew_every`]. Whether it ran.
    async fn renew(&self, selector: Option<&str>) -> bool {
        let key = selector.unwrap_or("").to_string();
        {
            let mut mem = self.memory.lock().unwrap();
            if mem
                .renewed
                .get(&key)
                .is_some_and(|at| at.elapsed() < self.renew_every)
            {
                return false;
            }
            mem.renewed.insert(key, Instant::now());
        }
        let env: Vec<(&str, &str)> = selector
            .map(|d| ("CLAUDE_CONFIG_DIR", d))
            .into_iter()
            .collect();
        let argv = [self.claude.clone(), "doctor".into()];
        run_with(&argv, None, &env, self.renew_timeout)
            .await
            .is_ok()
    }
}

fn parse_credentials(text: &str) -> Login {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return Login::Invalid("Claude Code's stored login is not JSON".into());
    };
    let oauth = v.get("claudeAiOauth").unwrap_or(&v);
    let Some(token) = oauth
        .get("accessToken")
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty())
    else {
        return Login::Missing;
    };
    Login::Found(Credentials {
        token: token.trim().to_string(),
        refreshable: oauth.get("refreshToken").is_some_and(|t| !t.is_null()),
        expires_at_ms: oauth
            .get("expiresAt")
            .and_then(Value::as_f64)
            .map(|n| n as i64),
        plan: oauth
            .get("subscriptionType")
            .or_else(|| v.get("subscriptionType"))
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn window(raw: &Value, id: &str, label: &str, kind: &str, seconds: Option<f64>) -> Option<Window> {
    let used = raw.get("utilization").and_then(Value::as_f64)?;
    let reset = raw.get("resets_at").or_else(|| raw.get("reset_at"));
    Some(
        Window::new(id, label, kind, used)
            .resets(reset.and_then(parse_time))
            .lasting(seconds),
    )
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

/// The windows Anthropic's usage answer lists: its `limits` array when it
/// has one (every window, model-scoped ones included), else the fixed
/// fields; plus extra usage when it is on.
fn usage_windows(v: &Value) -> Vec<Window> {
    let mut out = Vec::new();
    for e in v
        .get("limits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(used) = e.get("percent").and_then(Value::as_f64) else {
            continue;
        };
        let reset = e.get("resets_at").and_then(parse_time);
        let model = e.get("scope").and_then(|s| s.get("model"));
        let name = model
            .and_then(|m| m.get("display_name"))
            .and_then(Value::as_str);
        let w = if let Some(name) = name {
            let key = model
                .and_then(|m| m.get("id"))
                .and_then(Value::as_str)
                .map_or_else(|| slug(name), str::to_string);
            Window::new(
                &format!("model:{key}"),
                &format!("{name} week"),
                "model",
                used,
            )
            .lasting(Some(SEVEN_DAYS))
        } else {
            match e.get("group").and_then(Value::as_str) {
                Some("session") => {
                    Window::new("five_hour", "session", "session", used).lasting(Some(FIVE_HOURS))
                }
                Some("weekly") => {
                    Window::new("seven_day", "week", "weekly", used).lasting(Some(SEVEN_DAYS))
                }
                _ => {
                    let kind = e.get("kind").and_then(Value::as_str).unwrap_or("limit");
                    Window::new(kind, kind, "unknown", used)
                }
            }
        };
        out.push(w.resets(reset));
    }
    if out.is_empty() {
        out.extend(
            [
                v.get("five_hour")
                    .and_then(|w| window(w, "five_hour", "session", "session", Some(FIVE_HOURS))),
                v.get("seven_day")
                    .and_then(|w| window(w, "seven_day", "week", "weekly", Some(SEVEN_DAYS))),
                v.get("seven_day_opus").and_then(|w| {
                    window(w, "seven_day_opus", "opus week", "model", Some(SEVEN_DAYS))
                }),
            ]
            .into_iter()
            .flatten(),
        );
    }
    if let Some(extra) = v
        .get("extra_usage")
        .filter(|e| e.get("is_enabled") == Some(&Value::Bool(true)))
    {
        let places = extra
            .get("decimal_places")
            .and_then(Value::as_f64)
            .unwrap_or(2.0);
        let div = 10f64.powf(places);
        let spent = extra
            .get("used_credits")
            .and_then(Value::as_f64)
            .map(|c| c / div);
        let limit = extra
            .get("monthly_limit")
            .and_then(Value::as_f64)
            .map(|c| c / div);
        let used = extra
            .get("utilization")
            .and_then(Value::as_f64)
            .or(match (spent, limit) {
                (Some(s), Some(l)) if l > 0.0 => Some(100.0 * s / l),
                _ => None,
            });
        if let Some(used) = used {
            out.push(Window::new("extra_usage", "extra usage", "credits", used));
        }
    }
    out
}

/// A schema-5 reading from Anthropic's usage answer.
fn usage_reading(v: &Value, plan: Option<&str>, now: OffsetDateTime) -> Value {
    let windows = usage_windows(v);
    let account: Vec<&Window> = windows
        .iter()
        .filter(|w| w.id == "five_hour" || w.id == "seven_day")
        .collect();
    let mut scopes = vec![("all_models".to_string(), account.clone())];
    for m in windows.iter().filter(|w| w.kind == "model") {
        let mut bound = account.clone();
        bound.push(m);
        scopes.push((m.id.clone(), bound));
    }
    let mut extra = Map::new();
    if let Some(p) = plan {
        extra.insert("plan".into(), json!(p));
    }
    if windows.is_empty() {
        return failure(
            PROVIDER,
            SOURCE,
            now,
            "unavailable",
            "Claude reported no quota windows",
            None,
        );
    }
    reading(PROVIDER, SOURCE, now, &windows, &scopes, extra)
}

#[async_trait]
impl super::LocalReading for ClaudeUsage {
    fn provider(&self) -> &str {
        PROVIDER
    }

    async fn read(&self) -> Value {
        self.reading_for(self.default_dir.as_deref(), false).await
    }

    async fn read_account(&self, config_dir: Option<&Path>, grant: bool) -> Option<Value> {
        let selector = match config_dir {
            Some(d) => Some(d.to_string_lossy().into_owned()),
            None => self.default_dir.clone(),
        };
        Some(self.reading_for(selector.as_deref(), grant).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::LocalReading;

    #[test]
    fn keychain_service_hashes_the_raw_directory() {
        assert_eq!(keychain_service(None), "Claude Code-credentials");
        let s = keychain_service(Some("/Users/me/.claude-work"));
        assert!(s.starts_with("Claude Code-credentials-"));
        assert_eq!(s.len(), KEYCHAIN_SERVICE.len() + 9);
        // sha256("/tmp/x") = 9b0b9f9f...
        let want: String = Sha256::digest(b"/tmp/x")
            .iter()
            .take(4)
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(
            keychain_service(Some("/tmp/x")),
            format!("Claude Code-credentials-{want}")
        );
    }

    #[test]
    fn reads_the_fixed_fields_and_limits() {
        let now = OffsetDateTime::parse(
            "2026-11-11T12:00:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        let v = json!({
            "five_hour": {"utilization": 25.0, "resets_at": "2026-11-11T14:00:00Z"},
            "seven_day": {"utilization": 40.0, "resets_at": "2026-11-15T12:00:00Z"},
            "seven_day_opus": null,
            "extra_usage": {"is_enabled": true, "used_credits": 500, "monthly_limit": 2000},
        });
        let r = usage_reading(&v, Some("max"), now);
        assert!(crate::quota::valid(&r), "{r}");
        let p = &r["providers"][0];
        assert_eq!(p["plan"], "max");
        assert_eq!(p["windows"].as_array().unwrap().len(), 3);
        let row = &p["quotaSemantics"]["effectiveAvailability"][0];
        assert_eq!(row["scope"], "all_models");
        assert_eq!(row["effectivePercentRemaining"], json!(60));

        let v = json!({"limits": [
            {"group": "session", "percent": 10, "resets_at": "2026-11-11T14:00:00Z"},
            {"group": "weekly", "percent": 20, "resets_at": "2026-11-15T12:00:00Z"},
            {"percent": 90, "resets_at": "2026-11-15T12:00:00Z",
             "scope": {"model": {"id": "opus", "display_name": "Opus"}}},
        ]});
        let r = usage_reading(&v, None, now);
        assert!(crate::quota::valid(&r), "{r}");
        let rows = &r["providers"][0]["quotaSemantics"]["effectiveAvailability"];
        assert_eq!(rows[1]["scope"], "model:opus");
        assert_eq!(rows[1]["effectivePercentRemaining"], json!(10));
    }

    fn stub(dir: &Path, name: &str, body: &str) -> String {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        p.to_string_lossy().into_owned()
    }

    fn reader(dir: &Path) -> ClaudeUsage {
        let mut r = ClaudeUsage::new(Some(dir.to_path_buf()), None).isolated();
        r.keychain = true;
        r.cache_for = Duration::ZERO;
        r.keychain_timeout = Duration::from_millis(500);
        r.claude = stub(dir, "claude", "exit 0");
        // The token arrives on stdin; echo it back in the answer.
        r.curl = stub(
            dir,
            "curl",
            "tok=$(cat | sed 's/.*Bearer //; s/\"$//')\n\
             if [ \"$tok\" = good ]; then printf '{\"five_hour\":{\"utilization\":30}}\\n200'; \
             else printf 'no\\n401'; fi",
        );
        r
    }

    #[tokio::test]
    async fn reads_the_keychain_then_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let mut r = reader(dir);
        let login = r#"{"claudeAiOauth":{"accessToken":"good","subscriptionType":"pro"}}"#;
        // The Keychain answers for the default service only.
        r.security = stub(
            dir,
            "security",
            &format!("[ \"$4\" = '{KEYCHAIN_SERVICE}' ] || exit 44\nprintf '%s' '{login}'"),
        );
        let v = r.read().await;
        let p = &v["providers"][0];
        assert_eq!(p["plan"], "pro", "{v}");
        assert_eq!(
            p["quotaSemantics"]["effectiveAvailability"][0]["effectivePercentRemaining"],
            json!(70)
        );

        // An added account with no item falls back to its credential file.
        let acct = dir.join("acct");
        std::fs::create_dir_all(&acct).unwrap();
        let v = r.read_account(Some(&acct), false).await.unwrap();
        assert_eq!(v["providers"][0]["state"]["status"], "auth_required", "{v}");
        std::fs::write(acct.join(".credentials.json"), login).unwrap();
        let v = r.read_account(Some(&acct), false).await.unwrap();
        assert_eq!(v["providers"][0]["state"]["status"], "fresh", "{v}");

        let bad = r#"{"claudeAiOauth":{"accessToken":"bad"}}"#;
        std::fs::write(acct.join(".credentials.json"), bad).unwrap();
        let v = r.read_account(Some(&acct), false).await.unwrap();
        assert_eq!(
            v["providers"][0]["state"]["error"],
            "Claude sign-in required"
        );
    }

    #[tokio::test]
    async fn a_prompting_keychain_waits_for_a_grant() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let mut r = reader(dir);
        let calls = dir.join("calls");
        // Hangs like an unanswered prompt unless granted (marker file).
        r.security = stub(
            dir,
            "security",
            &format!(
                "echo x >> '{c}'\n[ -f '{g}' ] || sleep 5\nprintf '%s' '{{\"claudeAiOauth\":{{\"accessToken\":\"good\"}}}}'",
                c = calls.display(),
                g = dir.join("granted").display(),
            ),
        );
        let v = r.read().await;
        assert_eq!(v["providers"][0]["state"]["reason"], KEYCHAIN_REASON, "{v}");
        // Withheld items are not asked again in the background.
        let v = r.read().await;
        assert_eq!(v["providers"][0]["state"]["reason"], KEYCHAIN_REASON);
        assert_eq!(std::fs::read_to_string(&calls).unwrap().lines().count(), 1);
        std::fs::write(dir.join("granted"), "").unwrap();
        let v = r.read_account(None, true).await.unwrap();
        assert_eq!(v["providers"][0]["state"]["status"], "fresh", "{v}");
        let v = r.read().await;
        assert_eq!(v["providers"][0]["state"]["status"], "fresh", "{v}");
    }

    #[tokio::test]
    async fn an_expired_login_is_renewed_by_claude_code() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let mut r = reader(dir);
        r.keychain = false;
        let creds = dir.join(".claude");
        std::fs::create_dir_all(&creds).unwrap();
        std::fs::write(
            creds.join(".credentials.json"),
            r#"{"claudeAiOauth":{"accessToken":"old","refreshToken":"r","expiresAt":1000}}"#,
        )
        .unwrap();
        // `claude doctor` rewrites the file with a fresh token.
        r.claude = stub(
            dir,
            "claude",
            &format!(
                "printf '%s' '{{\"claudeAiOauth\":{{\"accessToken\":\"good\",\"refreshToken\":\"r2\",\"expiresAt\":99999999999999}}}}' > '{}'",
                creds.join(".credentials.json").display()
            ),
        );
        let v = r.read().await;
        assert_eq!(v["providers"][0]["state"]["status"], "fresh", "{v}");
    }
}
