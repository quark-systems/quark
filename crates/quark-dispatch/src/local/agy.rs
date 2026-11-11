//! Antigravity's quota, read the way quota-axi read it, without it.
//!
//! `agy -p /quota --output-format json` prints the signed-in account's
//! quota buckets: a five-hour and a weekly window for Gemini models and for
//! Claude/GPT models (and Gemini Argon on paid plans). It runs with stand-in
//! `open` and `xdg-open` first on its `PATH`, so a sign-in it wants can
//! never open a browser from quarkd. quota-axi's other source, probing a
//! running Antigravity's loopback language server, is not ported: the CLI
//! answers whenever Antigravity is signed in.
//!
//! The Gemini windows bound the `gemini` scope and the Claude/GPT windows
//! the `claude_gpt` scope.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Map, Value};
use time::OffsetDateTime;

use super::support::{self, num, pick, Cache};
use super::windows::{failure, reading, Window};
use super::{parse_time, LocalReading};

pub const PROVIDER: &str = "agy";
const SOURCE: &str = "quarkd: agy /quota";
const FIVE_HOURS: f64 = 18_000.0;
const SEVEN_DAYS: f64 = 604_800.0;

/// Reads Antigravity's quota through its CLI.
#[derive(Debug, Clone)]
pub struct AgyUsage {
    pub agy: String,
    pub timeout: Duration,
    pub cache: Cache,
}

impl Default for AgyUsage {
    fn default() -> Self {
        Self {
            agy: "agy".into(),
            timeout: Duration::from_secs(20),
            cache: Cache::default(),
        }
    }
}

impl AgyUsage {
    pub async fn reading(&self) -> Value {
        if let Some(v) = self.cache.get(PROVIDER) {
            return v;
        }
        let v = self.read_now().await;
        self.cache.put(PROVIDER, &v);
        v
    }

    async fn read_now(&self) -> Value {
        let now = OffsetDateTime::now_utc();
        let fail = |status: &str, error: &str| failure(PROVIDER, SOURCE, now, status, error, None);
        let guard = match OpenerGuard::new() {
            Ok(g) => g,
            Err(e) => return fail("unavailable", &format!("agy: {e}")),
        };
        let path = match std::env::var_os("PATH") {
            Some(p) => {
                let mut dirs = vec![guard.0.clone()];
                dirs.extend(std::env::split_paths(&p));
                std::env::join_paths(dirs).unwrap_or(p)
            }
            None => guard.0.clone().into_os_string(),
        };
        let path = path.to_string_lossy().into_owned();
        let argv = [
            self.agy.clone(),
            "-p".into(),
            "/quota".into(),
            "--output-format".into(),
            "json".into(),
        ];
        let out = super::run_with(&argv, None, &[("PATH", &path)], self.timeout).await;
        drop(guard);
        let text = match out {
            Ok((_, text)) => text,
            Err(e) if e.contains("No such file") || e.contains("not found") => {
                return fail("unavailable", "Antigravity's CLI (agy) is not installed")
            }
            Err(e) => return fail("unavailable", &e),
        };
        let parsed = text
            .find('{')
            .and_then(|i| serde_json::from_str::<Value>(text[i..].trim()).ok());
        match parsed.and_then(|v| quota_reading(&v, now)) {
            Some(v) => v,
            None if signed_out(&text) => fail(
                "auth_required",
                "Antigravity is not signed in here; run `agy` and sign in",
            ),
            None => fail("unavailable", "agy /quota printed no quota"),
        }
    }
}

fn signed_out(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    [
        "sign in",
        "log in",
        "login",
        "not authenticated",
        "unauthenticated",
    ]
    .iter()
    .any(|k| t.contains(k))
}

/// A scratch directory holding `open` and `xdg-open` that do nothing.
struct OpenerGuard(PathBuf);

impl OpenerGuard {
    fn new() -> std::io::Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "quarkd-agy-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir)?;
        for name in ["open", "xdg-open"] {
            write_stub(&dir.join(name))?;
        }
        Ok(Self(dir))
    }
}

fn write_stub(path: &Path) -> std::io::Result<()> {
    std::fs::write(path, "#!/bin/sh\nexit 1\n")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

impl Drop for OpenerGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `(id, label, rank)` of a bucket's model group.
fn group(name: &str, bucket: &str) -> (String, String, u8) {
    let n = format!("{name} {bucket}").to_ascii_lowercase();
    if n.contains("gemini") && n.contains("argon") {
        return ("gemini_argon".into(), "Gemini Argon".into(), 2);
    }
    if n.contains("gemini") {
        return ("gemini".into(), "Gemini".into(), 0);
    }
    if n.contains("claude") || n.contains("gpt") || n.contains("3p") {
        return ("claude_gpt".into(), "Claude/GPT".into(), 1);
    }
    let slug: String = name
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let slug = slug.trim_matches('_').to_string();
    let slug = if slug.is_empty() {
        "quota".into()
    } else {
        slug
    };
    (slug, name.to_string(), 2)
}

/// `(id, label, kind, length, rank)` of a bucket's window.
fn window_kind(b: &Value) -> (&'static str, &'static str, &'static str, Option<f64>, u8) {
    let names: Vec<String> = ["window", "bucketId", "bucket_id", "displayName", "name"]
        .iter()
        .filter_map(|k| support::text(b.get(*k)))
        .map(|s| s.to_ascii_lowercase())
        .collect();
    let strip = |s: &str| -> String {
        let s = [
            "gemini-",
            "gemini_",
            "3p-",
            "3p_",
            "third-party-",
            "third-party_",
        ]
        .iter()
        .find_map(|p| s.strip_prefix(p))
        .unwrap_or(s);
        s.strip_suffix(" limit").unwrap_or(s).to_string()
    };
    if names.iter().any(|n| {
        matches!(
            strip(n).as_str(),
            "5h" | "5-hour" | "five hour" | "five_hour" | "five-hour"
        )
    }) {
        return ("5h", "5-hour", "session", Some(FIVE_HOURS), 0);
    }
    if names.iter().any(|n| strip(n) == "weekly") {
        return ("weekly", "weekly", "weekly", Some(SEVEN_DAYS), 1);
    }
    ("unknown", "quota", "unknown", None, 2)
}

fn quota_reading(v: &Value, now: OffsetDateTime) -> Option<Value> {
    let command = v.get("command")?;
    let name = command.get("name")?.as_str()?;
    if !matches!(name, "usage" | "/usage" | "quota" | "/quota") {
        return None;
    }
    let data = command.get("data")?;
    let payload = data
        .get("response")
        .filter(|x| x.is_object())
        .or_else(|| data.get("summary").filter(|x| x.is_object()))
        .or_else(|| {
            data.get("groups")
                .is_some_and(Value::is_array)
                .then_some(data)
        })?;
    let mut ranked = Vec::new();
    for g in payload.get("groups")?.as_array()? {
        let gname = support::text(g.get("displayName"))
            .or_else(|| support::text(g.get("name")))
            .unwrap_or_else(|| "Quota".into());
        for b in g
            .get("buckets")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if b.get("disabled").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            let Some(id) = support::text(pick(b, &["bucketId", "bucket_id", "id"])) else {
                continue;
            };
            let (gid, glabel, grank) = group(&gname, &id);
            let (kid, klabel, kind, secs, krank) = window_kind(b);
            let remaining = num(pick(b, &["remainingFraction", "remaining_fraction"]))
                .or_else(|| num(b.get("remaining").and_then(|r| r.get("remainingFraction"))));
            let Some(remaining) = remaining else {
                continue;
            };
            let reset = pick(b, &["resetTime", "reset_time"]).and_then(parse_time);
            let used = (1.0 - remaining.clamp(0.0, 1.0)) * 100.0;
            ranked.push((
                (grank, krank),
                Window::new(
                    &format!("{gid}_{kid}"),
                    &format!("{glabel} {klabel}"),
                    kind,
                    used,
                )
                .resets(reset)
                .lasting(secs),
            ));
        }
    }
    ranked.sort_by_key(|(r, _)| *r);
    let windows: Vec<Window> = ranked.into_iter().map(|(_, w)| w).collect();
    if windows.is_empty() {
        return None;
    }
    let of = |ids: [&str; 2]| -> Vec<&Window> {
        windows
            .iter()
            .filter(|w| ids.contains(&w.id.as_str()))
            .collect()
    };
    Some(reading(
        PROVIDER,
        SOURCE,
        now,
        &windows,
        &[
            ("gemini".into(), of(["gemini_5h", "gemini_weekly"])),
            (
                "claude_gpt".into(),
                of(["claude_gpt_5h", "claude_gpt_weekly"]),
            ),
        ],
        Map::new(),
    ))
}

#[async_trait]
impl LocalReading for AgyUsage {
    fn provider(&self) -> &str {
        PROVIDER
    }

    async fn read(&self) -> Value {
        self.reading().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        json!({"command": {"name": "/quota", "data": {"groups": [
            {"displayName": "Gemini models", "buckets": [
                {"bucketId": "gemini-weekly", "remainingFraction": 0.6},
                {"bucketId": "gemini-5h", "remainingFraction": 0.9},
            ]},
            {"displayName": "Claude and GPT", "buckets": [
                {"bucketId": "3p-5h", "remainingFraction": 0.25},
                {"bucketId": "x", "disabled": true, "remainingFraction": 0},
            ]},
        ]}}})
    }

    #[test]
    fn groups_gemini_and_claude_gpt() {
        let v = quota_reading(&sample(), OffsetDateTime::now_utc()).unwrap();
        assert!(crate::quota::valid(&v), "{v}");
        let p = &v["providers"][0];
        let ids: Vec<&str> = p["windows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["gemini_5h", "gemini_weekly", "claude_gpt_5h"]);
        let rows = p["quotaSemantics"]["effectiveAvailability"]
            .as_array()
            .unwrap();
        assert_eq!(rows[0]["scope"], "gemini");
        assert_eq!(rows[0]["effectivePercentRemaining"], json!(60));
        assert_eq!(rows[1]["effectivePercentRemaining"], json!(25));
        assert!(quota_reading(
            &json!({"command": {"name": "help"}}),
            OffsetDateTime::now_utc()
        )
        .is_none());
    }

    #[tokio::test]
    async fn runs_the_cli_without_a_browser() {
        let tmp = tempfile::tempdir().unwrap();
        let agy = tmp.path().join("agy");
        // Fails when the stand-in `open` is not first on PATH.
        std::fs::write(
            &agy,
            format!(
                "#!/bin/sh\nopen && exit 3\nprintf 'Loading...\\n%s\\n' '{}'\n",
                sample()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&agy, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let r = AgyUsage {
            agy: agy.to_string_lossy().into_owned(),
            cache: Cache::none(),
            ..Default::default()
        };
        let v = r.read().await;
        assert_eq!(v["providers"][0]["state"]["status"], "fresh", "{v}");
        let missing = AgyUsage {
            agy: tmp.path().join("none").to_string_lossy().into_owned(),
            cache: Cache::none(),
            ..Default::default()
        };
        let v = missing.read().await;
        assert_eq!(v["providers"][0]["state"]["status"], "unavailable");
    }
}
