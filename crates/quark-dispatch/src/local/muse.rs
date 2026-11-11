//! Muse's subscription usage, read the way quota-axi read it, without it.
//!
//! The login is the Muse CLI's `auth.json` (`$XDG_CONFIG_HOME/muse`, else
//! `~/.config/muse`, `providers.meta.access_token`), else its macOS Keychain
//! item (`ai.meta.dev.credentials`), else `META_API_KEY`. The token goes to
//! Meta's `/muse-code/key` endpoint, which answers the subscription tier and
//! its rolling five-hour and weekly usage. quota-axi asked that endpoint at
//! most every five minutes, and so does quarkd.
//!
//! The five-hour and weekly windows jointly bound `all_models`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use time::OffsetDateTime;

use super::claude::KEYCHAIN_REASON;
use super::support::{self, num, Cache, Http, Keychain, Request};
use super::windows::{failure, reading, Window};
use super::LocalReading;

pub const PROVIDER: &str = "muse";
const SOURCE: &str = "quarkd: Muse usage";
const URL: &str = "https://api.meta.ai/muse-code/key";
const KEYCHAIN_SERVICE: &str = "ai.meta.dev.credentials";
const KEYCHAIN_ACCOUNT: &str = "meta";
const FIVE_HOURS: f64 = 18_000.0;
const SEVEN_DAYS: f64 = 604_800.0;

/// Reads Muse's subscription usage for any of its logins.
#[derive(Debug, Clone)]
pub struct MuseUsage {
    /// The default login's directory (`~/.config/muse`).
    pub default_dir: Option<PathBuf>,
    /// `META_API_KEY` in quarkd's own environment.
    pub api_key: Option<String>,
    pub keychain: Keychain,
    pub http: Http,
    pub url: String,
    pub cache: Cache,
}

impl MuseUsage {
    pub fn from_env() -> Self {
        Self {
            default_dir: support::config_home().map(|c| c.join("muse")),
            api_key: support::env("META_API_KEY"),
            ..Self::new()
        }
    }

    pub fn new() -> Self {
        Self {
            default_dir: None,
            api_key: None,
            keychain: Keychain::default(),
            http: Http::default(),
            url: URL.into(),
            cache: Cache::lasting(Duration::from_secs(300)),
        }
    }

    /// The reading for the login in `dir` (`None`: the default login).
    pub async fn reading_for(&self, dir: Option<&Path>, grant: bool) -> Value {
        let now = OffsetDateTime::now_utc();
        let fail = |status: &str, error: &str, reason: Option<&str>| {
            failure(PROVIDER, SOURCE, now, status, error, reason)
        };
        let file = dir
            .map(Path::to_path_buf)
            .or_else(|| self.default_dir.clone())
            .map(|d| d.join("auth.json"));
        let from_file = match file.as_deref().map(support::read_json) {
            Some(Ok(Some(v))) => support::text(v.pointer("/providers/meta/access_token")),
            Some(Err(e)) => return fail("unavailable", &e, None),
            _ => None,
        };
        let token = match (from_file, dir) {
            (Some(t), _) => t,
            (None, Some(_)) => {
                return fail(
                    "auth_required",
                    "Muse is not signed in for this account; run `muse` and sign in",
                    None,
                )
            }
            (None, None) => match self
                .keychain
                .read(KEYCHAIN_SERVICE, Some(KEYCHAIN_ACCOUNT), grant)
                .await
            {
                Ok(Some(secret)) => match serde_json::from_str::<Value>(&secret)
                    .ok()
                    .and_then(|v| support::text(v.get("access_token")))
                    .or_else(|| self.api_key.clone())
                {
                    Some(t) => t,
                    None => {
                        return fail("unavailable", "Muse's Keychain item holds no token", None)
                    }
                },
                Ok(None) => match &self.api_key {
                    Some(k) => k.clone(),
                    None => {
                        return fail(
                            "auth_required",
                            "Muse is not signed in here; run `muse` and sign in",
                            None,
                        )
                    }
                },
                Err(support::Withheld) => match &self.api_key {
                    Some(k) => k.clone(),
                    None => {
                        return fail(
                            "unavailable",
                            &support::withheld_message("Muse's sign-in"),
                            Some(KEYCHAIN_REASON),
                        )
                    }
                },
            },
        };
        self.read_token(&token).await
    }

    /// The reading for `token`, at most once per five minutes per token.
    pub async fn read_token(&self, token: &str) -> Value {
        use sha2::Digest;
        let key = format!("muse\n{:x}", sha2::Sha256::digest(token.as_bytes()));
        if let Some(v) = self.cache.get(&key) {
            return v;
        }
        let v = self.read_now(token).await;
        self.cache.put(&key, &v);
        v
    }

    async fn read_now(&self, token: &str) -> Value {
        let now = OffsetDateTime::now_utc();
        let fail = |status: &str, error: &str| failure(PROVIDER, SOURCE, now, status, error, None);
        let req = Request::post(&self.url, r#"{"onboard":false}"#)
            .bearer(token)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("User-Agent", "quarkd")
            .header("x-api-version", "1.0.0");
        match self.http.send(&req).await {
            Ok(r) if r.status == 200 => match r.json().and_then(|v| usage_reading(&v, now)) {
                Some(v) => v,
                None => fail("unavailable", "Muse quota response invalid"),
            },
            // 403 also comes from network policy, so only 401 means sign-in.
            Ok(r) if r.status == 403 => {
                fail("unavailable", "Muse refused the quota request (HTTP 403)")
            }
            Ok(r) => {
                let (s, e) = support::http_failure("Muse", r.status);
                fail(s, &e)
            }
            Err(e) => fail("unavailable", &format!("Muse quota unavailable: {e}")),
        }
    }
}

impl Default for MuseUsage {
    fn default() -> Self {
        Self::new()
    }
}

fn usage_reading(v: &Value, now: OffsetDateTime) -> Option<Value> {
    if v.get("is_subs_active").is_none() && v.get("subs_usage").is_none() {
        return None;
    }
    let mut extra = Map::new();
    if let Some(plan) = support::text(v.get("subs_tier_name")) {
        extra.insert("plan".into(), json!(plan));
    }
    let mut windows = Vec::new();
    let usage = v.get("subs_usage").filter(|u| u.is_object());
    let active = v.get("is_subs_active").and_then(Value::as_bool) != Some(false);
    let entry = |e: &Value, id: &str, label: &str, kind: &str, secs: Option<f64>| {
        let used = num(e.get("used_percent")).filter(|u| *u >= 0.0)?;
        let reset = num(e.get("resets_at"))
            .and_then(|s| OffsetDateTime::from_unix_timestamp(s as i64).ok());
        if reset.is_some_and(|r| r <= now) {
            return None;
        }
        Some(
            Window::new(id, label, kind, used)
                .resets(reset)
                .lasting(secs),
        )
    };
    if let (true, Some(u)) = (active, usage) {
        if let Some(w) = u.get("window") {
            let mins = num(w.get("window_duration_mins")).filter(|m| *m > 0.0);
            let made = if mins == Some(300.0) {
                entry(w, "five_hour", "session", "session", Some(FIVE_HOURS))
            } else {
                entry(w, "window", "window", "unknown", mins.map(|m| m * 60.0))
            };
            windows.extend(made);
        }
        if let Some(w) = u.get("weekly") {
            windows.extend(entry(w, "weekly", "week", "weekly", Some(SEVEN_DAYS)));
        }
    }
    let bounds: Vec<&Window> = windows
        .iter()
        .filter(|w| w.id == "five_hour" || w.id == "weekly")
        .collect();
    Some(reading(
        PROVIDER,
        SOURCE,
        now,
        &windows,
        &[("all_models".into(), bounds)],
        extra,
    ))
}

#[async_trait]
impl LocalReading for MuseUsage {
    fn provider(&self) -> &str {
        PROVIDER
    }

    async fn read(&self) -> Value {
        self.reading_for(None, false).await
    }

    async fn read_with_key(&self, key: &str) -> Option<Value> {
        Some(self.read_token(key).await)
    }

    async fn read_account(&self, config_dir: Option<&Path>, grant: bool) -> Option<Value> {
        Some(self.reading_for(config_dir, grant).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_session_and_week() {
        let now = OffsetDateTime::now_utc();
        let t = now.unix_timestamp();
        let v = json!({
            "subs_tier_name": "Pro",
            "is_subs_active": true,
            "subs_usage": {
                "window": {"used_percent": 30, "window_duration_mins": 300, "resets_at": t + 3600},
                "weekly": {"used_percent": 55.5, "resets_at": t + 86_400},
            },
        });
        let r = usage_reading(&v, now).unwrap();
        assert!(crate::quota::valid(&r), "{r}");
        let p = &r["providers"][0];
        assert_eq!(p["plan"], "Pro");
        assert_eq!(p["windows"][0]["id"], "five_hour");
        let row = &p["quotaSemantics"]["effectiveAvailability"][0];
        assert_eq!(row["effectivePercentRemaining"], json!(44.5));
        let idle = usage_reading(&json!({"is_subs_active": false}), now).unwrap();
        assert_eq!(idle["providers"][0]["quotaSemantics"]["status"], "unknown");
        assert!(usage_reading(&json!({}), now).is_none());
    }

    #[tokio::test]
    async fn reads_file_and_key_logins() {
        let tmp = tempfile::tempdir().unwrap();
        let body = br#"{"is_subs_active":true,"subs_usage":{"weekly":{"used_percent":10}}}"#;
        let mut r = MuseUsage::new();
        r.http.curl = support::stub_curl(tmp.path(), &[("meta.ai", 200, body)]);
        r.cache = Cache::none();
        r.keychain.enabled = false;
        r.default_dir = Some(tmp.path().join("muse"));
        let v = r.read().await;
        assert_eq!(v["providers"][0]["state"]["status"], "auth_required");
        std::fs::create_dir_all(tmp.path().join("muse")).unwrap();
        std::fs::write(
            tmp.path().join("muse/auth.json"),
            r#"{"providers":{"meta":{"access_token":"t"}}}"#,
        )
        .unwrap();
        let v = r.read().await;
        assert_eq!(v["providers"][0]["state"]["status"], "fresh", "{v}");
        let v = r.read_with_key("k").await.unwrap();
        assert_eq!(v["providers"][0]["state"]["status"], "fresh", "{v}");
    }
}
