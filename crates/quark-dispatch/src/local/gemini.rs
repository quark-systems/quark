//! Gemini CLI's daily model quota (not a quota-axi provider).
//!
//! The login is Gemini CLI's "Sign in with Google" token in
//! `~/.gemini/oauth_creds.json`. Its Code Assist project (from
//! `GOOGLE_CLOUD_PROJECT`, else `loadCodeAssist`) goes to Code Assist's
//! `retrieveUserQuota`, which answers, per model, the fraction of the day's
//! requests left and when it resets. An expired token is renewed in memory
//! only (never written back) with the OAuth client the installed Gemini CLI
//! itself uses, found in its `oauth2.js`; with no Gemini CLI installed the
//! reading waits for the CLI's next run to renew it. A Gemini API key has
//! no plan quota to read.
//!
//! Each model's windows bound `model:<id>`.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use time::OffsetDateTime;

use super::support::{self, num, Cache, Http, Request};
use super::windows::{failure, reading, Window};
use super::{parse_time, LocalReading};

pub const PROVIDER: &str = "gemini";
const SOURCE: &str = "quarkd: Gemini Code Assist quota";
const API: &str = "https://cloudcode-pa.googleapis.com/v1internal";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const DAY: f64 = 86_400.0;
const SIGN_IN: &str = "Gemini CLI is not signed in with Google here; run `gemini` and sign in";

/// Reads Gemini CLI's daily model quota.
#[derive(Debug, Clone)]
pub struct GeminiUsage {
    pub user_home: Option<PathBuf>,
    /// `GOOGLE_CLOUD_PROJECT` (or `GOOGLE_CLOUD_PROJECT_ID`).
    pub project: Option<String>,
    /// The Gemini CLI, whose install holds its OAuth client.
    pub gemini: Option<String>,
    pub http: Http,
    pub api: String,
    pub token_url: String,
    pub cache: Cache,
}

impl GeminiUsage {
    pub fn from_env() -> Self {
        Self {
            user_home: support::user_home(),
            project: support::env("GOOGLE_CLOUD_PROJECT")
                .or_else(|| support::env("GOOGLE_CLOUD_PROJECT_ID")),
            ..Self::new()
        }
    }

    pub fn new() -> Self {
        Self {
            user_home: None,
            project: None,
            gemini: Some("gemini".into()),
            http: Http::default(),
            api: API.into(),
            token_url: TOKEN_URL.into(),
            cache: Cache::default(),
        }
    }

    /// The reading for the login in `dir` (`None`: `~/.gemini`).
    pub async fn reading_for(&self, dir: Option<&Path>) -> Value {
        let dir = dir
            .map(Path::to_path_buf)
            .or_else(|| self.user_home.as_ref().map(|h| h.join(".gemini")));
        let key = format!(
            "gemini\n{}",
            dir.as_deref().unwrap_or(Path::new("")).display()
        );
        if let Some(v) = self.cache.get(&key) {
            return v;
        }
        let v = self.read_now(dir.as_deref()).await;
        self.cache.put(&key, &v);
        v
    }

    async fn read_now(&self, dir: Option<&Path>) -> Value {
        let now = OffsetDateTime::now_utc();
        let fail = |status: &str, error: &str| failure(PROVIDER, SOURCE, now, status, error, None);
        let creds = match dir.map(|d| support::read_json(&d.join("oauth_creds.json"))) {
            Some(Ok(Some(v))) => v,
            Some(Err(e)) => return fail("unavailable", &e),
            _ => return fail("auth_required", SIGN_IN),
        };
        let Some(mut token) = support::text(creds.get("access_token")) else {
            return fail("auth_required", SIGN_IN);
        };
        let expiry_ms = num(creds.get("expiry_date"));
        let now_ms = now.unix_timestamp() as f64 * 1000.0;
        if expiry_ms.is_some_and(|e| e <= now_ms + 60_000.0) {
            let renewed = match support::text(creds.get("refresh_token")) {
                Some(rt) => self.renew(&rt).await,
                None => None,
            };
            match renewed {
                Some(t) => token = t,
                None => {
                    return fail(
                        "auth_required",
                        "Gemini CLI's sign-in expired; it renews the next time `gemini` runs",
                    )
                }
            }
        }
        let call = |method: &str, body: Value| {
            let req = Request::post(format!("{}:{method}", self.api), body.to_string())
                .bearer(&token)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json")
                .header("User-Agent", "quarkd");
            async move {
                match self.http.send(&req).await {
                    Ok(r) if r.ok() => r.json().ok_or_else(|| {
                        ("unavailable", "Gemini quota response invalid".to_string())
                    }),
                    Ok(r) => Err(support::http_failure("Gemini", r.status)),
                    Err(e) => Err(("unavailable", format!("Gemini quota unavailable: {e}"))),
                }
            }
        };
        let mut extra = Map::new();
        let project = match &self.project {
            Some(p) => p.clone(),
            None => {
                let meta = json!({"metadata": {
                    "ideType": "IDE_UNSPECIFIED",
                    "platform": "PLATFORM_UNSPECIFIED",
                    "pluginType": "GEMINI",
                }});
                let v = match call("loadCodeAssist", meta).await {
                    Ok(v) => v,
                    Err((s, e)) => return fail(s, &e),
                };
                if let Some(tier) = v
                    .get("paidTier")
                    .or_else(|| v.get("currentTier"))
                    .and_then(|t| support::text(t.get("name")))
                {
                    extra.insert("plan".into(), json!(tier));
                }
                let p = v.get("cloudaicompanionProject");
                match p.and_then(|p| support::text(Some(p)).or_else(|| support::text(p.get("id"))))
                {
                    Some(p) => p,
                    None => {
                        return fail(
                            "unavailable",
                            "Gemini CLI has no Code Assist project; set GOOGLE_CLOUD_PROJECT",
                        )
                    }
                }
            }
        };
        match call("retrieveUserQuota", json!({"project": project})).await {
            Ok(v) => quota_reading(&v, extra, now)
                .unwrap_or_else(|| fail("unavailable", "Gemini reported no quota buckets")),
            Err((s, e)) => fail(s, &e),
        }
    }

    /// A fresh access token from `refresh`, with the installed Gemini CLI's
    /// OAuth client.
    async fn renew(&self, refresh: &str) -> Option<String> {
        let (id, secret) = oauth_client(self.gemini.as_deref()?)?;
        let body = format!(
            "grant_type=refresh_token&refresh_token={}&client_id={}&client_secret={}",
            form(refresh),
            form(&id),
            form(&secret)
        );
        let req = Request::post(&self.token_url, body)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json");
        let r = self.http.send(&req).await.ok().filter(|r| r.ok())?;
        support::text(r.json()?.get("access_token"))
    }
}

impl Default for GeminiUsage {
    fn default() -> Self {
        Self::new()
    }
}

fn form(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The OAuth client id and secret in the installed Gemini CLI's
/// `code_assist/oauth2.js`, found from the `gemini` command on `PATH`.
fn oauth_client(gemini: &str) -> Option<(String, String)> {
    let exe = if gemini.contains('/') {
        PathBuf::from(gemini)
    } else {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|d| d.join(gemini))
            .find(|p| p.is_file())?
    };
    let exe = std::fs::canonicalize(exe).ok()?;
    let rel = Path::new("@google/gemini-cli-core/dist/src/code_assist/oauth2.js");
    for dir in exe.ancestors().take(8) {
        for c in [
            dir.join("node_modules").join(rel),
            dir.join("lib/node_modules/@google/gemini-cli/node_modules")
                .join(rel),
            dir.join("lib/node_modules").join(rel),
        ] {
            if let Ok(text) = std::fs::read_to_string(&c) {
                return client_in(&text);
            }
        }
    }
    // A bundled CLI keeps the constants in its own script.
    client_in(&std::fs::read_to_string(&exe).ok()?)
}

fn client_in(text: &str) -> Option<(String, String)> {
    let constant = |name: &str| -> Option<String> {
        let at = text.find(name)? + name.len();
        let rest = text[at..].trim_start().strip_prefix('=')?.trim_start();
        let quote = rest.chars().next().filter(|c| *c == '\'' || *c == '"')?;
        let rest = &rest[1..];
        Some(rest[..rest.find(quote)?].to_string())
    };
    Some((
        constant("OAUTH_CLIENT_ID")?,
        constant("OAUTH_CLIENT_SECRET")?,
    ))
}

fn quota_reading(v: &Value, extra: Map<String, Value>, now: OffsetDateTime) -> Option<Value> {
    let mut windows = Vec::new();
    for b in v.get("buckets")?.as_array()? {
        let Some(model) = support::text(b.get("modelId")) else {
            continue;
        };
        let Some(left) = num(b.get("remainingFraction")) else {
            continue;
        };
        let tokens = support::text(b.get("tokenType")).unwrap_or_default();
        let (id, label) = if tokens.is_empty() || tokens == "REQUESTS" {
            (model.clone(), model.clone())
        } else {
            let t = tokens.to_ascii_lowercase();
            (
                format!("{model}:{t}"),
                format!("{model} {}", t.replace('_', " ")),
            )
        };
        let reset = b.get("resetTime").and_then(parse_time);
        windows.push((
            model,
            Window::new(&id, &label, "daily", (1.0 - left.clamp(0.0, 1.0)) * 100.0)
                .resets(reset)
                .lasting(Some(DAY)),
        ));
    }
    if windows.is_empty() {
        return None;
    }
    let mut models: Vec<&String> = Vec::new();
    for (m, _) in &windows {
        if !models.contains(&m) {
            models.push(m);
        }
    }
    let scopes: Vec<(String, Vec<&Window>)> = models
        .iter()
        .map(|m| {
            (
                format!("model:{m}"),
                windows
                    .iter()
                    .filter(|(x, _)| x == *m)
                    .map(|(_, w)| w)
                    .collect(),
            )
        })
        .collect();
    let ws: Vec<Window> = windows.iter().map(|(_, w)| w.clone()).collect();
    Some(reading(PROVIDER, SOURCE, now, &ws, &scopes, extra))
}

#[async_trait]
impl LocalReading for GeminiUsage {
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

    #[test]
    fn reads_model_buckets() {
        let now = OffsetDateTime::now_utc();
        let v = json!({"buckets": [
            {"modelId": "gemini-2.5-pro", "tokenType": "REQUESTS", "remainingFraction": 0.75,
             "resetTime": super::super::utc_iso(now + time::Duration::hours(5))},
            {"modelId": "gemini-2.5-flash", "remainingFraction": 1},
        ]});
        let r = quota_reading(&v, Map::new(), now).unwrap();
        assert!(crate::quota::valid(&r), "{r}");
        let rows = r["providers"][0]["quotaSemantics"]["effectiveAvailability"]
            .as_array()
            .unwrap();
        assert_eq!(rows[0]["scope"], "model:gemini-2.5-pro");
        assert_eq!(rows[0]["effectivePercentRemaining"], json!(75));
        assert_eq!(rows[1]["scope"], "model:gemini-2.5-flash");
        assert!(quota_reading(&json!({"buckets": []}), Map::new(), now).is_none());
    }

    #[test]
    fn finds_the_cli_oauth_client() {
        let js = "const OAUTH_CLIENT_ID = 'id-1.apps';\nconst OAUTH_CLIENT_SECRET = \"s3\";";
        assert_eq!(client_in(js), Some(("id-1.apps".into(), "s3".into())));
        assert_eq!(client_in("nothing"), None);
        assert_eq!(form("a b/c"), "a%20b%2Fc");
    }

    #[tokio::test]
    async fn asks_code_assist_for_the_project() {
        let tmp = tempfile::tempdir().unwrap();
        let curl = support::stub_curl(
            tmp.path(),
            &[
                (":loadCodeAssist", 200, br#"{"cloudaicompanionProject":"p1","currentTier":{"name":"Gemini Code Assist for individuals"}}"#),
                (":retrieveUserQuota", 200, br#"{"buckets":[{"modelId":"gemini-2.5-pro","remainingFraction":0.5}]}"#),
            ],
        );
        let home = tmp.path().join("g");
        std::fs::create_dir_all(&home).unwrap();
        let mut r = GeminiUsage::new();
        r.http.curl = curl;
        r.cache = Cache::none();
        r.gemini = None;
        let v = r.reading_for(Some(&home)).await;
        assert_eq!(v["providers"][0]["state"]["status"], "auth_required");
        let later = (OffsetDateTime::now_utc().unix_timestamp() + 3600) * 1000;
        std::fs::write(
            home.join("oauth_creds.json"),
            format!(r#"{{"access_token":"t","expiry_date":{later}}}"#),
        )
        .unwrap();
        let v = r.reading_for(Some(&home)).await;
        assert_eq!(v["providers"][0]["state"]["status"], "fresh", "{v}");
        assert_eq!(
            v["providers"][0]["plan"],
            "Gemini Code Assist for individuals"
        );
        std::fs::write(
            home.join("oauth_creds.json"),
            r#"{"access_token":"t","refresh_token":"r","expiry_date":1}"#,
        )
        .unwrap();
        let v = r.reading_for(Some(&home)).await;
        assert_eq!(v["providers"][0]["state"]["status"], "auth_required");
    }
}
