//! What the providers' readers share: HTTP through curl (tokens never on
//! argv), the macOS Keychain through `security`, credential files, Pi's
//! logins, and a short cache so readers called together make one request.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::AsyncWriteExt;

/// One HTTP request.
#[derive(Debug, Clone, Default)]
pub struct Request {
    pub method: &'static str,
    pub url: String,
    /// Sent as `Authorization: Bearer <token>` through curl's stdin.
    pub bearer: Option<String>,
    /// Any other header sent on stdin rather than argv (a cookie, an API
    /// key header).
    pub secret_headers: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

impl Request {
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            method: "GET",
            url: url.into(),
            ..Default::default()
        }
    }

    pub fn post(url: impl Into<String>, body: impl Into<Vec<u8>>) -> Self {
        Self {
            method: "POST",
            url: url.into(),
            body: Some(body.into()),
            ..Default::default()
        }
    }

    pub fn bearer(mut self, token: &str) -> Self {
        self.bearer = Some(token.to_string());
        self
    }

    pub fn header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.into(), v.into()));
        self
    }

    pub fn secret_header(mut self, k: &str, v: &str) -> Self {
        self.secret_headers.push((k.into(), v.into()));
        self
    }
}

/// An HTTP answer.
#[derive(Debug, Clone, Default)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
    /// Lower-cased names.
    pub headers: Vec<(String, String)>,
}

impl Response {
    pub fn json(&self) -> Option<Value> {
        serde_json::from_slice(&self.body).ok()
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// HTTP through curl.
#[derive(Debug, Clone)]
pub struct Http {
    pub curl: String,
    pub timeout: Duration,
}

impl Default for Http {
    fn default() -> Self {
        Self {
            curl: "curl".into(),
            timeout: Duration::from_secs(20),
        }
    }
}

/// A config-file value for curl: quoted, with `"` and `\` escaped and
/// control characters dropped.
fn quoted(s: &str) -> String {
    let clean: String = s.chars().filter(|c| !c.is_control()).collect();
    format!("\"{}\"", clean.replace('\\', "\\\\").replace('"', "\\\""))
}

fn scratch_dir() -> std::io::Result<PathBuf> {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "quarkd-http-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

impl Http {
    pub async fn send(&self, req: &Request) -> Result<Response, String> {
        let dir = scratch_dir().map_err(|e| format!("scratch dir: {e}"))?;
        let out = self.send_in(&dir, req).await;
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    async fn send_in(&self, dir: &Path, req: &Request) -> Result<Response, String> {
        let body_out = dir.join("body");
        let head_out = dir.join("head");
        let mut cfg = String::new();
        if let Some(t) = &req.bearer {
            cfg.push_str(&format!(
                "header = {}\n",
                quoted(&format!("Authorization: Bearer {t}"))
            ));
        }
        for (k, v) in &req.secret_headers {
            cfg.push_str(&format!("header = {}\n", quoted(&format!("{k}: {v}"))));
        }
        let mut cmd = tokio::process::Command::new(&self.curl);
        cmd.args(["-sS", "--max-time"])
            .arg(self.timeout.as_secs().max(1).to_string())
            .args(["-X", req.method, "-o"])
            .arg(&body_out)
            .arg("-D")
            .arg(&head_out)
            .args(["-w", "%{http_code}"]);
        for (k, v) in &req.headers {
            cmd.arg("-H").arg(format!("{k}: {v}"));
        }
        if let Some(b) = &req.body {
            let path = dir.join("req");
            std::fs::write(&path, b).map_err(|e| e.to_string())?;
            cmd.arg("--data-binary").arg(format!("@{}", path.display()));
        }
        let mut child = cmd
            .args(["-K", "-"])
            .arg(&req.url)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("{}: {e}", self.curl))?;
        let mut stdin = child.stdin.take().ok_or("curl stdin")?;
        stdin
            .write_all(cfg.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        drop(stdin);
        let out = tokio::time::timeout(
            self.timeout + Duration::from_secs(5),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| format!("no answer within {}s", self.timeout.as_secs()))?
        .map_err(|e| e.to_string())?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let line = err.lines().next().unwrap_or("").trim().to_string();
            return Err(if line.is_empty() {
                format!("curl exited {}", out.status)
            } else {
                line
            });
        }
        let status = String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .unwrap_or(0);
        let body = std::fs::read(&body_out).unwrap_or_default();
        let headers = std::fs::read_to_string(&head_out)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let (k, v) = l.split_once(':')?;
                Some((k.trim().to_ascii_lowercase(), v.trim().to_string()))
            })
            .collect();
        Ok(Response {
            status,
            body,
            headers,
        })
    }
}

/// `security`'s exit status for an item that is not there.
const ITEM_NOT_FOUND: i32 = 44;

/// The macOS Keychain, read with `security find-generic-password`.
///
/// An item whose read times out (an unanswered prompt) or is refused is
/// remembered as withheld, and background reads stop asking for it until a
/// granting read (one the person is there to answer) succeeds, so quarkd
/// never keeps raising dialogs.
#[derive(Debug, Clone)]
pub struct Keychain {
    pub enabled: bool,
    pub security: String,
    pub timeout: Duration,
    pub grant_timeout: Duration,
    withheld: Arc<Mutex<HashSet<String>>>,
}

impl Default for Keychain {
    fn default() -> Self {
        static WITHHELD: OnceLock<Arc<Mutex<HashSet<String>>>> = OnceLock::new();
        Self {
            enabled: cfg!(target_os = "macos"),
            security: "security".into(),
            timeout: Duration::from_secs(10),
            grant_timeout: Duration::from_secs(90),
            withheld: WITHHELD.get_or_init(Default::default).clone(),
        }
    }
}

/// A Keychain item that was withheld: its read timed out or was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Withheld;

/// The message for an item the Keychain withheld.
pub fn withheld_message(what: &str) -> String {
    format!("Keychain access needed: allow Quark to read {what} (Accounts, Allow Keychain access)")
}

impl Keychain {
    /// A Keychain with its own memory, for tests.
    pub fn isolated(mut self) -> Self {
        self.withheld = Default::default();
        self
    }

    /// The item's secret, `None` when there is no such item (or no
    /// Keychain here), or [`Withheld`].
    pub async fn read(
        &self,
        service: &str,
        account: Option<&str>,
        grant: bool,
    ) -> Result<Option<String>, Withheld> {
        if !self.enabled {
            return Ok(None);
        }
        let key = format!("{service}\n{}", account.unwrap_or(""));
        if !grant && self.withheld.lock().unwrap().contains(&key) {
            return Err(Withheld);
        }
        let mut cmd = tokio::process::Command::new(&self.security);
        cmd.arg("find-generic-password");
        if let Some(a) = account {
            cmd.args(["-a", a]);
        }
        cmd.args(["-w", "-s", service])
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        let timeout = if grant {
            self.grant_timeout
        } else {
            self.timeout
        };
        let out = tokio::time::timeout(timeout, cmd.output()).await;
        let mark = |withhold: bool| {
            let mut w = self.withheld.lock().unwrap();
            if withhold {
                w.insert(key.clone());
            } else {
                w.remove(&key);
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
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            _ => {
                mark(true);
                Err(Withheld)
            }
        }
    }
}

/// Readings by key, with when each was taken.
type Readings = Arc<Mutex<HashMap<String, (Instant, Value)>>>;

/// A short-lived cache of readings, shared by every reader of the process.
#[derive(Debug, Clone)]
pub struct Cache {
    pub ttl: Duration,
    map: Readings,
}

impl Default for Cache {
    fn default() -> Self {
        static MAP: OnceLock<Readings> = OnceLock::new();
        Self {
            ttl: Duration::from_secs(45),
            map: MAP.get_or_init(Default::default).clone(),
        }
    }
}

impl Cache {
    /// The shared cache, keeping readings for `ttl`.
    pub fn lasting(ttl: Duration) -> Self {
        Self {
            ttl,
            ..Self::default()
        }
    }

    /// No caching, for tests.
    pub fn none() -> Self {
        Self {
            ttl: Duration::ZERO,
            map: Default::default(),
        }
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        let map = self.map.lock().unwrap();
        let (at, v) = map.get(key)?;
        (at.elapsed() < self.ttl).then(|| v.clone())
    }

    pub fn put(&self, key: &str, v: &Value) {
        self.map
            .lock()
            .unwrap()
            .insert(key.to_string(), (Instant::now(), v.clone()));
    }
}

/// A JSON file: `Ok(None)` when it is not there.
pub fn read_json(path: &Path) -> Result<Option<Value>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|_| format!("{} is not JSON", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// A trimmed, non-empty string.
pub fn text(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// A finite number, also from a numeric string.
pub fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64().filter(|f| f.is_finite()),
        Value::String(s) => s.trim().parse::<f64>().ok().filter(|f| f.is_finite()),
        _ => None,
    }
}

/// The first of `keys` present in `v`.
pub fn pick<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| v.get(*k).filter(|x| !x.is_null()))
}

/// The person's home.
pub fn user_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// An environment variable that is set and not blank.
pub fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// `XDG_CONFIG_HOME`, else `~/.config`.
pub fn config_home() -> Option<PathBuf> {
    env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| user_home().map(|h| h.join(".config")))
}

/// Pi's agent directory (`PI_CODING_AGENT_DIR`, else `~/.pi/agent`).
pub fn pi_dir() -> Option<PathBuf> {
    let home = user_home();
    match env("PI_CODING_AGENT_DIR") {
        Some(d) if d == "~" => home,
        Some(d) => match d.strip_prefix("~/") {
            Some(rest) => home.map(|h| h.join(rest)),
            None => Some(PathBuf::from(d)),
        },
        None => home.map(|h| h.join(".pi").join("agent")),
    }
}

/// A login Pi keeps for a provider in its `auth.json`.
#[derive(Debug, Clone, PartialEq)]
pub enum PiLogin {
    OAuth {
        access: String,
        account_id: Option<String>,
        expired: bool,
    },
    Key(String),
}

/// Pi's login for `entry` (such as `openai-codex`, `kimi-coding`, `xai`).
pub fn pi_login(dir: Option<&Path>, entry: &str) -> Option<PiLogin> {
    let v = read_json(&dir?.join("auth.json")).ok()??;
    let e = v.get(entry)?;
    match e.get("type").and_then(Value::as_str) {
        Some("oauth") => {
            let expires = num(e.get("expires"));
            let now_ms = time::OffsetDateTime::now_utc().unix_timestamp() as f64 * 1000.0;
            Some(PiLogin::OAuth {
                access: text(e.get("access"))?,
                account_id: text(e.get("accountId")),
                expired: expires.is_some_and(|x| x <= now_ms),
            })
        }
        Some("api_key") => text(e.get("key")).map(PiLogin::Key),
        _ => None,
    }
}

/// An OpenCode login (`~/.local/share/opencode/auth.json`) for a
/// provider: an API key or an OAuth access token.
pub fn opencode_login(entry: &str) -> Option<String> {
    let dir = env("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| user_home().map(|h| h.join(".local").join("share")))?;
    let v = read_json(&dir.join("opencode").join("auth.json")).ok()??;
    let e = v.get(entry)?;
    text(e.get("key")).or_else(|| text(e.get("access")))
}

/// A failed HTTP answer as a reading's state: `(status, error)`.
pub fn http_failure(label: &str, status: u16) -> (&'static str, String) {
    match status {
        401 | 403 => (
            "auth_required",
            format!("{label} rejected the sign-in; sign in again"),
        ),
        429 => (
            "rate_limited",
            format!("{label} quota endpoint rate limited"),
        ),
        _ => (
            "unavailable",
            format!("{label} quota unavailable (HTTP {status})"),
        ),
    }
}

/// `path` with a leading `~/` (or a lone `~`) taken from the person's home.
pub fn expand_home(path: &str) -> Option<PathBuf> {
    match path {
        "~" => user_home(),
        _ => match path.strip_prefix("~/") {
            Some(rest) => user_home().map(|h| h.join(rest)),
            None => Some(PathBuf::from(path)),
        },
    }
}

/// A stand-in curl for tests: each request whose URL contains a route's
/// pattern gets that route's status and body (the first match wins), and
/// any other gets a 404.
#[cfg(test)]
pub fn stub_curl(dir: &Path, routes: &[(&str, u16, &[u8])]) -> String {
    use std::fmt::Write;
    let mut script = String::from(
        "#!/bin/sh\nfor a in \"$@\"; do url=$a; done\n\
         while [ $# -gt 0 ]; do case \"$1\" in -o) o=$2; shift;; -D) d=$2; shift;; esac; shift; done\n\
         cat >/dev/null\nprintf 'HTTP/1.1 200 OK\\r\\n' > \"$d\"\ncase \"$url\" in\n",
    );
    for (i, (pattern, status, body)) in routes.iter().enumerate() {
        let file = dir.join(format!("stub-body-{i}"));
        std::fs::write(&file, body).unwrap();
        let _ = writeln!(
            script,
            "*'{pattern}'*) cp '{}' \"$o\"; printf {status};;",
            file.display()
        );
    }
    script.push_str("*) : > \"$o\"; printf 404;;\nesac\n");
    let curl = dir.join("curl");
    std::fs::write(&curl, script).unwrap();
    std::fs::set_permissions(&curl, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    curl.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_config_values() {
        assert_eq!(quoted("a\"b\\c\nd"), "\"a\\\"b\\\\cd\"");
    }

    #[tokio::test]
    async fn sends_through_curl() {
        let tmp = tempfile::tempdir().unwrap();
        let curl = tmp.path().join("curl");
        // Writes the body and headers where -o and -D say, and echoes the
        // config it got on stdin into the body.
        std::fs::write(
            &curl,
            "#!/bin/sh\nwhile [ $# -gt 0 ]; do case \"$1\" in -o) o=$2; shift;; -D) d=$2; shift;; esac; shift; done\n\
             cat > \"$o\"\nprintf 'HTTP/1.1 200 OK\\r\\nGrpc-Status: 0\\r\\n' > \"$d\"\nprintf 200\n",
        )
        .unwrap();
        std::fs::set_permissions(&curl, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let http = Http {
            curl: curl.to_string_lossy().into_owned(),
            timeout: Duration::from_secs(5),
        };
        let r = http
            .send(&Request::post("http://x", "{}").bearer("tok"))
            .await
            .unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.header("grpc-status"), Some("0"));
        assert!(String::from_utf8_lossy(&r.body).contains("Bearer tok"));
    }

    #[test]
    fn reads_pi_logins() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("auth.json"),
            r#"{"xai":{"type":"api_key","key":"k"},"kimi-coding":{"type":"oauth","access":"a","expires":1}}"#,
        )
        .unwrap();
        assert_eq!(
            pi_login(Some(tmp.path()), "xai"),
            Some(PiLogin::Key("k".into()))
        );
        assert!(matches!(
            pi_login(Some(tmp.path()), "kimi-coding"),
            Some(PiLogin::OAuth { expired: true, .. })
        ));
        assert_eq!(pi_login(Some(tmp.path()), "none"), None);
    }
}
