//! Grok's plan credits, read the way quota-axi read them, without it.
//!
//! The login is the Grok CLI's `auth.json` (`GROK_AUTH_JSON`,
//! `GROK_AUTH_PATH`, else `$GROK_HOME/auth.json`, default `~/.grok`): its
//! grok.com session goes to grok.com's `GetGrokCreditsConfig` gRPC-web call,
//! which answers the shared credit percent for the billing period and one
//! percent per product. For the default login, a machine with no Grok CLI
//! session but Pi signed in to xAI with OAuth (`xai` in Pi's `auth.json`) is
//! read with Pi's token instead. A session the server rejects that carries a
//! refresh token is handed back to the CLI (`grok models`, which renews and
//! rewrites the file without spending quota) and read once more.
//!
//! The shared credits window bounds `all_products`; each product window adds
//! its own bound for `product:<id>`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use time::OffsetDateTime;

use super::support::{self, Cache, Http, PiLogin, Request};
use super::windows::{failure, reading, Window};
use super::LocalReading;

pub const PROVIDER: &str = "grok";
const SOURCE: &str = "quarkd: Grok credits";
const URL: &str = "https://grok.com/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig";
const PI_ENTRY: &str = "xai";
const SIGN_IN: &str = "Grok is not signed in here; run `grok login`";
const PRODUCTS: &[(u64, &str, &str)] = &[
    (0, "unspecified", "Other"),
    (1, "api", "API"),
    (2, "grok_build", "Grok Build"),
    (3, "grok_plugins", "Grok Plugins"),
    (4, "chat", "Chat"),
    (5, "imagine", "Imagine"),
    (6, "voice", "Voice"),
];

/// Reads Grok's plan credits for any of its logins.
#[derive(Debug, Clone)]
pub struct GrokUsage {
    pub user_home: Option<PathBuf>,
    /// `GROK_HOME` in quarkd's own environment: the default login.
    pub default_dir: Option<PathBuf>,
    /// An explicit auth file (`GROK_AUTH_JSON` or `GROK_AUTH_PATH`).
    pub auth_file: Option<PathBuf>,
    pub pi_dir: Option<PathBuf>,
    pub http: Http,
    /// The Grok CLI, run to renew a rejected session; `None` never runs it.
    pub grok: Option<String>,
    pub url: String,
    pub cache: Cache,
}

/// One session in `auth.json`.
#[derive(Debug, Clone, PartialEq)]
struct Session {
    key: String,
    refreshable: bool,
}

impl GrokUsage {
    pub fn from_env() -> Self {
        let mut r = Self::new(
            support::user_home(),
            support::env("GROK_HOME").and_then(|d| support::expand_home(&d)),
        );
        r.auth_file = support::env("GROK_AUTH_JSON")
            .or_else(|| support::env("GROK_AUTH_PATH"))
            .and_then(|f| support::expand_home(&f));
        r.pi_dir = support::pi_dir();
        r
    }

    pub fn new(user_home: Option<PathBuf>, default_dir: Option<PathBuf>) -> Self {
        Self {
            user_home,
            default_dir,
            auth_file: None,
            pi_dir: None,
            http: Http::default(),
            grok: Some("grok".into()),
            url: URL.into(),
            cache: Cache::default(),
        }
    }

    fn default_home(&self) -> Option<PathBuf> {
        self.default_dir
            .clone()
            .or_else(|| self.user_home.as_ref().map(|h| h.join(".grok")))
    }

    /// The reading for the login whose `GROK_HOME` is `dir` (`None`: the
    /// default login, which may also be read through Pi's).
    pub async fn reading_for(&self, dir: Option<&Path>) -> Value {
        let file = match dir {
            Some(d) => d.join("auth.json"),
            None => match self
                .auth_file
                .clone()
                .or_else(|| self.default_home().map(|h| h.join("auth.json")))
            {
                Some(f) => f,
                None => return self.fail("auth_required", SIGN_IN),
            },
        };
        let key = format!("grok\n{}", file.display());
        if let Some(v) = self.cache.get(&key) {
            return v;
        }
        let v = self.read_now(&file, dir).await;
        self.cache.put(&key, &v);
        v
    }

    fn fail(&self, status: &str, error: &str) -> Value {
        failure(
            PROVIDER,
            SOURCE,
            OffsetDateTime::now_utc(),
            status,
            error,
            None,
        )
    }

    async fn read_now(&self, file: &Path, dir: Option<&Path>) -> Value {
        let sessions = match support::read_json(file) {
            Ok(Some(v)) => sessions(&v),
            Ok(None) => Vec::new(),
            Err(e) => return self.fail("unavailable", &e),
        };
        let mut last = None;
        for s in &sessions {
            match self.read_with(&s.key).await {
                Ok(v) => return v,
                Err(e) => last = Some(e),
            }
        }
        // A rejected session the CLI can renew: let it, then read again.
        let owned = dir.is_some() || self.auth_file.is_none();
        if owned
            && matches!(last, Some((ref st, _)) if st == "auth_required")
            && sessions.iter().any(|s| s.refreshable)
        {
            if let Some(grok) = &self.grok {
                let home = dir.map(|d| d.to_string_lossy().into_owned());
                let env: Vec<(&str, &str)> = home
                    .as_deref()
                    .map(|h| vec![("GROK_HOME", h)])
                    .unwrap_or_default();
                let argv = [grok.clone(), "models".into()];
                if super::run_with(&argv, None, &env, Duration::from_secs(20))
                    .await
                    .is_ok()
                {
                    if let Ok(Some(v)) = support::read_json(file) {
                        for s in sessions_changed(&v, &sessions) {
                            match self.read_with(&s.key).await {
                                Ok(v) => return v,
                                Err(e) => last = Some(e),
                            }
                        }
                    }
                }
            }
        }
        if dir.is_none() && last.as_ref().is_none_or(|(st, _)| st == "auth_required") {
            match support::pi_login(self.pi_dir.as_deref(), PI_ENTRY) {
                Some(PiLogin::OAuth { access, .. }) => match self.read_with(&access).await {
                    Ok(v) => return v,
                    Err(e) => last = last.or(Some(e)),
                },
                Some(PiLogin::Key(_)) if sessions.is_empty() => {
                    return self.fail(
                        "unavailable",
                        "Pi signs in to xAI with an API key, which has no plan credits",
                    )
                }
                _ => {}
            }
        }
        match last {
            Some((status, error)) => self.fail(&status, &error),
            None => self.fail("auth_required", SIGN_IN),
        }
    }

    async fn read_with(&self, token: &str) -> Result<Value, (String, String)> {
        let req = Request::post(&self.url, vec![0u8; 5])
            .bearer(token)
            .header("Accept", "*/*")
            .header("Content-Type", "application/grpc-web+proto")
            .header("Origin", "https://grok.com")
            .header("Referer", "https://grok.com/?_s=usage")
            .header("x-grpc-web", "1")
            .header("x-user-agent", "connect-es/2.1.1");
        let r = self.http.send(&req).await.map_err(|e| {
            (
                "unavailable".to_string(),
                format!("Grok quota unavailable: {e}"),
            )
        })?;
        if !r.ok() {
            let (s, e) = support::http_failure("Grok", r.status);
            return Err((s.into(), e));
        }
        grpc_status(r.header("grpc-status"))?;
        let payload = grpc_payload(&r.body)?;
        let now = OffsetDateTime::now_utc();
        credits_reading(&payload, now).ok_or_else(|| {
            (
                "unavailable".to_string(),
                "Grok quota response invalid".to_string(),
            )
        })
    }
}

/// The sessions in a Grok `auth.json`, grok.com sessions first, API keys
/// left out (they bill per token and have no plan credits).
fn sessions(v: &Value) -> Vec<Session> {
    let entry = |scope: &str, item: &Value| -> Option<(bool, bool, Session)> {
        let key = support::text(item.get("key"))?;
        let scope = support::text(item.get("scope"))
            .or_else(|| support::text(item.get("url")))
            .or_else(|| support::text(item.get("audience")))
            .unwrap_or_else(|| scope.to_string());
        let lower = scope.to_ascii_lowercase();
        let kind = support::text(item.get("type"))
            .or_else(|| support::text(item.get("kind")))
            .unwrap_or_default()
            .to_ascii_lowercase();
        let host = host_of(&lower);
        let api_key = kind == "api-key"
            || kind == "api_key"
            || lower.contains("api-key")
            || lower.contains("api_key")
            || host == "api.x.ai"
            || host == "api.grok.com";
        let oidc = support::text(item.get("auth_mode"))
            .or_else(|| support::text(item.get("authMode")))
            .is_some_and(|m| m.eq_ignore_ascii_case("oidc"));
        let session = !api_key
            && ((host == "auth.x.ai" && oidc)
                || (host == "accounts.x.ai" && path_of(&lower).starts_with("/sign-in"))
                || host == "grok.com"
                || host == "www.grok.com");
        let refreshable = item.get("refresh_token").is_some() || item.get("refreshToken").is_some();
        Some((session, api_key, Session { key, refreshable }))
    };
    let found: Vec<(bool, bool, Session)> = match v.get("key") {
        Some(_) => entry("", v).into_iter().collect(),
        None => v
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(scope, item)| entry(scope, item))
            .collect(),
    };
    let sessions: Vec<Session> = found
        .iter()
        .filter(|(s, _, _)| *s)
        .map(|(_, _, x)| x.clone())
        .collect();
    if !sessions.is_empty() {
        return sessions;
    }
    found
        .into_iter()
        .filter(|(_, k, _)| !k)
        .map(|(_, _, x)| x)
        .collect()
}

/// The sessions in a rewritten file whose token changed.
fn sessions_changed(v: &Value, before: &[Session]) -> Vec<Session> {
    sessions(v)
        .into_iter()
        .filter(|s| !before.iter().any(|b| b.key == s.key))
        .collect()
}

fn host_of(scope: &str) -> String {
    let s = scope.split("::").next().unwrap_or(scope);
    let s = s.split_once("://").map_or(s, |(_, rest)| rest);
    s.split(['/', '?', '#']).next().unwrap_or("").to_string()
}

fn path_of(scope: &str) -> String {
    let s = scope.split("::").next().unwrap_or(scope);
    let s = s.split_once("://").map_or(s, |(_, rest)| rest);
    s.find('/').map_or(String::new(), |i| s[i..].to_string())
}

fn invalid() -> (String, String) {
    (
        "unavailable".to_string(),
        "Grok quota response invalid".to_string(),
    )
}

fn grpc_status(v: Option<&str>) -> Result<(), (String, String)> {
    match v.map(str::trim) {
        None | Some("0") => Ok(()),
        Some("16") | Some("7") => Err(("auth_required".into(), SIGN_IN.into())),
        Some("8") => Err((
            "rate_limited".into(),
            "Grok quota endpoint rate limited".into(),
        )),
        Some(_) => Err(("unavailable".into(), "Grok quota unavailable".into())),
    }
}

/// The one data frame of a gRPC-web body (or the body itself when it is
/// not framed), with any trailer frame's status checked.
fn grpc_payload(bytes: &[u8]) -> Result<Vec<u8>, (String, String)> {
    let first = *bytes.first().ok_or_else(invalid)?;
    if !(first == 0 || first == 1 || first & 0x80 != 0) {
        return Ok(bytes.to_vec());
    }
    let mut data = None;
    let mut i = 0;
    while i < bytes.len() {
        if i + 5 > bytes.len() {
            return Err(invalid());
        }
        let flags = bytes[i];
        let len =
            u32::from_be_bytes([bytes[i + 1], bytes[i + 2], bytes[i + 3], bytes[i + 4]]) as usize;
        let (start, end) = (i + 5, i + 5 + len);
        if end > bytes.len() {
            return Err(invalid());
        }
        let frame = &bytes[start..end];
        match flags {
            0x80 => {
                let text = String::from_utf8_lossy(frame);
                let status = text.lines().find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.trim()
                        .eq_ignore_ascii_case("grpc-status")
                        .then(|| v.trim().to_string())
                });
                grpc_status(status.as_deref())?;
            }
            0 if data.is_none() => data = Some(frame.to_vec()),
            _ => return Err(invalid()),
        }
        i = end;
    }
    data.ok_or_else(invalid)
}

/// Protobuf fields: `(field, wire, value)`, where a varint's value is its
/// little-endian bytes and the rest are raw.
mod proto {
    pub enum Val<'a> {
        Varint(u64),
        Fixed32([u8; 4]),
        Bytes(&'a [u8]),
        Fixed64,
    }

    fn varint(b: &[u8], i: &mut usize) -> Option<u64> {
        let mut v = 0u64;
        for n in 0..10 {
            let byte = *b.get(*i)?;
            *i += 1;
            v |= u64::from(byte & 0x7f) << (n * 7);
            if byte & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }

    pub fn scan(b: &[u8]) -> Option<Vec<(u64, Val<'_>)>> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            let key = varint(b, &mut i)?;
            let field = key >> 3;
            if field == 0 {
                return None;
            }
            let v = match key & 7 {
                0 => Val::Varint(varint(b, &mut i)?),
                1 => {
                    i += 8;
                    (i <= b.len()).then_some(Val::Fixed64)?
                }
                5 => {
                    let x: [u8; 4] = b.get(i..i + 4)?.try_into().ok()?;
                    i += 4;
                    Val::Fixed32(x)
                }
                2 => {
                    let len = varint(b, &mut i)? as usize;
                    let x = b.get(i..i.checked_add(len)?)?;
                    i += len;
                    Val::Bytes(x)
                }
                _ => return None,
            };
            out.push((field, v));
        }
        Some(out)
    }

    pub fn varint_at(f: &[(u64, Val<'_>)], n: u64) -> Option<u64> {
        f.iter().find_map(|(k, v)| match v {
            Val::Varint(x) if *k == n => Some(*x),
            _ => None,
        })
    }

    pub fn float_at(f: &[(u64, Val<'_>)], n: u64) -> Option<f64> {
        f.iter().find_map(|(k, v)| match v {
            Val::Fixed32(x) if *k == n => Some(f64::from(f32::from_le_bytes(*x))),
            _ => None,
        })
    }

    pub fn messages_at<'a>(f: &[(u64, Val<'a>)], n: u64) -> Vec<&'a [u8]> {
        f.iter()
            .filter_map(|(k, v)| match v {
                Val::Bytes(x) if *k == n => Some(*x),
                _ => None,
            })
            .collect()
    }

    pub fn message_at<'a>(f: &[(u64, Val<'a>)], n: u64) -> Option<Vec<(u64, Val<'a>)>> {
        scan(messages_at(f, n).first()?)
    }
}

/// The reading for a `GetGrokCreditsConfig` answer.
fn credits_reading(payload: &[u8], now: OffsetDateTime) -> Option<Value> {
    use proto::*;
    let response = scan(payload)?;
    let config = message_at(&response, 1)?;
    let period = message_at(&config, 8);
    let ptype = period.as_ref().and_then(|p| varint_at(p, 1));
    let stamp = |n| {
        let ts = message_at(period.as_ref()?, n)?;
        OffsetDateTime::from_unix_timestamp(varint_at(&ts, 1)? as i64).ok()
    };
    let (start, reset) = (stamp(2), stamp(3));
    let (kind, label) = match ptype {
        Some(2) => ("weekly", "week"),
        Some(1) => ("monthly", "month"),
        _ => ("credits", "credits"),
    };
    let valid_period = kind != "credits" && start.is_some() && reset.is_some();
    let length = match (start, reset) {
        (Some(s), Some(r)) if r > s => Some((r - s).as_seconds_f64()),
        _ => None,
    };
    let make = |id: &str, label: &str, used: f64| {
        Window::new(id, label, kind, used)
            .resets(reset)
            .lasting(length)
    };
    let mut windows = Vec::new();
    let shared = float_at(&config, 1);
    if shared.is_some() || valid_period {
        windows.push(make("credits", label, shared.unwrap_or(0.0)));
    }
    for p in messages_at(&config, 7) {
        let p = scan(p)?;
        let used = float_at(&p, 2);
        if used.is_none() && !valid_period {
            continue;
        }
        let n = varint_at(&p, 1).unwrap_or(0);
        let (id, label) = PRODUCTS
            .iter()
            .find(|(k, _, _)| *k == n)
            .map(|(_, id, l)| (id.to_string(), l.to_string()))
            .unwrap_or_else(|| (format!("unknown_{n}"), format!("Product {n}")));
        windows.push(make(&format!("product:{id}"), &label, used.unwrap_or(0.0)));
    }
    if windows.is_empty() {
        return None;
    }
    let mut extra = Map::new();
    if let Some(balance) = message_at(&config, 12).and_then(|p| varint_at(&p, 1)) {
        extra.insert(
            "credits".into(),
            json!({"remaining": balance, "unit": "credits"}),
        );
    }
    let shared: Vec<&Window> = windows.iter().filter(|w| w.id == "credits").collect();
    let mut scopes = Vec::new();
    if !shared.is_empty() {
        scopes.push(("all_products".to_string(), shared.clone()));
    }
    for w in windows.iter().filter(|w| w.id.starts_with("product:")) {
        let mut b = shared.clone();
        b.push(w);
        scopes.push((w.id.clone(), b));
    }
    Some(reading(PROVIDER, SOURCE, now, &windows, &scopes, extra))
}

#[async_trait]
impl LocalReading for GrokUsage {
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

    fn varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }

    fn msg(field: u64, body: &[u8], out: &mut Vec<u8>) {
        varint(field << 3 | 2, out);
        varint(body.len() as u64, out);
        out.extend_from_slice(body);
    }

    fn float(field: u64, v: f32, out: &mut Vec<u8>) {
        varint(field << 3 | 5, out);
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn vint(field: u64, v: u64, out: &mut Vec<u8>) {
        varint(field << 3, out);
        varint(v, out);
    }

    /// A weekly period with 25% of the shared credits and 40% of Grok
    /// Build used, framed as gRPC-web with an OK trailer.
    pub(super) fn sample(now: OffsetDateTime) -> Vec<u8> {
        let (start, end) = (
            now.unix_timestamp() - 86_400,
            now.unix_timestamp() + 6 * 86_400,
        );
        let ts = |s: i64| {
            let mut t = Vec::new();
            vint(1, s as u64, &mut t);
            t
        };
        let mut period = Vec::new();
        vint(1, 2, &mut period);
        msg(2, &ts(start), &mut period);
        msg(3, &ts(end), &mut period);
        let mut product = Vec::new();
        vint(1, 2, &mut product);
        float(2, 40.0, &mut product);
        let mut config = Vec::new();
        float(1, 25.0, &mut config);
        msg(7, &product, &mut config);
        msg(8, &period, &mut config);
        let mut response = Vec::new();
        msg(1, &config, &mut response);
        let mut framed = vec![0];
        framed.extend_from_slice(&(response.len() as u32).to_be_bytes());
        framed.extend_from_slice(&response);
        let trailer = b"grpc-status: 0\r\n";
        framed.push(0x80);
        framed.extend_from_slice(&(trailer.len() as u32).to_be_bytes());
        framed.extend_from_slice(trailer);
        framed
    }

    #[test]
    fn reads_credits_and_products() {
        let now = OffsetDateTime::now_utc();
        let payload = grpc_payload(&sample(now)).unwrap();
        let v = credits_reading(&payload, now).unwrap();
        assert!(crate::quota::valid(&v), "{v}");
        let p = &v["providers"][0];
        assert_eq!(p["windows"][0]["id"], "credits");
        assert_eq!(p["windows"][0]["kind"], "weekly");
        assert_eq!(p["windows"][1]["id"], "product:grok_build");
        let rows = p["quotaSemantics"]["effectiveAvailability"]
            .as_array()
            .unwrap();
        assert_eq!(rows[0]["scope"], "all_products");
        assert_eq!(rows[0]["effectivePercentRemaining"], json!(75));
        assert_eq!(rows[1]["scope"], "product:grok_build");
        assert_eq!(rows[1]["effectivePercentRemaining"], json!(60));
    }

    #[test]
    fn grpc_errors_name_the_cause() {
        let mut framed = vec![0x80];
        let t = b"grpc-status: 16\r\n";
        framed.extend_from_slice(&(t.len() as u32).to_be_bytes());
        framed.extend_from_slice(t);
        assert_eq!(grpc_payload(&framed).unwrap_err().0, "auth_required");
        assert_eq!(grpc_status(Some("8")).unwrap_err().0, "rate_limited");
    }

    #[test]
    fn prefers_grok_sessions_over_api_keys() {
        let v = json!({
            "https://api.x.ai": {"key": "api", "type": "api_key"},
            "https://auth.x.ai::oidc": {"key": "oidc", "auth_mode": "oidc", "refresh_token": "r"},
            "https://accounts.x.ai/sign-in": {"key": "web"},
        });
        let s = sessions(&v);
        assert_eq!(s.len(), 2);
        assert!(s.iter().all(|s| s.key != "api"));
        assert!(s.iter().any(|s| s.key == "oidc" && s.refreshable));
        assert!(sessions(&json!({"x": {"key": "api", "type": "api-key"}})).is_empty());
        assert_eq!(sessions(&json!({"key": "k"}))[0].key, "k");
    }

    #[tokio::test]
    async fn reads_through_curl() {
        let tmp = tempfile::tempdir().unwrap();
        let now = OffsetDateTime::now_utc();
        let curl = support::stub_curl(tmp.path(), &[("grok.com", 200, &sample(now))]);
        let home = tmp.path().join("grok");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("auth.json"),
            r#"{"https://accounts.x.ai/sign-in": {"key": "tok"}}"#,
        )
        .unwrap();
        let mut r = GrokUsage::new(None, Some(home.clone()));
        r.http.curl = curl;
        r.cache = Cache::none();
        r.grok = None;
        let v = r.read().await;
        assert_eq!(v["providers"][0]["state"]["status"], "fresh", "{v}");
        let missing = r.reading_for(Some(&tmp.path().join("none"))).await;
        assert_eq!(missing["providers"][0]["state"]["status"], "auth_required");
    }
}
