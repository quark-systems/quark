//! Providers read with an API key (or a token that works like one), ported
//! from quota-axi: OpenRouter, DeepSeek, Z.ai, MiniMax, OpenCode Go,
//! Command Code and MiMo.
//!
//! Each finds its key in quarkd's environment, then in Pi's `auth.json`,
//! then in OpenCode's (and, for some, in its own CLI's config), and tries
//! each key it finds until one answers:
//!
//! | provider | endpoint | rows |
//! |---|---|---|
//! | `openrouter` | `GET openrouter.ai/api/v1/key` | none: a key's spend cap is shown |
//! | `deepseek` | `GET api.deepseek.com/user/balance` | none: the balance is shown |
//! | `zai` | `GET api.z.ai` (or `open.bigmodel.cn`) `/api/monitor/usage/quota/limit` | `all_models` (five-hour, weekly), `tools` (MCP month) |
//! | `minimax` | `GET api.minimax.io` (or `api.minimaxi.com`) `/v1/token_plan/remains`, or `/account/query_balance` for an `sk-api-` key | `model:<id>` per model |
//! | `opencode-go` | `GET opencode.ai/zen/go/v1/usage` | `all_models` (rolling, weekly, monthly) |
//! | `commandcode` | `GET api.commandcode.ai/alpha/whoami`, then `/alpha/billing/credits` | `included_credits` (five-hour, weekly) |
//! | `mimo` | none: MiMo has no quota endpoint, so a key only shows it is signed in | none |

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use time::OffsetDateTime;

use super::support::{self, num, pick, Cache, Http, Request};
use super::windows::{failure, reading, Window};
use super::{parse_time, LocalReading};

const FIVE_HOURS: f64 = 18_000.0;
const SEVEN_DAYS: f64 = 604_800.0;
const ZAI_HOST: &str = "https://api.z.ai";
const ZHIPU_HOST: &str = "https://open.bigmodel.cn";
const MINIMAX_GLOBAL: &str = "https://api.minimax.io";
const MINIMAX_CHINA: &str = "https://api.minimaxi.com";

/// A provider read with a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Api {
    OpenRouter,
    DeepSeek,
    Zai,
    MiniMax,
    OpenCodeGo,
    CommandCode,
    Mimo,
}

impl Api {
    pub const ALL: [Api; 7] = [
        Api::OpenRouter,
        Api::DeepSeek,
        Api::Zai,
        Api::MiniMax,
        Api::OpenCodeGo,
        Api::CommandCode,
        Api::Mimo,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Api::OpenRouter => "openrouter",
            Api::DeepSeek => "deepseek",
            Api::Zai => "zai",
            Api::MiniMax => "minimax",
            Api::OpenCodeGo => "opencode-go",
            Api::CommandCode => "commandcode",
            Api::Mimo => "mimo",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Api::OpenRouter => "OpenRouter",
            Api::DeepSeek => "DeepSeek",
            Api::Zai => "Z.ai",
            Api::MiniMax => "MiniMax",
            Api::OpenCodeGo => "OpenCode Go",
            Api::CommandCode => "Command Code",
            Api::Mimo => "MiMo",
        }
    }

    fn source(self) -> String {
        format!("quarkd: {} usage", self.label())
    }

    fn env(self) -> &'static [&'static str] {
        match self {
            Api::OpenRouter => &["OPENROUTER_API_KEY"],
            Api::DeepSeek => &["DEEPSEEK_API_KEY"],
            Api::Zai => &["ZAI_API_KEY"],
            Api::MiniMax => &["MINIMAX_API_KEY"],
            Api::OpenCodeGo => &[],
            Api::CommandCode => &["COMMAND_CODE_API_KEY", "COMMANDCODE_API_KEY"],
            Api::Mimo => &["MIMO_API_KEY"],
        }
    }

    /// Which provider a Pi or OpenCode `auth.json` entry signs in to, and
    /// the API base it implies.
    pub fn for_entry(name: &str) -> Option<(Api, Option<&'static str>)> {
        Some(match name {
            "openrouter" => (Api::OpenRouter, None),
            "deepseek" => (Api::DeepSeek, None),
            "zai" | "zai-coding-plan" | "z-ai" | "z.ai" => (Api::Zai, Some(ZAI_HOST)),
            "zhipu" | "zhipuai" | "zhipuai-coding-plan" => (Api::Zai, Some(ZHIPU_HOST)),
            "minimax" => (Api::MiniMax, None),
            "minimax-cn" => (Api::MiniMax, Some(MINIMAX_CHINA)),
            "opencode-go" | "opencode" => (Api::OpenCodeGo, None),
            "commandcode" => (Api::CommandCode, None),
            n if n == "xiaomi" || n.starts_with("xiaomi-token-plan") => (Api::Mimo, None),
            _ => return None,
        })
    }
}

/// A key and, where the provider has regions, the base it answers on.
#[derive(Debug, Clone, PartialEq)]
pub struct Key {
    pub token: String,
    pub base: Option<String>,
}

/// The key in a Pi or OpenCode `auth.json` entry: an API key, else an
/// OAuth access token.
pub fn entry_key(e: &Value) -> Option<String> {
    ["key", "apiKey", "api_key", "access", "token", "accessToken"]
        .iter()
        .find_map(|k| support::text(e.get(*k)))
}

/// Reads one key-based provider.
#[derive(Debug, Clone)]
pub struct KeyedUsage {
    pub api: Api,
    pub user_home: Option<PathBuf>,
    pub pi_dir: Option<PathBuf>,
    /// OpenCode's `auth.json`.
    pub opencode: Option<PathBuf>,
    /// Keys from quarkd's own environment, in [`Api::env`] order.
    pub env_keys: Vec<String>,
    pub http: Http,
    pub cache: Cache,
}

impl KeyedUsage {
    pub fn from_env(api: Api) -> Self {
        let data = support::env("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| support::user_home().map(|h| h.join(".local").join("share")));
        Self {
            user_home: support::user_home(),
            pi_dir: support::pi_dir(),
            opencode: data.map(|d| d.join("opencode").join("auth.json")),
            env_keys: api.env().iter().filter_map(|v| support::env(v)).collect(),
            ..Self::new(api)
        }
    }

    pub fn new(api: Api) -> Self {
        Self {
            api,
            user_home: None,
            pi_dir: None,
            opencode: None,
            env_keys: Vec::new(),
            http: Http::default(),
            cache: Cache::default(),
        }
    }

    fn fail(&self, status: &str, error: &str) -> Value {
        failure(
            self.api.id(),
            &self.api.source(),
            OffsetDateTime::now_utc(),
            status,
            error,
            None,
        )
    }

    /// Every key this machine has for the provider, in the order tried.
    fn keys(&self) -> Vec<Key> {
        let mut keys: Vec<Key> = Vec::new();
        let mut push = |token: String, base: Option<String>| {
            if !keys.iter().any(|k| k.token == token) {
                keys.push(Key { token, base });
            }
        };
        let env_base = match self.api {
            Api::MiniMax => support::env("MINIMAX_BASE_URL").filter(|b| minimax_base(b)),
            _ => None,
        };
        for k in &self.env_keys {
            push(k.clone(), env_base.clone());
        }
        for file in [
            self.pi_dir.as_ref().map(|d| d.join("auth.json")),
            self.opencode.clone(),
        ]
        .into_iter()
        .flatten()
        {
            if let Ok(Some(v)) = support::read_json(&file) {
                for (name, e) in v.as_object().into_iter().flatten() {
                    if let Some((api, base)) = Api::for_entry(name) {
                        if api == self.api {
                            if let Some(t) = entry_key(e) {
                                push(t, base.map(str::to_string));
                            }
                        }
                    }
                }
            }
        }
        let home = self.user_home.as_deref();
        match self.api {
            Api::MiniMax => {
                let dir = support::env("MMX_CONFIG_DIR")
                    .map(PathBuf::from)
                    .or_else(|| home.map(|h| h.join(".mmx")));
                if let Some(Ok(Some(v))) = dir.map(|d| support::read_json(&d.join("config.json"))) {
                    let base = support::text(v.get("base_url"))
                        .or_else(|| support::text(v.pointer("/oauth/resource_url")))
                        .filter(|b| minimax_base(b))
                        .or_else(|| {
                            support::text(v.get("region"))
                                .filter(|r| r.eq_ignore_ascii_case("cn"))
                                .map(|_| MINIMAX_CHINA.to_string())
                        });
                    for t in [
                        support::text(v.pointer("/oauth/access_token")),
                        support::text(v.get("api_key")),
                    ]
                    .into_iter()
                    .flatten()
                    {
                        push(t, base.clone());
                    }
                }
            }
            Api::CommandCode => {
                for (file, entry) in [
                    (home.map(|h| h.join(".commandcode/auth.json")), None),
                    (
                        home.map(|h| h.join(".omp/agent/auth.json")),
                        Some("commandcode"),
                    ),
                ] {
                    if let Some(Ok(Some(v))) = file.as_deref().map(support::read_json) {
                        let e = match entry {
                            Some(n) => v.get(n).cloned().unwrap_or(Value::Null),
                            None => v,
                        };
                        if let Some(t) = entry_key(&e) {
                            push(t, None);
                        }
                    }
                }
            }
            _ => {}
        }
        keys
    }

    /// The reading for this machine's logins: the first key that answers.
    pub async fn reading(&self) -> Value {
        let key = format!("keyed\n{}", self.api.id());
        if let Some(v) = self.cache.get(&key) {
            return v;
        }
        let v = self.read_now().await;
        self.cache.put(&key, &v);
        v
    }

    async fn read_now(&self) -> Value {
        let mut last = None;
        for k in self.keys() {
            match self.read_key(&k).await {
                Ok(v) => return v,
                Err(e) => last = Some(e),
            }
        }
        match last {
            Some((s, e)) => self.fail(s, &e),
            None => self.fail(
                "auth_required",
                &format!("No {} key here", self.api.label()),
            ),
        }
    }

    /// The reading for one key, or why it gave none.
    pub async fn read_key(&self, k: &Key) -> Result<Value, (&'static str, String)> {
        let now = OffsetDateTime::now_utc();
        let label = self.api.label();
        let get = |url: String| {
            let req = Request::get(url)
                .bearer(&k.token)
                .header("Accept", "application/json")
                .header("Accept-Language", "en-US,en")
                .header("User-Agent", "quarkd");
            async move {
                let r = self
                    .http
                    .send(&req)
                    .await
                    .map_err(|e| ("unavailable", format!("{label} quota unavailable: {e}")))?;
                if !r.ok() {
                    return Err(support::http_failure(label, r.status));
                }
                r.json()
                    .ok_or(("unavailable", format!("{label} quota response invalid")))
            }
        };
        let invalid = || ("unavailable", format!("{label} quota response invalid"));
        let parsed = match self.api {
            Api::Mimo => Some(mimo_reading(now)),
            Api::OpenRouter => {
                openrouter_reading(&get("https://openrouter.ai/api/v1/key".into()).await?, now)
            }
            Api::DeepSeek => deepseek_reading(
                &get("https://api.deepseek.com/user/balance".into()).await?,
                now,
            ),
            Api::Zai => {
                let host = k.base.as_deref().unwrap_or(ZAI_HOST);
                let v = get(format!("{host}/api/monitor/usage/quota/limit")).await?;
                if v.get("success") == Some(&json!(false)) && num(v.get("code")) == Some(1000.0) {
                    return Err(("auth_required", format!("{label} rejected the key")));
                }
                zai_reading(&v, now)
            }
            Api::MiniMax => {
                let base = k.base.as_deref().unwrap_or(MINIMAX_GLOBAL);
                let path = if k.token.starts_with("sk-api-") {
                    "/account/query_balance"
                } else {
                    "/v1/token_plan/remains"
                };
                minimax_reading(
                    &get(format!("{base}{path}")).await?,
                    base == MINIMAX_CHINA,
                    now,
                )
            }
            Api::OpenCodeGo => opencode_go_reading(
                &get("https://opencode.ai/zen/go/v1/usage".into()).await?,
                now,
            ),
            Api::CommandCode => {
                let who = get("https://api.commandcode.ai/alpha/whoami?limits=1".into()).await?;
                let data = who.get("data").unwrap_or(&who);
                let org = data
                    .get("org")
                    .or_else(|| data.get("organization"))
                    .and_then(|o| support::text(o.get("id")));
                let url = match org {
                    Some(o) => format!(
                        "https://api.commandcode.ai/alpha/billing/credits?orgId={}",
                        o.replace(
                            |c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_',
                            ""
                        )
                    ),
                    None => "https://api.commandcode.ai/alpha/billing/credits".into(),
                };
                commandcode_reading(&get(url).await?, now)
            }
        };
        parsed.ok_or_else(invalid)
    }
}

fn minimax_base(b: &str) -> bool {
    let b = b.trim_end_matches('/');
    b == MINIMAX_GLOBAL || b == MINIMAX_CHINA
}

fn with_plan(plan: Option<String>) -> Map<String, Value> {
    let mut m = Map::new();
    if let Some(p) = plan {
        m.insert("plan".into(), json!(p));
    }
    m
}

fn mimo_reading(now: OffsetDateTime) -> Value {
    reading(
        "mimo",
        &Api::Mimo.source(),
        now,
        &[],
        &[],
        with_plan(Some("signed in (MiMo reports no quota)".into())),
    )
}

fn money(amount: f64, unit: &str) -> String {
    match unit {
        "usd" => format!("${amount:.2}"),
        "cny" => format!("¥{amount:.2}"),
        u => format!("{amount:.2} {u}"),
    }
}

fn openrouter_reading(v: &Value, now: OffsetDateTime) -> Option<Value> {
    let d = v.get("data")?;
    let unlimited = d.get("limit").is_some_and(Value::is_null);
    let limit = num(d.get("limit")).filter(|l| *l >= 0.0);
    if !unlimited && limit.is_none() {
        return None;
    }
    let remaining = num(d.get("limit_remaining"));
    let mut windows = Vec::new();
    let mut extra = Map::new();
    if let (Some(limit), Some(rem)) = (limit, remaining) {
        let used = (limit - rem).max(0.0);
        let pct = if limit > 0.0 {
            used / limit * 100.0
        } else {
            100.0
        };
        windows.push(Window::new("key-limit", "key spend cap", "credits", pct));
        extra.insert("credits".into(), json!({"remaining": rem, "unit": "usd"}));
        extra.insert(
            "plan".into(),
            json!(format!(
                "{} of {} key cap left",
                money(rem, "usd"),
                money(limit, "usd")
            )),
        );
    } else if unlimited {
        extra.insert("credits".into(), json!({"unlimited": true, "unit": "usd"}));
        extra.insert("plan".into(), json!("key with no spend cap"));
    }
    Some(reading(
        "openrouter",
        &Api::OpenRouter.source(),
        now,
        &windows,
        &[],
        extra,
    ))
}

fn deepseek_reading(v: &Value, now: OffsetDateTime) -> Option<Value> {
    v.get("is_available")?.as_bool()?;
    let infos = v.get("balance_infos").and_then(Value::as_array);
    let find = |cur: &str| {
        infos?
            .iter()
            .find(|i| i.get("currency").and_then(Value::as_str) == Some(cur))
            .and_then(|i| num(i.get("total_balance")))
    };
    let mut extra = Map::new();
    if let Some((amount, unit)) = find("USD")
        .map(|a| (a, "usd"))
        .or_else(|| find("CNY").map(|a| (a, "cny")))
    {
        extra.insert("credits".into(), json!({"remaining": amount, "unit": unit}));
        extra.insert(
            "plan".into(),
            json!(format!("{} balance", money(amount, unit))),
        );
    }
    Some(reading(
        "deepseek",
        &Api::DeepSeek.source(),
        now,
        &[],
        &[],
        extra,
    ))
}

fn zai_reading(v: &Value, now: OffsetDateTime) -> Option<Value> {
    let data = v.get("data").filter(|d| d.is_object()).unwrap_or(v);
    let mut windows: Vec<Window> = Vec::new();
    for (i, e) in data.get("limits")?.as_array()?.iter().enumerate() {
        let used = num(e.get("percentage")).or_else(|| {
            let total = num(e.get("usage")).filter(|u| *u > 0.0)?;
            Some(num(e.get("currentValue"))? / total * 100.0)
        });
        let Some(used) = used else { continue };
        let reset = e.get("nextResetTime").and_then(parse_time);
        let ty = e.get("type").and_then(Value::as_str).unwrap_or("");
        let (unit, number) = (num(e.get("unit")), num(e.get("number")));
        let tokens = ty == "TOKENS_LIMIT" || ty == "CREDIT_LIMIT";
        let w = match (tokens, unit, number) {
            (true, Some(3.0), Some(5.0)) => {
                Window::new("five_hour", "session", "session", used).lasting(Some(FIVE_HOURS))
            }
            (true, Some(6.0), Some(1.0)) => {
                Window::new("weekly", "week", "weekly", used).lasting(Some(SEVEN_DAYS))
            }
            _ if ty == "TIME_LIMIT" => Window::new("mcp_month", "MCP month", "monthly", used),
            _ => Window::new(
                &format!("limit:{}", i + 1),
                &format!("limit {}", i + 1),
                "unknown",
                used,
            ),
        };
        if windows.iter().any(|x| x.id == w.id) {
            continue;
        }
        windows.push(w.resets(reset));
    }
    if windows.is_empty() {
        return None;
    }
    let of = |ids: &[&str]| -> Vec<&Window> {
        windows
            .iter()
            .filter(|w| ids.contains(&w.id.as_str()))
            .collect()
    };
    Some(reading(
        "zai",
        &Api::Zai.source(),
        now,
        &windows,
        &[
            ("all_models".into(), of(&["five_hour", "weekly"])),
            ("tools".into(), of(&["mcp_month"])),
        ],
        with_plan(support::text(data.get("level"))),
    ))
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.trim().to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

fn minimax_reading(v: &Value, china: bool, now: OffsetDateTime) -> Option<Value> {
    let data = v.get("data").filter(|d| d.is_object()).unwrap_or(v);
    if let Some(balance) = num(data.get("available_amount")) {
        let unit = if china { "cny" } else { "usd" };
        let mut extra = with_plan(Some(format!("{} balance", money(balance, unit))));
        extra.insert(
            "credits".into(),
            json!({"remaining": balance, "unit": unit}),
        );
        return Some(reading(
            "minimax",
            &Api::MiniMax.source(),
            now,
            &[],
            &[],
            extra,
        ));
    }
    let rows = data.get("model_remains")?.as_array()?;
    let mut windows: Vec<(String, Window)> = Vec::new();
    for r in rows {
        let Some(name) = support::text(r.get("model_name")) else {
            continue;
        };
        let id = slug(&name);
        for (prefix, start, end) in [
            ("current_interval", "start_time", "end_time"),
            ("current_weekly", "weekly_start_time", "weekly_end_time"),
        ] {
            let f = |k: &str| num(r.get(format!("{prefix}_{k}").as_str()));
            let (status, total, count) = (f("status"), f("total_count"), f("usage_count"));
            if status == Some(3.0) && total == Some(0.0) && count == Some(0.0) {
                continue;
            }
            let remaining = f("remaining_percent").or_else(|| match (count, total) {
                // A count with no percent is what remains (as MiniMax's CLI reads it).
                (Some(c), Some(t)) if t > 0.0 && (0.0..=t).contains(&c) => Some(c / t * 100.0),
                _ => (status == Some(2.0)).then_some(0.0),
            });
            let Some(remaining) = remaining else { continue };
            let s = r.get(start).and_then(parse_time);
            let e = r.get(end).and_then(parse_time);
            let secs = s
                .zip(e)
                .map(|(s, e)| (e - s).as_seconds_f64())
                .filter(|x| *x > 0.0);
            let suffix = match secs {
                Some(FIVE_HOURS) => "5h".to_string(),
                Some(SEVEN_DAYS) => "7d".to_string(),
                _ if prefix == "current_weekly" => "weekly".to_string(),
                _ => "interval".to_string(),
            };
            windows.push((
                format!("model:{id}"),
                Window::new(
                    &format!("model:{id}:{suffix}"),
                    &format!("{name} {suffix}"),
                    "model",
                    100.0 - remaining.clamp(0.0, 100.0),
                )
                .resets(e)
                .lasting(secs),
            ));
        }
    }
    let mut scopes: Vec<(String, Vec<&Window>)> = Vec::new();
    for (scope, w) in &windows {
        match scopes.iter_mut().find(|(s, _)| s == scope) {
            Some((_, ws)) => ws.push(w),
            None => scopes.push((scope.clone(), vec![w])),
        }
    }
    let plan = ["plan", "plan_name", "planName"]
        .iter()
        .find_map(|k| support::text(data.get(*k)));
    let ws: Vec<Window> = windows.iter().map(|(_, w)| w.clone()).collect();
    Some(reading(
        "minimax",
        &Api::MiniMax.source(),
        now,
        &ws,
        &scopes,
        with_plan(plan),
    ))
}

fn opencode_go_reading(v: &Value, now: OffsetDateTime) -> Option<Value> {
    let usage = v.get("usage");
    let mut windows = Vec::new();
    for (names, id, label, kind) in [
        (
            ["rollingUsage", "rolling"],
            "rolling",
            "rolling 5 hours",
            "session",
        ),
        (["weeklyUsage", "weekly"], "weekly", "week", "weekly"),
        (["monthlyUsage", "monthly"], "monthly", "month", "monthly"),
    ] {
        let Some(r) = names
            .iter()
            .find_map(|n| usage.and_then(|u| u.get(*n)).or_else(|| v.get(*n)))
            .filter(|r| r.is_object())
        else {
            continue;
        };
        let remaining = num(pick(r, &["percentRemaining", "remainingPercent"]))
            .or_else(|| {
                num(pick(
                    r,
                    &["percent", "percentUsed", "usedPercent", "usagePercent"],
                ))
                .map(|u| 100.0 - u)
            })
            .or_else(|| {
                (r.get("status").and_then(Value::as_str) == Some("rate-limited")).then_some(0.0)
            });
        let Some(remaining) = remaining else { continue };
        let reset = pick(r, &["resetsAt", "resetAt", "reset_at", "nextResetTime"])
            .and_then(parse_time)
            .or_else(|| {
                let s = num(pick(r, &["resetInSec", "reset_in_sec"])).filter(|s| *s >= 0.0)?;
                Some(now + time::Duration::seconds_f64(s))
            });
        let secs = num(pick(
            r,
            &["windowSeconds", "window_seconds", "durationSeconds"],
        ))
        .or(match id {
            "rolling" => Some(FIVE_HOURS),
            "weekly" => Some(SEVEN_DAYS),
            _ => reset.map(|_| 30.0 * 86_400.0),
        });
        windows.push(
            Window::new(id, label, kind, 100.0 - remaining.clamp(0.0, 100.0))
                .resets(reset)
                .lasting(secs),
        );
    }
    if windows.is_empty() {
        return None;
    }
    let plan = ["planName", "plan_name", "plan"]
        .iter()
        .find_map(|k| support::text(v.get(*k)))
        .unwrap_or_else(|| "OpenCode Go".into());
    let all: Vec<&Window> = windows.iter().collect();
    Some(reading(
        "opencode-go",
        &Api::OpenCodeGo.source(),
        now,
        &windows,
        &[("all_models".into(), all)],
        with_plan(Some(plan)),
    ))
}

fn commandcode_reading(v: &Value, now: OffsetDateTime) -> Option<Value> {
    let data = v.get("data").filter(|d| d.is_object()).unwrap_or(v);
    if !data.is_object() {
        return None;
    }
    let mut windows = Vec::new();
    let limits = data.get("windowLimits");
    if limits
        .and_then(|l| l.get("limited"))
        .and_then(Value::as_bool)
        != Some(false)
    {
        for (key, id, label, kind, secs) in [
            ("fiveHour", "five_hour", "5-hour", "session", FIVE_HOURS),
            ("weekly", "weekly", "week", "weekly", SEVEN_DAYS),
        ] {
            let Some(e) = limits.and_then(|l| l.get(key)) else {
                continue;
            };
            let (Some(used), Some(cap)) =
                (num(e.get("used")), num(e.get("cap")).filter(|c| *c > 0.0))
            else {
                continue;
            };
            let reset =
                pick(e, &["resetAt", "resetTime", "reset_at", "reset_time"]).and_then(parse_time);
            windows.push(
                Window::new(id, label, kind, used / cap * 100.0)
                    .resets(reset)
                    .lasting(Some(secs)),
            );
        }
    }
    let credits = ["monthlyCredits", "purchasedCredits", "freeCredits"]
        .iter()
        .map(|k| num(data.get(*k)))
        .sum::<Option<f64>>();
    if windows.is_empty() && credits.is_none() {
        return None;
    }
    let mut extra = Map::new();
    if let Some(c) = credits {
        extra.insert("credits".into(), json!({"remaining": c, "unit": "credits"}));
        extra.insert("plan".into(), json!(format!("{c:.0} credits left")));
    }
    let bound: Vec<&Window> = if windows.len() == 2 {
        windows.iter().collect()
    } else {
        Vec::new()
    };
    Some(reading(
        "commandcode",
        &Api::CommandCode.source(),
        now,
        &windows,
        &[("included_credits".into(), bound)],
        extra,
    ))
}

#[async_trait]
impl LocalReading for KeyedUsage {
    fn provider(&self) -> &str {
        self.api.id()
    }

    async fn read(&self) -> Value {
        self.reading().await
    }

    async fn read_with_key(&self, key: &str) -> Option<Value> {
        let k = Key {
            token: key.to_string(),
            base: None,
        };
        Some(match self.read_key(&k).await {
            Ok(v) => v,
            Err((s, e)) => self.fail(s, &e),
        })
    }

    async fn read_account(&self, config_dir: Option<&Path>, _grant: bool) -> Option<Value> {
        match config_dir {
            None => Some(self.reading().await),
            Some(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    fn valid(v: &Value) {
        assert!(crate::quota::valid(v), "{v}");
    }

    #[test]
    fn reads_balances_and_caps() {
        let v = openrouter_reading(
            &json!({"data": {"label": "k", "limit": 20, "limit_remaining": 5}}),
            now(),
        )
        .unwrap();
        valid(&v);
        assert_eq!(
            v["providers"][0]["windows"][0]["percentRemaining"],
            json!(25)
        );
        assert_eq!(v["providers"][0]["plan"], "$5.00 of $20.00 key cap left");
        let v = openrouter_reading(&json!({"data": {"limit": null}}), now()).unwrap();
        assert_eq!(v["providers"][0]["plan"], "key with no spend cap");
        let v = deepseek_reading(
            &json!({"is_available": true, "balance_infos": [{"currency": "CNY", "total_balance": "9.50"}]}),
            now(),
        )
        .unwrap();
        valid(&v);
        assert_eq!(v["providers"][0]["plan"], "¥9.50 balance");
        assert!(deepseek_reading(&json!({}), now()).is_none());
        valid(&mimo_reading(now()));
    }

    #[test]
    fn reads_zai_windows() {
        let v = zai_reading(
            &json!({"success": true, "data": {"level": "pro", "limits": [
                {"type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 40},
                {"type": "TOKENS_LIMIT", "unit": 6, "number": 1, "percentage": 10,
                 "nextResetTime": (now().unix_timestamp() + 86_400) * 1000},
                {"type": "TIME_LIMIT", "usage": 100, "currentValue": 25},
            ]}}),
            now(),
        )
        .unwrap();
        valid(&v);
        let p = &v["providers"][0];
        assert_eq!(p["plan"], "pro");
        let rows = p["quotaSemantics"]["effectiveAvailability"]
            .as_array()
            .unwrap();
        assert_eq!(rows[0]["effectivePercentRemaining"], json!(60));
        assert_eq!(rows[1]["scope"], "tools");
        assert_eq!(rows[1]["effectivePercentRemaining"], json!(75));
    }

    #[test]
    fn reads_minimax_models_and_balance() {
        let t = now().unix_timestamp() * 1000;
        let v = minimax_reading(
            &json!({"data": {"model_remains": [{
                "model_name": "MiniMax-M2",
                "start_time": t - 3_600_000, "end_time": t + 14_400_000,
                "current_interval_total_count": 100, "current_interval_usage_count": 80,
                "current_weekly_status": 3, "current_weekly_total_count": 0, "current_weekly_usage_count": 0,
            }]}}),
            false,
            now(),
        )
        .unwrap();
        valid(&v);
        let row = &v["providers"][0]["quotaSemantics"]["effectiveAvailability"][0];
        assert_eq!(row["scope"], "model:minimax-m2");
        assert_eq!(row["effectivePercentRemaining"], json!(80));
        let b = minimax_reading(&json!({"data": {"available_amount": 3}}), true, now()).unwrap();
        assert_eq!(b["providers"][0]["plan"], "¥3.00 balance");
    }

    #[test]
    fn reads_opencode_go_and_commandcode() {
        let v = opencode_go_reading(
            &json!({"usage": {"rollingUsage": {"percent": 20, "resetInSec": 600},
                              "weeklyUsage": {"percentRemaining": 70},
                              "monthlyUsage": {"usagePercent": 50}}}),
            now(),
        )
        .unwrap();
        valid(&v);
        let row = &v["providers"][0]["quotaSemantics"]["effectiveAvailability"][0];
        assert_eq!(row["effectivePercentRemaining"], json!(50));
        let v = commandcode_reading(
            &json!({"monthlyCredits": 10, "purchasedCredits": 5, "freeCredits": 0,
                    "windowLimits": {"limited": true, "fiveHour": {"used": 1, "cap": 4},
                                     "weekly": {"used": 3, "cap": 4}}}),
            now(),
        )
        .unwrap();
        valid(&v);
        let p = &v["providers"][0];
        assert_eq!(p["plan"], "15 credits left");
        assert_eq!(
            p["quotaSemantics"]["effectiveAvailability"][0]["effectivePercentRemaining"],
            json!(25)
        );
    }

    #[test]
    fn maps_auth_entries() {
        assert_eq!(
            Api::for_entry("zhipuai"),
            Some((Api::Zai, Some(ZHIPU_HOST)))
        );
        assert_eq!(
            Api::for_entry("xiaomi-token-plan-sgp").map(|x| x.0),
            Some(Api::Mimo)
        );
        assert_eq!(Api::for_entry("anthropic"), None);
        assert_eq!(
            entry_key(&json!({"type": "api", "key": "k"})),
            Some("k".into())
        );
    }

    #[tokio::test]
    async fn tries_each_key_in_turn() {
        let tmp = tempfile::tempdir().unwrap();
        let pi = tmp.path().join("pi");
        std::fs::create_dir_all(&pi).unwrap();
        std::fs::write(
            pi.join("auth.json"),
            r#"{"zhipuai":{"type":"api_key","key":"z"}}"#,
        )
        .unwrap();
        let mut r = KeyedUsage::new(Api::Zai);
        r.http.curl = support::stub_curl(
            tmp.path(),
            &[(
                "open.bigmodel.cn",
                200,
                br#"{"data":{"limits":[{"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":1}]}}"#,
            )],
        );
        r.cache = Cache::none();
        let v = r.read().await;
        assert_eq!(v["providers"][0]["state"]["status"], "auth_required");
        r.pi_dir = Some(pi);
        let v = r.read().await;
        assert_eq!(v["providers"][0]["state"]["status"], "fresh", "{v}");
        r.env_keys = vec!["bad".into()];
        let v = r.read().await;
        // The environment's key fails on api.z.ai; Pi's still answers.
        assert_eq!(v["providers"][0]["state"]["status"], "fresh", "{v}");
    }
}
