//! Cursor's plan usage, read the way quota-axi read it, without it.
//!
//! The login is the Cursor editor's access token in its `state.vscdb`
//! (read with `sqlite3`), else the Cursor CLI's (`cursor-agent login`): the
//! Keychain item `cursor-access-token` on macOS, `~/.config/cursor/auth.json`
//! elsewhere. The token goes to Cursor's dashboard service, which answers
//! the billing cycle's included, auto and API usage percents, the spend
//! limit, and the Grok Bot weekly allowance. Tokens are only read, never
//! refreshed.
//!
//! The included, auto, API and spend-limit windows jointly bound
//! `all_models`; Grok Bot is its own `grok_bot` scope.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use time::OffsetDateTime;

use super::claude::KEYCHAIN_REASON;
use super::support::{self, num, pick, Cache, Http, Keychain, Request};
use super::windows::{failure, reading, Window};
use super::{parse_time, LocalReading};

pub const PROVIDER: &str = "cursor";
const SOURCE: &str = "quarkd: Cursor usage";
const API: &str = "https://api2.cursor.sh/aiserver.v1.DashboardService";
const KEYCHAIN_SERVICE: &str = "cursor-access-token";
const KEYCHAIN_ACCOUNT: &str = "cursor-user";
const SIGN_IN: &str = "Cursor is not signed in here; sign in to Cursor or run `cursor-agent login`";
const IDE_WINDOWS: &[&str] = &["included_usage", "auto_usage", "api_usage", "spend_limit"];

/// Reads Cursor's plan usage for the machine's Cursor sign-in.
#[derive(Debug, Clone)]
pub struct CursorUsage {
    /// The editor's `state.vscdb`.
    pub state_db: Option<PathBuf>,
    /// The CLI's `auth.json` (not macOS).
    pub cli_auth: Option<PathBuf>,
    pub sqlite: String,
    pub keychain: Keychain,
    pub http: Http,
    pub api: String,
    pub cache: Cache,
}

#[derive(Debug, Clone, Default)]
struct Login {
    token: String,
    membership: Option<String>,
}

impl CursorUsage {
    pub fn from_env() -> Self {
        let home = support::user_home();
        let state_db = support::env("CURSOR_STATE_DB")
            .map(PathBuf::from)
            .or_else(|| {
                let base = if cfg!(target_os = "macos") {
                    home.as_ref()?.join("Library").join("Application Support")
                } else {
                    support::config_home()?
                };
                Some(base.join("Cursor/User/globalStorage/state.vscdb"))
            });
        Self {
            state_db,
            cli_auth: if cfg!(target_os = "macos") {
                None
            } else {
                support::config_home().map(|c| c.join("cursor").join("auth.json"))
            },
            ..Self::new()
        }
    }

    pub fn new() -> Self {
        Self {
            state_db: None,
            cli_auth: None,
            sqlite: "sqlite3".into(),
            keychain: Keychain::default(),
            http: Http::default(),
            api: API.into(),
            cache: Cache::default(),
        }
    }

    async fn state_value(&self, key: &str) -> Option<String> {
        let db = self.state_db.as_ref().filter(|p| p.exists())?;
        let argv = [
            self.sqlite.clone(),
            "-readonly".into(),
            db.to_string_lossy().into_owned(),
            format!(
                "select value from ItemTable where key = '{}' limit 1;",
                key.replace('\'', "''")
            ),
        ];
        let mut cmd = tokio::process::Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        let out = tokio::time::timeout(Duration::from_secs(5), cmd.output())
            .await
            .ok()?
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if text.is_empty() {
            return None;
        }
        Some(match serde_json::from_str::<Value>(&text) {
            Ok(Value::String(s)) if !s.is_empty() => s,
            _ => text,
        })
    }

    async fn editor_login(&self) -> Option<Login> {
        Some(Login {
            token: self.state_value("cursorAuth/accessToken").await?,
            membership: self.state_value("cursorAuth/stripeMembershipType").await,
        })
    }

    /// The CLI's login, or `Err` when the Keychain withheld it.
    async fn cli_login(&self, grant: bool) -> Result<Option<Login>, ()> {
        if let Some(f) = &self.cli_auth {
            if let Ok(Some(v)) = support::read_json(f) {
                if let Some(t) = support::text(v.get("accessToken")) {
                    return Ok(Some(Login {
                        token: t,
                        membership: None,
                    }));
                }
            }
        }
        Ok(self
            .keychain
            .read(KEYCHAIN_SERVICE, Some(KEYCHAIN_ACCOUNT), grant)
            .await?
            .filter(|t| !t.is_empty())
            .map(|token| Login {
                token,
                membership: None,
            }))
    }

    pub async fn reading(&self, grant: bool) -> Value {
        let key = "cursor";
        if !grant {
            if let Some(v) = self.cache.get(key) {
                return v;
            }
        }
        let v = self.read_now(grant).await;
        self.cache.put(key, &v);
        v
    }

    async fn read_now(&self, grant: bool) -> Value {
        let now = OffsetDateTime::now_utc();
        let fail = |status: &str, error: &str, reason: Option<&str>| {
            failure(PROVIDER, SOURCE, now, status, error, reason)
        };
        let mut last = None;
        if let Some(login) = self.editor_login().await {
            match self.read_with(&login, now).await {
                Ok(v) => return v,
                Err(e) => last = Some(e),
            }
        }
        if last.is_none() || matches!(last, Some((ref s, _)) if s == "auth_required") {
            match self.cli_login(grant).await {
                Ok(Some(login)) => match self.read_with(&login, now).await {
                    Ok(v) => return v,
                    Err(e) => last = Some(e),
                },
                Ok(None) => {}
                Err(()) if last.is_none() => {
                    return fail(
                        "unavailable",
                        &support::withheld_message("Cursor's sign-in"),
                        Some(KEYCHAIN_REASON),
                    )
                }
                Err(()) => {}
            }
        }
        match last {
            Some((s, e)) => fail(&s, &e, None),
            None => fail("auth_required", SIGN_IN, None),
        }
    }

    async fn rpc(&self, token: &str, method: &str) -> Result<Value, (String, String)> {
        let req = Request::post(format!("{}/{method}", self.api), "{}")
            .bearer(token)
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .header("connect-protocol-version", "1");
        let r = self.http.send(&req).await.map_err(|e| {
            (
                "unavailable".to_string(),
                format!("Cursor quota unavailable: {e}"),
            )
        })?;
        if !r.ok() {
            let (s, e) = support::http_failure("Cursor", r.status);
            return Err((s.into(), e));
        }
        r.json().ok_or_else(|| {
            (
                "unavailable".to_string(),
                "Cursor quota response invalid".to_string(),
            )
        })
    }

    async fn read_with(
        &self,
        login: &Login,
        now: OffsetDateTime,
    ) -> Result<Value, (String, String)> {
        let (usage, plan, sand) = tokio::join!(
            self.rpc(&login.token, "GetCurrentPeriodUsage"),
            self.rpc(&login.token, "GetPlanInfo"),
            self.rpc(&login.token, "GetSandUsageStatus"),
        );
        let usage = usage?;
        usage_reading(
            &usage,
            plan.ok().as_ref(),
            sand.ok().as_ref(),
            login.membership.as_deref(),
            now,
        )
        .ok_or_else(|| {
            (
                "unavailable".to_string(),
                "Cursor reported no quota windows".to_string(),
            )
        })
    }
}

impl Default for CursorUsage {
    fn default() -> Self {
        Self::new()
    }
}

fn usage_reading(
    usage: &Value,
    plan: Option<&Value>,
    sand: Option<&Value>,
    membership: Option<&str>,
    now: OffsetDateTime,
) -> Option<Value> {
    let info = plan.and_then(|p| p.get("planInfo"));
    let plan_name = info
        .and_then(|i| support::text(i.get("planName")).or_else(|| support::text(i.get("price"))))
        .or_else(|| membership.map(str::to_string));
    let reset = usage
        .get("billingCycleEnd")
        .and_then(parse_time)
        .or_else(|| info?.get("billingCycleEnd").and_then(parse_time));
    let start = usage
        .get("billingCycleStart")
        .and_then(parse_time)
        .or_else(|| info?.get("billingCycleStart").and_then(parse_time));
    let cycle = match (start, reset) {
        (Some(s), Some(r)) if r > s => Some((r - s).as_seconds_f64()),
        (None, Some(_)) => Some(30.0 * 86_400.0),
        _ => None,
    };
    let monthly = |id: &str, label: &str, used: f64| {
        Window::new(id, label, "monthly", used)
            .resets(reset)
            .lasting(cycle)
    };
    let mut windows = Vec::new();
    let pu = usage.get("planUsage");
    for (key, id, label) in [
        ("totalPercentUsed", "included_usage", "included usage"),
        ("autoPercentUsed", "auto_usage", "auto usage"),
        ("apiPercentUsed", "api_usage", "API usage"),
    ] {
        if let Some(p) = pu.and_then(|p| num(p.get(key))) {
            windows.push(monthly(id, label, p));
        }
    }
    let spend = usage.get("spendLimitUsage");
    let limit = spend.and_then(|s| num(s.get("individualLimit")));
    if let Some(limit) = limit.filter(|l| *l > 0.0) {
        let used = spend.and_then(|s| {
            num(s.get("individualUsed"))
                .or_else(|| num(s.get("individualRemaining")).map(|r| limit - r))
        });
        let used = used.map_or(0.0, |u| u / limit * 100.0);
        windows.push(
            Window::new("spend_limit", "spend limit", "credits", used)
                .resets(reset)
                .lasting(cycle),
        );
    }
    if let Some(s) = sand {
        let pooled = pick(
            s,
            &[
                "usesPooledEnterpriseAllowance",
                "uses_pooled_enterprise_allowance",
            ],
        )
        .and_then(Value::as_bool)
            == Some(true);
        if let (false, Some(p)) = (pooled, num(pick(s, &["usagePercent", "usage_percent"]))) {
            let reset = pick(s, &["nextResetTimestampUtc", "next_reset_timestamp_utc"])
                .and_then(parse_time);
            windows.push(
                Window::new("grok_bot", "Grok Bot", "weekly", p)
                    .resets(reset)
                    .lasting(Some(604_800.0)),
            );
        }
    }
    if windows.is_empty() {
        return None;
    }
    let ide: Vec<&Window> = windows
        .iter()
        .filter(|w| IDE_WINDOWS.contains(&w.id.as_str()))
        .collect();
    let bot: Vec<&Window> = windows.iter().filter(|w| w.id == "grok_bot").collect();
    let mut extra = Map::new();
    if let Some(p) = plan_name {
        extra.insert("plan".into(), json!(p));
    }
    Some(reading(
        PROVIDER,
        SOURCE,
        now,
        &windows,
        &[("all_models".into(), ide), ("grok_bot".into(), bot)],
        extra,
    ))
}

#[async_trait]
impl LocalReading for CursorUsage {
    fn provider(&self) -> &str {
        PROVIDER
    }

    async fn read(&self) -> Value {
        self.reading(false).await
    }

    async fn read_account(
        &self,
        config_dir: Option<&std::path::Path>,
        grant: bool,
    ) -> Option<Value> {
        match config_dir {
            None => Some(self.reading(grant).await),
            Some(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_plan_windows() {
        let now = OffsetDateTime::now_utc();
        let end = (now.unix_timestamp() + 10 * 86_400) * 1000;
        let usage = json!({
            "billingCycleEnd": end.to_string(),
            "planUsage": {"totalPercentUsed": 30, "autoPercentUsed": 10, "apiPercentUsed": 55},
            "spendLimitUsage": {"individualLimit": 2000, "individualUsed": 500},
        });
        let sand = json!({"usagePercent": 5, "nextResetTimestampUtc": end});
        let plan = json!({"planInfo": {"planName": "Pro"}});
        let v = usage_reading(&usage, Some(&plan), Some(&sand), None, now).unwrap();
        assert!(crate::quota::valid(&v), "{v}");
        let p = &v["providers"][0];
        assert_eq!(p["plan"], "Pro");
        assert_eq!(p["windows"].as_array().unwrap().len(), 5);
        let rows = p["quotaSemantics"]["effectiveAvailability"]
            .as_array()
            .unwrap();
        assert_eq!(rows[0]["scope"], "all_models");
        assert_eq!(rows[0]["effectivePercentRemaining"], json!(45));
        assert_eq!(rows[1]["scope"], "grok_bot");
        assert!(usage_reading(&json!({}), None, None, None, now).is_none());
    }

    #[tokio::test]
    async fn falls_back_to_the_cli_login() {
        let tmp = tempfile::tempdir().unwrap();
        let body = br#"{"planUsage":{"totalPercentUsed":12}}"#;
        let curl = support::stub_curl(tmp.path(), &[("GetCurrentPeriodUsage", 200, body)]);
        let mut r = CursorUsage::new();
        r.http.curl = curl;
        r.cache = Cache::none();
        r.keychain = Keychain::default().isolated();
        r.keychain.enabled = false;
        r.state_db = Some(tmp.path().join("missing.vscdb"));
        r.cli_auth = Some(tmp.path().join("auth.json"));
        let v = r.read().await;
        assert_eq!(v["providers"][0]["state"]["status"], "auth_required");
        std::fs::write(tmp.path().join("auth.json"), r#"{"accessToken":"t"}"#).unwrap();
        let v = r.read().await;
        assert_eq!(v["providers"][0]["state"]["status"], "fresh", "{v}");
        assert_eq!(r.read_account(Some(tmp.path()), false).await, None);
    }
}
