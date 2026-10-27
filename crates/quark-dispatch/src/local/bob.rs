//! IBM Bob's monthly Bobcoin allowance (firstmate's `fm-bob-quota.sh`).
//!
//! Two sources, tried in order:
//!
//! 1. **Live API**, when `BOBSHELL_API_KEY` is set (the variable Bob itself
//!    reads): `GET /admin/v1/profile` on `api.us-east.bob.ibm.com`, then each
//!    team's `/admin/v1/teams/<team>/users/<user>` on its region, summing
//!    `usage` and `budget_limit` across teams. The key reaches curl on
//!    stdin as a config line, never on argv, and is never logged.
//! 2. **Ledger plus anchor**, otherwise or when the API fails. Bob records
//!    each session's spend in the `tasks` table of `~/.bob/db/bob.db`
//!    (`costs` is JSON whose `cost` is Bobcoins), opened read-only. That
//!    ledger sees only this machine, so it is a lower bound; an anchor (one
//!    "Monthly Usage" figure from Bob's `/status`, recorded with the ledger
//!    total at that moment, `quarkd quota bob anchor <coins>`) turns it into
//!    an estimate: anchor plus ledger growth since, while the anchor is in
//!    the same charge month and younger than `anchor_max_age_hours`.
//!
//! Subagent rows are left out of the ledger: Bob adds a subagent's spend to
//! its parent task as it happens.
//!
//! Confidence: `known` (API, or a fresh anchor) gives a known row whose
//! floors and ordering apply; `partial` (ledger but no usable anchor) gives
//! an unknown row, `exhausted_now` when the ledger alone reaches the
//! allowance; `unknown` (nothing readable, or a bad config) gives no row.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::{OffsetDateTime, Time, UtcOffset};
use tokio::io::AsyncWriteExt;

use super::{epoch, selection, snapshot, tidy, utc_iso};

pub const PROVIDER: &str = "bob";
/// Bob's API key variable.
pub const KEY_ENV: &str = "BOBSHELL_API_KEY";
const API_HOST: &str = "api.us-east.bob.ibm.com";
const TRUSTED_SUFFIX: &str = ".bob.ibm.com";

/// `<home>/quota/bob.json`. A missing file is the defaults.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BobConfig {
    /// The monthly allowance, when the API does not report one.
    pub monthly_coins: f64,
    /// Day of the month the allowance resets, 1 to 28.
    pub reset_day: u8,
    /// `UTC` or a fixed offset such as `+05:30`.
    pub reset_timezone: String,
    pub anchor_max_age_hours: f64,
}

impl Default for BobConfig {
    fn default() -> Self {
        Self {
            monthly_coins: 1500.0,
            reset_day: 1,
            reset_timezone: "UTC".into(),
            anchor_max_age_hours: 24.0,
        }
    }
}

impl BobConfig {
    fn validate(&self) -> Result<UtcOffset, String> {
        if !self.monthly_coins.is_finite() || self.monthly_coins <= 0.0 {
            return Err("monthly_coins must be a number above 0".into());
        }
        if !(1..=28).contains(&self.reset_day) {
            return Err("reset_day must be a whole number from 1 to 28".into());
        }
        if !self.anchor_max_age_hours.is_finite() || self.anchor_max_age_hours <= 0.0 {
            return Err("anchor_max_age_hours must be a number above 0".into());
        }
        offset(&self.reset_timezone).ok_or_else(|| {
            format!(
                "reset_timezone {:?} must be UTC or a fixed offset such as +05:30",
                self.reset_timezone
            )
        })
    }

    /// The charge month containing `now`, as `[start, end)`.
    fn period(&self, tz: UtcOffset, now: OffsetDateTime) -> (OffsetDateTime, OffsetDateTime) {
        let local = now.to_offset(tz);
        let at = |y: i32, m: time::Month| {
            time::Date::from_calendar_date(y, m, self.reset_day)
                .expect("reset_day is 1..=28")
                .with_time(Time::MIDNIGHT)
                .assume_offset(tz)
        };
        let mut start = at(local.year(), local.month());
        if start > local {
            start = match local.month() {
                time::Month::January => at(local.year() - 1, time::Month::December),
                m => at(local.year(), m.previous()),
            };
        }
        let end = match start.month() {
            time::Month::December => at(start.year() + 1, time::Month::January),
            m => at(start.year(), m.next()),
        };
        (start, end)
    }
}

fn offset(tz: &str) -> Option<UtcOffset> {
    if matches!(tz, "UTC" | "Etc/UTC" | "Z") {
        return Some(UtcOffset::UTC);
    }
    let (sign, rest) = match tz.strip_prefix('+') {
        Some(r) => (1, r),
        None => (-1, tz.strip_prefix('-')?),
    };
    let (h, m) = rest.split_once(':')?;
    let (h, m): (i8, i8) = (h.parse().ok()?, m.parse().ok()?);
    UtcOffset::from_hms(sign * h, sign * m, 0).ok()
}

/// `<home>/quota/bob-anchor.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Anchor {
    pub used_coins: f64,
    #[serde(default)]
    pub budget_coins: Option<f64>,
    pub ledger_total_coins: f64,
    pub recorded_at: String,
    pub recorded_at_epoch: f64,
    /// Epoch seconds of the charge month the anchor was taken in.
    pub period_start: f64,
    #[serde(default)]
    pub source: String,
}

/// Reads Bob's allowance.
#[derive(Debug, Clone)]
pub struct BobUsage {
    pub config: PathBuf,
    pub anchor: PathBuf,
    /// Bob's database, `~/.bob/db/bob.db`.
    pub db: PathBuf,
    pub curl: String,
    pub api_timeout: Duration,
    /// The API key; read from [`KEY_ENV`] by [`Self::for_home`].
    pub api_key: Option<String>,
}

impl BobUsage {
    /// The reader for a Quark home: `<home>/quota/bob.json` and
    /// `bob-anchor.json`, Bob's database under `user_home`.
    pub fn for_home(home: &Path, user_home: Option<&Path>) -> Self {
        Self {
            config: home.join("quota").join("bob.json"),
            anchor: home.join("quota").join("bob-anchor.json"),
            db: user_home
                .map(|h| h.join(".bob").join("db").join("bob.db"))
                .unwrap_or_default(),
            curl: "curl".into(),
            api_timeout: Duration::from_secs(10),
            api_key: std::env::var(KEY_ENV).ok().and_then(|k| clean_key(&k)),
        }
    }

    fn load_config(&self) -> Result<(BobConfig, UtcOffset), String> {
        let cfg: BobConfig = match std::fs::read_to_string(&self.config) {
            Ok(s) => serde_json::from_str(&s)
                .map_err(|e| format!("{} is not valid: {e}", self.config.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BobConfig::default(),
            Err(e) => return Err(format!("{}: {e}", self.config.display())),
        };
        let tz = cfg.validate()?;
        Ok((cfg, tz))
    }

    fn load_anchor(&self) -> Result<Anchor, String> {
        match std::fs::read_to_string(&self.anchor) {
            Ok(s) => serde_json::from_str(&s).map_err(|_| "anchor record malformed".to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err("no anchor recorded".into()),
            Err(_) => Err("anchor record unreadable".into()),
        }
    }

    /// The reading now.
    pub async fn reading_at(&self, now: OffsetDateTime) -> Value {
        let mut bob = json!({"confidence": "unknown", "generatedAt": utc_iso(now)});
        let (cfg, tz) = match self.load_config() {
            Ok(c) => c,
            Err(e) => {
                bob["reason"] = json!(e);
                return snapshot(PROVIDER, "quarkd", now, "unknown", vec![], bob);
            }
        };
        let (start, end) = cfg.period(tz, now);
        let window = Window {
            start: epoch(start),
            end: epoch(end),
            now: epoch(now),
        };
        bob["allowanceCoins"] = tidy(cfg.monthly_coins, 6);
        bob["resetTimezone"] = json!(cfg.reset_timezone);
        bob["periodStart"] = json!(utc_iso(start));
        bob["periodEnd"] = json!(utc_iso(end));

        let mut api_error = None;
        if let Some(key) = &self.api_key {
            match self.fetch_api(key).await {
                Ok(api) => {
                    let allowance = api.limit.unwrap_or(cfg.monthly_coins);
                    let row = known_row(allowance, api.used, window);
                    bob["confidence"] = json!("known");
                    bob["source"] = json!("api");
                    bob["estimatedUsedCoins"] = tidy(api.used, 6);
                    bob["allowanceCoins"] = tidy(allowance, 6);
                    bob["basis"] = json!("live API");
                    bob["reason"] = json!("live API read succeeded");
                    if let Some(l) = api.limit {
                        bob["apiLimitCoins"] = tidy(l, 6);
                    }
                    return snapshot(PROVIDER, "quarkd", now, "known", vec![row], bob);
                }
                Err(e) => {
                    bob["apiError"] = json!(e);
                    api_error = Some(e);
                }
            }
        }

        let db = self.db.clone();
        let since_ms = window.start * 1000.0;
        let ledger = tokio::task::spawn_blocking(move || ledger(&db, since_ms))
            .await
            .unwrap_or_else(|e| Err(format!("bob ledger read failed: {e}")));
        let (month, total) = match ledger {
            Ok(l) => l,
            Err(mut e) => {
                if let Some(api) = api_error {
                    if self.db.as_os_str().is_empty() {
                        // A key account has only the API.
                        e = api;
                    } else {
                        e.push_str("; API fallback also unavailable: ");
                        e.push_str(&api);
                    }
                }
                bob["reason"] = json!(e);
                return snapshot(PROVIDER, "quarkd", now, "unknown", vec![], bob);
            }
        };
        bob["ledgerMonthCoins"] = tidy(month, 6);
        bob["ledgerTotalCoins"] = tidy(total, 6);

        let anchor = self.load_anchor().and_then(|a| {
            let age_h = (window.now - a.recorded_at_epoch) / 3600.0;
            bob["anchor"] = json!({"usedCoins": a.used_coins, "budgetCoins": a.budget_coins,
                "recordedAt": a.recorded_at, "source": a.source,
                "ageHours": (age_h * 100.0).round() / 100.0});
            if (a.period_start - window.start).abs() > 0.5 {
                Err("anchor is from another charge month".to_string())
            } else if age_h < 0.0 {
                Err("anchor is recorded in the future".into())
            } else if age_h > cfg.anchor_max_age_hours {
                Err(format!(
                    "anchor is {age_h:.1} hours old (limit {})",
                    cfg.anchor_max_age_hours
                ))
            } else {
                Ok(a)
            }
        });
        match anchor {
            Ok(a) => {
                let used = a.used_coins + (total - a.ledger_total_coins).max(0.0);
                let row = known_row(cfg.monthly_coins, used, window);
                bob["confidence"] = json!("known");
                bob["estimatedUsedCoins"] = tidy(used, 6);
                bob["basis"] = json!("anchor plus ledger growth");
                bob["reason"] = json!("anchor fresh");
                snapshot(PROVIDER, "quarkd", now, "known", vec![row], bob)
            }
            Err(why) => {
                let mut row = json!({"scope": "all_models", "status": "unknown"});
                let mut reason = format!("{why}; ledger alone is a lower bound on usage");
                if month >= cfg.monthly_coins {
                    row["runway"] = json!({"status": "exhausted_now"});
                    reason.push_str("; the ledger alone reaches the allowance");
                }
                bob["confidence"] = json!("partial");
                bob["reason"] = json!(reason);
                snapshot(PROVIDER, "quarkd", now, "partial", vec![row], bob)
            }
        }
    }

    /// Record `used` coins (and the budget, when `/status` shows one) as
    /// the anchor, with the ledger total now. Refuses when the ledger cannot
    /// be read, since growth could not be measured from it. Returns a line
    /// for people.
    pub fn record_anchor(
        &self,
        used: f64,
        budget: Option<f64>,
        source: &str,
        now: OffsetDateTime,
    ) -> Result<String, String> {
        if !used.is_finite() || used < 0.0 {
            return Err("used coins must be a non-negative number".into());
        }
        let (cfg, tz) = self.load_config()?;
        let (start, _) = cfg.period(tz, now);
        let (_, total) = ledger(&self.db, epoch(start) * 1000.0)
            .map_err(|e| format!("cannot anchor: the Bob ledger is not readable ({e})"))?;
        let anchor = Anchor {
            used_coins: used,
            budget_coins: budget,
            ledger_total_coins: total,
            recorded_at: utc_iso(now),
            recorded_at_epoch: epoch(now),
            period_start: epoch(start),
            source: source.into(),
        };
        let dir = self.anchor.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let tmp = self
            .anchor
            .with_extension(format!("quark-{}.tmp", std::process::id()));
        let body = serde_json::to_string_pretty(&anchor).map_err(|e| e.to_string())? + "\n";
        std::fs::write(&tmp, body)
            .and_then(|_| restrict(&tmp))
            .and_then(|_| std::fs::rename(&tmp, &self.anchor))
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                format!("{}: {e}", self.anchor.display())
            })?;
        let mut note = String::new();
        if let Some(b) = budget.filter(|b| (b - cfg.monthly_coins).abs() > 0.005) {
            note = format!(
                "; note: /status shows a {b:.2}-coin budget but quota/bob.json uses {}",
                cfg.monthly_coins
            );
        }
        Ok(format!(
            "bob: anchored {used:.2} used coins at ledger total {total:.2} ({source}){note}"
        ))
    }

    async fn fetch_api(&self, key: &str) -> Result<ApiUsage, String> {
        let profile: Value = self
            .get(&format!("https://{API_HOST}/admin/v1/profile"), key, &[])
            .await
            .map_err(|e| format!("profile {e}"))?;
        let Some(instances) = profile.get("instances").and_then(Value::as_array) else {
            return Err("profile response missing 'instances'".into());
        };
        let mut teams = Vec::new();
        for inst in instances {
            let s = |k: &str| inst.get(k).and_then(Value::as_str).unwrap_or("").trim();
            let user = s("user_id");
            if user.is_empty() {
                continue;
            }
            let Some(base) = regional_base(s("region_domain")) else {
                continue;
            };
            let instance = s("instance_id");
            for team in inst
                .get("teams")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let id = team.get("id").and_then(Value::as_str).unwrap_or("").trim();
                if id.is_empty() {
                    continue;
                }
                let url = format!(
                    "{base}/admin/v1/teams/{}/users/{}",
                    percent_encode(id),
                    percent_encode(user)
                );
                let mut headers = vec![("x-team-id", id.to_string())];
                if !instance.is_empty() {
                    headers.push(("x-instance-id", instance.to_string()));
                }
                let resp = self
                    .get(&url, key, &headers)
                    .await
                    .map_err(|e| format!("team {id}: {e}"))?;
                if resp.get("usage").is_none() {
                    return Err(format!("team response missing 'usage' for team {id}"));
                }
                let limit = number(&resp["budget_limit"])
                    .or_else(|| number(&team["budget_limit"]))
                    .filter(|l| *l >= 0.0);
                teams.push((number(&resp["usage"]).unwrap_or(0.0).max(0.0), limit));
            }
        }
        aggregate(&teams).ok_or_else(|| "API returned no subscription teams for this key".into())
    }

    /// GET `url` as JSON, the key in a config line on curl's stdin.
    async fn get(&self, url: &str, key: &str, headers: &[(&str, String)]) -> Result<Value, String> {
        let mut cmd = tokio::process::Command::new(&self.curl);
        cmd.args(["-sS", "--max-time"])
            .arg(self.api_timeout.as_secs().max(1).to_string())
            .args([
                "-w",
                "\n%{http_code}",
                "-H",
                "Accept: application/json",
                "-H",
                "User-Agent: quarkd",
            ]);
        for (k, v) in headers {
            cmd.arg("-H").arg(format!("{k}: {v}"));
        }
        let mut child = cmd
            .args(["-K", "-"])
            .arg(url)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("curl: {e}"))?;
        let mut stdin = child.stdin.take().ok_or("curl stdin")?;
        stdin
            .write_all(format!("header = \"Authorization: {}\"\n", auth_header(key)).as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        drop(stdin);
        let out = tokio::time::timeout(
            self.api_timeout + Duration::from_secs(5),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| "timed out".to_string())?
        .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!("curl exited {}", out.status));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let (body, code) = text.rsplit_once('\n').unwrap_or(("", &text));
        match code.trim().parse::<u16>().unwrap_or(0) {
            401 | 403 => Err(format!("API key rejected (HTTP {})", code.trim())),
            200..=299 => serde_json::from_str(body).map_err(|_| "response is not JSON".into()),
            c => Err(format!("HTTP {c}")),
        }
    }
}

#[async_trait]
impl super::LocalReading for BobUsage {
    fn provider(&self) -> &str {
        PROVIDER
    }

    async fn read(&self) -> Value {
        self.reading_at(OffsetDateTime::now_utc()).await
    }

    /// Another key's live API reading. Bob's local ledger and anchor are
    /// the default login's, so they are not used for it.
    async fn read_with_key(&self, key: &str) -> Option<Value> {
        let reader = Self {
            db: PathBuf::new(),
            api_key: clean_key(key),
            ..self.clone()
        };
        Some(reader.reading_at(OffsetDateTime::now_utc()).await)
    }
}

#[derive(Debug, Clone, Copy)]
struct Window {
    start: f64,
    end: f64,
    now: f64,
}

fn known_row(allowance: f64, used: f64, w: Window) -> Value {
    let pct = if allowance > 0.0 {
        (100.0 * (allowance - used) / allowance).clamp(0.0, 100.0)
    } else {
        0.0
    };
    let pct = (pct * 100.0).round() / 100.0;
    let used_pct = if allowance > 0.0 {
        100.0 * used / allowance
    } else {
        100.0
    };
    let sp = super::spend_priority(pct, used_pct, w.start, w.end, w.now);
    json!({
        "scope": "all_models",
        "status": "known",
        "effectivePercentRemaining": tidy(pct, 2),
        "runway": {"status": if pct <= 0.0 { "exhausted_now" } else { "unknown" }},
        "selection": selection(sp),
    })
}

#[derive(Debug, Clone, PartialEq)]
struct ApiUsage {
    used: f64,
    /// The summed limit, only when every team has one.
    limit: Option<f64>,
}

fn aggregate(teams: &[(f64, Option<f64>)]) -> Option<ApiUsage> {
    if teams.is_empty() {
        return None;
    }
    let used = teams.iter().map(|t| t.0).sum();
    let limit = teams
        .iter()
        .map(|t| t.1)
        .collect::<Option<Vec<f64>>>()
        .map(|l| l.iter().sum());
    Some(ApiUsage { used, limit })
}

fn number(v: &Value) -> Option<f64> {
    v.as_f64()
}

fn clean_key(raw: &str) -> Option<String> {
    let mut v = raw.trim();
    if v.len() >= 2 && (v.starts_with('"') || v.starts_with('\'')) && v.ends_with(&v[..1]) {
        v = v[1..v.len() - 1].trim();
    }
    (!v.is_empty()).then(|| v.to_string())
}

/// `Bearer` for a JWT, else Bob's `Apikey` scheme.
fn auth_header(key: &str) -> String {
    let jwt = {
        let parts: Vec<&str> = key.split('.').collect();
        parts.len() == 3 && base64_json_object(parts[1])
    };
    format!("{} {key}", if jwt { "Bearer" } else { "Apikey" })
}

fn base64_json_object(part: &str) -> bool {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut bits = 0u32;
    let mut n = 0;
    let mut out = Vec::new();
    for b in part.bytes().filter(|b| *b != b'=') {
        let b = match b {
            b'+' => b'-',
            b'/' => b'_',
            b => b,
        };
        let Some(v) = ALPHABET.iter().position(|a| *a == b) else {
            return false;
        };
        bits = (bits << 6) | v as u32;
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((bits >> n) as u8);
        }
    }
    serde_json::from_slice::<Value>(&out).is_ok_and(|v| v.is_object())
}

/// The regional API base for `region_domain`, only on Bob's own hosts.
fn regional_base(region: &str) -> Option<String> {
    if region.is_empty() {
        return Some(format!("https://{API_HOST}"));
    }
    let mut h = region.to_ascii_lowercase();
    if !h.starts_with("api.") {
        h = format!("api.{h}");
    }
    (h == API_HOST || h.ends_with(TRUSTED_SUFFIX)).then(|| format!("https://{h}"))
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(unix)]
fn restrict(p: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict(_: &Path) -> std::io::Result<()> {
    Ok(())
}

/// One `tasks` row: `created_at` (epoch ms) and `costs`.
type LedgerRow = (Option<f64>, Option<String>);

/// Coins in rows created since `since_ms`, and in every row.
fn ledger(db: &Path, since_ms: f64) -> Result<(f64, f64), String> {
    if db.as_os_str().is_empty() || !db.is_file() {
        return Err(format!("bob database absent ({})", db.display()));
    }
    let query = |con: &rusqlite::Connection| -> Result<Vec<LedgerRow>, rusqlite::Error> {
        let cols = columns(con)?;
        let has = |c: &str| cols.iter().any(|x| x == c);
        if !has("costs") || !has("created_at") {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let mut sql = "SELECT created_at, costs FROM tasks".to_string();
        if has("task_type") {
            sql.push_str(" WHERE task_type IS NULL OR task_type <> 'subagent'");
        }
        let mut stmt = con.prepare(&sql)?;
        let rows = stmt
            .query_map([], |r| {
                let created = match r.get_ref(0)? {
                    rusqlite::types::ValueRef::Integer(i) => Some(i as f64),
                    rusqlite::types::ValueRef::Real(f) => Some(f),
                    _ => None,
                };
                let costs = match r.get_ref(1)? {
                    rusqlite::types::ValueRef::Text(t) => {
                        Some(String::from_utf8_lossy(t).into_owned())
                    }
                    _ => None,
                };
                Ok((created, costs))
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    };
    let rows = read_only(db, query).map_err(|e| match e {
        Ok(rusqlite::Error::InvalidQuery) => {
            "bob database has no tasks table with costs and created_at".to_string()
        }
        Ok(e) => format!("bob database not readable read-only: {e}"),
        Err(e) => e,
    })?;
    let (mut month, mut total) = (0.0, 0.0);
    for (created, costs) in rows {
        let cost = costs
            .and_then(|c| serde_json::from_str::<Value>(&c).ok())
            .and_then(|v| v.get("cost").and_then(Value::as_f64))
            .filter(|c| *c >= 0.0);
        let Some(cost) = cost else { continue };
        total += cost;
        if created.is_some_and(|c| c >= since_ms) {
            month += cost;
        }
    }
    Ok((month, total))
}

/// Whether Bob has a conversation to continue in `dir`: a top-level task
/// started there, which `bob chat --resume latest` picks up. Read from
/// Bob's database at `db` (`~/.bob/db/bob.db`). With `since`, only a task
/// created at or after it counts, so a reused worktree slot does not offer
/// an earlier occupant's task.
pub fn has_task(db: &Path, dir: &Path, since: Option<std::time::SystemTime>) -> bool {
    if db.as_os_str().is_empty() || !db.is_file() {
        return false;
    }
    let mut dirs = vec![dir.to_string_lossy().into_owned()];
    if let Ok(real) = std::fs::canonicalize(dir) {
        let real = real.to_string_lossy().into_owned();
        if real != dirs[0] {
            dirs.push(real);
        }
    }
    let query = |con: &rusqlite::Connection| -> Result<bool, rusqlite::Error> {
        let cols = columns(con)?;
        let has = |c: &str| cols.iter().any(|x| x == c);
        if !has("directory") {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let mut sql = "SELECT 1 FROM tasks WHERE directory = ?1".to_string();
        if has("task_type") {
            sql.push_str(" AND (task_type IS NULL OR task_type = 'normal')");
        }
        if has("parent_id") {
            sql.push_str(" AND parent_id IS NULL");
        }
        // `created_at` is epoch milliseconds.
        let since_ms = since.filter(|_| has("created_at")).map(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as i64)
        });
        if since_ms.is_some() {
            sql.push_str(" AND created_at >= ?2");
        }
        sql.push_str(" LIMIT 1");
        let mut stmt = con.prepare(&sql)?;
        for d in &dirs {
            let found = match since_ms {
                Some(ms) => stmt.exists(rusqlite::params![d, ms])?,
                None => stmt.exists([d])?,
            };
            if found {
                return Ok(true);
            }
        }
        Ok(false)
    };
    read_only(db, query).unwrap_or(false)
}

/// One Bob task's spend so far, from its `costs`.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskCost {
    pub id: String,
    /// When the task started, epoch milliseconds.
    pub created_ms: Option<f64>,
    /// Bobcoins.
    pub coins: f64,
    /// The model, when `costs` names one.
    pub model: Option<String>,
    /// Token counts, when `costs` records them.
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

/// The spend of every task Bob ran in `dir` (subtasks too; subagent rows are
/// already counted in their parent), oldest first. Read from Bob's database
/// at `db`. An absent database, or one without `costs`, reads as no tasks.
pub fn task_costs(db: &Path, dir: &Path) -> Result<Vec<TaskCost>, String> {
    if db.as_os_str().is_empty() || !db.is_file() {
        return Ok(Vec::new());
    }
    let mut dirs = vec![dir.to_string_lossy().into_owned()];
    if let Ok(real) = std::fs::canonicalize(dir) {
        let real = real.to_string_lossy().into_owned();
        if real != dirs[0] {
            dirs.push(real);
        }
    }
    type Row = (String, Option<f64>, Option<String>);
    let query = |con: &rusqlite::Connection| -> Result<Vec<Row>, rusqlite::Error> {
        let cols = columns(con)?;
        let has = |c: &str| cols.iter().any(|x| x == c);
        if !has("costs") || !has("directory") || !has("id") {
            return Ok(Vec::new());
        }
        let created = if has("created_at") {
            "created_at"
        } else {
            "NULL"
        };
        let mut sql =
            format!("SELECT CAST(id AS TEXT), {created}, costs FROM tasks WHERE directory = ?1");
        if has("task_type") {
            sql.push_str(" AND (task_type IS NULL OR task_type <> 'subagent')");
        }
        sql.push_str(&format!(" ORDER BY {created}, rowid"));
        let mut stmt = con.prepare(&sql)?;
        let mut rows = Vec::new();
        for d in &dirs {
            let found = stmt.query_map([d], |r| {
                let created = match r.get_ref(1)? {
                    rusqlite::types::ValueRef::Integer(i) => Some(i as f64),
                    rusqlite::types::ValueRef::Real(f) => Some(f),
                    _ => None,
                };
                let costs = match r.get_ref(2)? {
                    rusqlite::types::ValueRef::Text(t) => {
                        Some(String::from_utf8_lossy(t).into_owned())
                    }
                    _ => None,
                };
                Ok((r.get::<_, String>(0)?, created, costs))
            })?;
            for row in found {
                rows.push(row?);
            }
        }
        Ok(rows)
    };
    let rows = read_only(db, query).map_err(|e| match e {
        Ok(e) => format!("bob database not readable read-only: {e}"),
        Err(e) => e,
    })?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, created_ms, costs)| {
            let v = serde_json::from_str::<Value>(&costs?).ok()?;
            let coins = v
                .get("cost")
                .and_then(Value::as_f64)
                .filter(|c| *c >= 0.0)?;
            let n = |keys: &[&str]| {
                keys.iter()
                    .find_map(|k| v.get(*k).and_then(Value::as_u64))
                    .unwrap_or(0)
            };
            let model = ["model", "modelId", "model_id"]
                .iter()
                .find_map(|k| v.get(*k).and_then(Value::as_str))
                .filter(|m| !m.is_empty())
                .map(str::to_string);
            Some(TaskCost {
                id,
                created_ms,
                coins,
                model,
                input: n(&["tokensIn", "inputTokens", "input_tokens"]),
                output: n(&["tokensOut", "outputTokens", "output_tokens"]),
                cache_read: n(&["cacheReads", "cacheReadTokens", "cache_read_tokens"]),
                cache_write: n(&["cacheWrites", "cacheWriteTokens", "cache_write_tokens"]),
            })
        })
        .collect())
}

/// The `tasks` table's columns.
fn columns(con: &rusqlite::Connection) -> Result<Vec<String>, rusqlite::Error> {
    con.prepare("PRAGMA table_info(tasks)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect()
}

/// `query` run on `db` opened read-only, never writing beside it. The error
/// is the query's (`Ok`), or why the database changed under the read.
fn read_only<T>(
    db: &Path,
    query: impl Fn(&rusqlite::Connection) -> Result<T, rusqlite::Error>,
) -> Result<T, Result<rusqlite::Error, String>> {
    use rusqlite::{Connection, OpenFlags};
    let run = |immutable: bool| -> Result<T, rusqlite::Error> {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
        let uri = format!(
            "file:{}?{}",
            percent_path(db),
            if immutable { "immutable=1" } else { "mode=ro" }
        );
        let con = Connection::open_with_flags(uri, flags)?;
        con.busy_timeout(Duration::from_secs(2))?;
        query(&con)
    };
    let wal = db.with_file_name(format!(
        "{}-wal",
        db.file_name().unwrap_or_default().to_string_lossy()
    ));
    match run(false) {
        Ok(v) => Ok(v),
        // A WAL database without its -wal file cannot open mode=ro without
        // creating -shm; every committed page is then in the main file, so
        // an immutable read is exact.
        Err(e) if !wal.exists() && is_wal(db) && !matches!(e, rusqlite::Error::InvalidQuery) => {
            let v = run(true).map_err(Ok)?;
            if wal.exists() {
                run(false).map_err(|e| Err(format!("bob database changed during the read: {e}")))
            } else {
                Ok(v)
            }
        }
        Err(e) => Err(Ok(e)),
    }
}

fn is_wal(db: &Path) -> bool {
    use std::io::Read;
    let mut header = [0u8; 20];
    std::fs::File::open(db)
        .and_then(|mut f| f.read_exact(&mut header))
        .is_ok()
        && &header[..16] == b"SQLite format 3\0"
        && header[18] == 2
        && header[19] == 2
}

fn percent_path(p: &Path) -> String {
    let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    abs.to_string_lossy()
        .bytes()
        .map(|b| match b {
            b'?' | b'#' | b'%' => format!("%{b:02X}"),
            b => (b as char).to_string(),
        })
        .collect()
}

/// One line for people: the estimate and its basis, or why it is not
/// measured.
pub fn status_line(reading: &Value) -> String {
    let p = &reading["providers"][0];
    let b = &p[PROVIDER];
    let reason = b["reason"].as_str().unwrap_or("unknown");
    match p["quotaSemantics"]["status"].as_str() {
        Some("known") => format!(
            "bob: {} of {} coins used ({}% left, {}), month ends {}",
            b["estimatedUsedCoins"],
            b["allowanceCoins"],
            p["quotaSemantics"]["effectiveAvailability"][0]["effectivePercentRemaining"],
            b["basis"].as_str().unwrap_or(""),
            b["periodEnd"].as_str().unwrap_or("").get(..10).unwrap_or("")
        ),
        Some("partial") => format!(
            "bob: estimate unverifiable - {reason}; this machine's ledger shows {} coins this month",
            b["ledgerMonthCoins"]
        ),
        _ => format!("bob: unmeasured - {reason}"),
    }
}

/// Reads a used figure (and the budget, when shown) from a captured
/// `/status` screen: the last `Monthly Usage:` and `Monthly Budget:`.
pub fn parse_status(screen: &str) -> Option<(f64, Option<f64>)> {
    let last = |label: &str| {
        screen
            .match_indices(label)
            .filter_map(|(i, _)| super::leading_number(&screen[i + label.len()..]).map(|(n, _)| n))
            .last()
    };
    Some((last("Monthly Usage:")?, last("Monthly Budget:")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QuotaSnapshot;

    fn at(s: &str) -> OffsetDateTime {
        OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).unwrap()
    }

    fn ms(s: &str) -> i64 {
        (at(s).unix_timestamp_nanos() / 1_000_000) as i64
    }

    struct Fixture {
        dir: tempfile::TempDir,
        reader: BobUsage,
    }

    fn fixture(rows: &[(&str, &str, Option<&str>)]) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("quark");
        let user = dir.path().join("user");
        let mut reader = BobUsage::for_home(&home, Some(&user));
        reader.api_key = None;
        std::fs::create_dir_all(reader.db.parent().unwrap()).unwrap();
        let con = rusqlite::Connection::open(&reader.db).unwrap();
        con.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE tasks (id INTEGER PRIMARY KEY, created_at INTEGER, costs TEXT, task_type TEXT);",
        )
        .unwrap();
        for (created, costs, kind) in rows {
            con.execute(
                "INSERT INTO tasks (created_at, costs, task_type) VALUES (?1, ?2, ?3)",
                rusqlite::params![ms(created), costs, kind],
            )
            .unwrap();
        }
        drop(con);
        Fixture { dir, reader }
    }

    fn parse(v: &Value) -> crate::quota::ProviderQuota {
        QuotaSnapshot::parse_checked(&v.to_string())
            .unwrap()
            .provider("bob")
            .unwrap()
            .clone()
    }

    const ROWS: &[(&str, &str, Option<&str>)] = &[
        ("2026-09-20T00:00:00Z", r#"{"cost": 100}"#, None),
        ("2026-10-05T00:00:00Z", r#"{"cost": 200}"#, Some("chat")),
        ("2026-10-06T00:00:00Z", r#"{"cost": 50}"#, Some("subagent")),
        ("2026-10-07T00:00:00Z", r#"not json"#, None),
    ];

    #[test]
    fn a_top_level_task_in_the_worktree_can_be_resumed() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("bob.db");
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        assert!(!has_task(&db, &wt, None), "no database");
        let con = rusqlite::Connection::open(&db).unwrap();
        con.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE tasks (id TEXT PRIMARY KEY, parent_id TEXT, directory TEXT, task_type TEXT);",
        )
        .unwrap();
        let wt_s = wt.to_string_lossy().into_owned();
        // A subtask or subagent there is not something to resume.
        con.execute(
            "INSERT INTO tasks VALUES ('a', 'p', ?1, 'subtask'), ('b', NULL, ?1, 'subagent'), ('c', NULL, '/elsewhere', 'normal')",
            [&wt_s],
        )
        .unwrap();
        assert!(!has_task(&db, &wt, None));
        con.execute(
            "INSERT INTO tasks VALUES ('d', NULL, ?1, 'normal')",
            [&wt_s],
        )
        .unwrap();
        drop(con);
        assert!(has_task(&db, &wt, None));
        assert!(!has_task(&db, &dir.path().join("other"), None));
    }

    #[test]
    fn an_earlier_occupants_task_is_not_resumed() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("bob.db");
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        let con = rusqlite::Connection::open(&db).unwrap();
        con.execute_batch(
            "CREATE TABLE tasks (id TEXT PRIMARY KEY, parent_id TEXT, directory TEXT, task_type TEXT, created_at INTEGER);",
        )
        .unwrap();
        let wt_s = wt.to_string_lossy().into_owned();
        // The slot's previous task, started at 1_000_000 ms.
        con.execute(
            "INSERT INTO tasks VALUES ('old', NULL, ?1, 'normal', 1000000)",
            [&wt_s],
        )
        .unwrap();
        let took = std::time::UNIX_EPOCH + std::time::Duration::from_millis(2_000_000);
        assert!(has_task(&db, &wt, None));
        assert!(!has_task(&db, &wt, Some(took)));
        con.execute(
            "INSERT INTO tasks VALUES ('new', NULL, ?1, 'normal', 2000500)",
            [&wt_s],
        )
        .unwrap();
        drop(con);
        assert!(has_task(&db, &wt, Some(took)));
    }

    #[tokio::test]
    async fn ledger_alone_is_partial() {
        let f = fixture(ROWS);
        let v = f.reader.reading_at(at("2026-10-16T00:00:00Z")).await;
        let p = parse(&v);
        assert_eq!(p.status, "partial");
        assert!(!p.rows[0].percent_remaining.is_some());
        let b = &v["providers"][0]["bob"];
        assert_eq!(b["ledgerMonthCoins"], 200);
        assert_eq!(b["ledgerTotalCoins"], 300);
        assert!(b["reason"]
            .as_str()
            .unwrap()
            .starts_with("no anchor recorded"));
        assert!(status_line(&v).starts_with("bob: estimate unverifiable - no anchor recorded"));
        assert!(f.dir.path().exists());
    }

    #[tokio::test]
    async fn a_fresh_anchor_measures_usage() {
        let f = fixture(ROWS);
        let line = f
            .reader
            .record_anchor(600.0, Some(1500.0), "manual", at("2026-10-15T00:00:00Z"))
            .unwrap();
        assert!(line.starts_with("bob: anchored 600.00 used coins at ledger total 300.00"));
        // 150 more coins since the anchor.
        let con = rusqlite::Connection::open(&f.reader.db).unwrap();
        con.execute(
            "INSERT INTO tasks (created_at, costs) VALUES (?1, ?2)",
            rusqlite::params![ms("2026-10-15T06:00:00Z"), r#"{"cost": 150}"#],
        )
        .unwrap();
        drop(con);
        let v = f.reader.reading_at(at("2026-10-16T00:00:00Z")).await;
        let p = parse(&v);
        assert_eq!(p.status, "known");
        assert_eq!(p.rows[0].percent_remaining, Some(50.0));
        assert!(p.rows[0].spend_priority.is_some());
        assert_eq!(v["providers"][0]["bob"]["estimatedUsedCoins"], 750);
        assert_eq!(
            status_line(&v),
            "bob: 750 of 1500 coins used (50% left, anchor plus ledger growth), month ends 2026-11-01"
        );

        // Too old, then a new month.
        let v = f.reader.reading_at(at("2026-10-17T00:00:00Z")).await;
        assert_eq!(parse(&v).status, "partial");
        let v = f.reader.reading_at(at("2026-11-02T00:00:00Z")).await;
        assert!(v["providers"][0]["bob"]["reason"]
            .as_str()
            .unwrap()
            .starts_with("anchor is from another charge month"));
    }

    #[tokio::test]
    async fn spent_ledger_reads_exhausted() {
        let f = fixture(&[("2026-10-05T00:00:00Z", r#"{"cost": 1600}"#, None)]);
        let v = f.reader.reading_at(at("2026-10-16T00:00:00Z")).await;
        let p = parse(&v);
        assert_eq!(p.rows[0].runway.as_deref(), Some("exhausted_now"));
    }

    #[tokio::test]
    async fn unreadable_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = BobUsage::for_home(dir.path(), Some(dir.path()));
        r.api_key = None;
        let v = r.reading_at(at("2026-10-16T00:00:00Z")).await;
        assert_eq!(parse(&v).status, "unknown");
        assert!(status_line(&v).starts_with("bob: unmeasured - bob database absent"));
        assert!(r
            .record_anchor(1.0, None, "manual", at("2026-10-16T00:00:00Z"))
            .is_err());

        std::fs::create_dir_all(r.config.parent().unwrap()).unwrap();
        std::fs::write(&r.config, r#"{"reset_day": 31}"#).unwrap();
        let v = r.reading_at(at("2026-10-16T00:00:00Z")).await;
        assert_eq!(
            v["providers"][0]["bob"]["reason"],
            "reset_day must be a whole number from 1 to 28"
        );
    }

    #[tokio::test]
    async fn the_api_wins_and_falls_back() {
        let f = fixture(ROWS);
        let dir = f.dir.path();
        // A stub curl answering the profile and team calls.
        let curl = dir.join("curl");
        std::fs::write(
            &curl,
            r#"#!/bin/sh
cat > "$0.stdin"
for a in "$@"; do url=$a; done
case "$url" in
  */admin/v1/profile) printf '%s\n200' '{"instances":[{"user_id":"u1","instance_id":"i1","region_domain":"eu-de.bob.ibm.com","teams":[{"id":"t1","budget_limit":1000},{"id":"t2"}]},{"user_id":"u2","region_domain":"evil.example.com","teams":[{"id":"t3"}]}]}' ;;
  https://api.eu-de.bob.ibm.com/admin/v1/teams/t1/users/u1) printf '%s\n200' '{"usage":100}' ;;
  https://api.eu-de.bob.ibm.com/admin/v1/teams/t2/users/u1) printf '%s\n200' '{"usage":150,"budget_limit":1000}' ;;
  *) printf 'nope\n404' ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&curl, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let mut r = f.reader.clone();
        r.curl = curl.to_string_lossy().into_owned();
        r.api_key = Some("sekret".into());
        let v = r.reading_at(at("2026-10-16T00:00:00Z")).await;
        let p = parse(&v);
        assert_eq!(p.status, "known");
        assert_eq!(p.rows[0].percent_remaining, Some(87.5));
        assert_eq!(v["providers"][0]["bob"]["source"], "api");
        assert!(!v.to_string().contains("sekret"));
        let stdin = std::fs::read_to_string(dir.join("curl.stdin")).unwrap();
        assert_eq!(stdin, "header = \"Authorization: Apikey sekret\"\n");

        std::fs::write(&curl, "#!/bin/sh\nprintf 'denied\\n401'\n").unwrap();
        let v = r.reading_at(at("2026-10-16T00:00:00Z")).await;
        assert_eq!(parse(&v).status, "partial");
        assert_eq!(
            v["providers"][0]["bob"]["apiError"],
            "profile API key rejected (HTTP 401)"
        );
    }

    #[test]
    fn periods_and_offsets() {
        let cfg = BobConfig {
            reset_day: 15,
            ..BobConfig::default()
        };
        let tz = cfg.validate().unwrap();
        let (s, e) = cfg.period(tz, at("2026-01-10T00:00:00Z"));
        assert_eq!(utc_iso(s), "2025-12-15T00:00:00Z");
        assert_eq!(utc_iso(e), "2026-01-15T00:00:00Z");
        let cfg = BobConfig {
            reset_timezone: "+05:30".into(),
            ..BobConfig::default()
        };
        let tz = cfg.validate().unwrap();
        let (s, _) = cfg.period(tz, at("2026-10-16T00:00:00Z"));
        assert_eq!(utc_iso(s), "2026-09-30T18:30:00Z");
        assert!(BobConfig {
            reset_timezone: "America/New_York".into(),
            ..BobConfig::default()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn keys_hosts_and_status() {
        assert_eq!(clean_key(" 'abc' ").as_deref(), Some("abc"));
        assert_eq!(clean_key("  ").as_deref(), None);
        assert_eq!(auth_header("abc"), "Apikey abc");
        // {"a":1}
        assert_eq!(auth_header("x.eyJhIjoxfQ.y"), "Bearer x.eyJhIjoxfQ.y");
        assert_eq!(
            regional_base("").unwrap(),
            "https://api.us-east.bob.ibm.com"
        );
        assert_eq!(
            regional_base("eu-de.bob.ibm.com").unwrap(),
            "https://api.eu-de.bob.ibm.com"
        );
        assert!(regional_base("bob.ibm.com.evil.io").is_none());
        assert_eq!(percent_encode("a b/c"), "a%20b%2Fc");
        assert_eq!(
            parse_status("Monthly Usage: 12.5\nMonthly Budget: 1,500\nMonthly Usage: 13"),
            Some((13.0, Some(1500.0)))
        );
        assert_eq!(parse_status("nothing"), None);
        assert_eq!(
            aggregate(&[(1.0, Some(2.0)), (3.0, None)]),
            Some(ApiUsage {
                used: 4.0,
                limit: None
            })
        );
    }

    #[test]
    fn reads_each_tasks_spend_in_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("bob.db");
        let wt = dir.path().join("wt");
        assert_eq!(task_costs(&db, &wt).unwrap(), Vec::new());
        let con = rusqlite::Connection::open(&db).unwrap();
        con.execute_batch(
            "CREATE TABLE tasks (id TEXT PRIMARY KEY, parent_id TEXT, directory TEXT,
                task_type TEXT, created_at INTEGER, costs TEXT);",
        )
        .unwrap();
        let wt_s = wt.to_string_lossy().into_owned();
        con.execute(
            "INSERT INTO tasks VALUES
                ('b', NULL, ?1, 'normal', 2000, '{\"cost\": 1.5, \"tokensIn\": 10, \"tokensOut\": 4}'),
                ('a', NULL, ?1, 'normal', 1000, '{\"cost\": 0.25, \"model\": \"granite\"}'),
                ('s', 'b', ?1, 'subagent', 3000, '{\"cost\": 9}'),
                ('o', NULL, '/elsewhere', 'normal', 1000, '{\"cost\": 9}'),
                ('n', NULL, ?1, 'normal', 4000, NULL)",
            [&wt_s],
        )
        .unwrap();
        drop(con);
        let costs = task_costs(&db, &wt).unwrap();
        let ids: Vec<_> = costs.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(costs[0].coins, 0.25);
        assert_eq!(costs[0].model.as_deref(), Some("granite"));
        assert_eq!(costs[0].created_ms, Some(1000.0));
        assert_eq!((costs[1].input, costs[1].output), (10, 4));
    }
}
