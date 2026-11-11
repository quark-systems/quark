//! GitHub Copilot's monthly allowances, read the way quota-axi read them,
//! without it.
//!
//! The login is the first of: the Copilot CLI's (the account `config.json`
//! in `~/.copilot` last signed in, whose token is in the macOS Keychain item
//! `copilot-cli`), the editors' `github-copilot/apps.json`, and the GitHub
//! CLI's (`gh`'s `hosts.yml`, else `gh auth token`). The token goes to
//! GitHub's `copilot_internal/user`, which answers the plan and one monthly
//! snapshot per allowance (chat, completions, premium requests).
//!
//! The allowances are separate budgets, none of which bounds every model, so
//! the reading has windows but no scope rows (as quota-axi's had).

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use time::OffsetDateTime;

use super::claude::KEYCHAIN_REASON;
use super::support::{self, num, Cache, Http, Keychain, Request};
use super::windows::{failure, reading, Window};
use super::{parse_time, LocalReading};

pub const PROVIDER: &str = "copilot";
const SOURCE: &str = "quarkd: GitHub Copilot usage";
const URL: &str = "https://api.github.com/copilot_internal/user";
const KEYCHAIN_SERVICE: &str = "copilot-cli";
const SIGN_IN: &str =
    "GitHub Copilot is not signed in here; run `copilot` and `/login`, or `gh auth login`";

/// Reads GitHub Copilot's allowances.
#[derive(Debug, Clone)]
pub struct CopilotUsage {
    pub copilot_home: Option<PathBuf>,
    pub apps_json: Option<PathBuf>,
    pub gh_hosts: Option<PathBuf>,
    /// The GitHub CLI, asked for its keyring token; `None` never runs it.
    pub gh: Option<String>,
    pub keychain: Keychain,
    pub http: Http,
    pub url: String,
    pub cache: Cache,
}

impl CopilotUsage {
    pub fn from_env() -> Self {
        let config = support::config_home();
        Self {
            copilot_home: support::env("COPILOT_HOME")
                .and_then(|d| support::expand_home(&d))
                .or_else(|| support::user_home().map(|h| h.join(".copilot"))),
            apps_json: config.as_ref().map(|c| c.join("github-copilot/apps.json")),
            gh_hosts: support::env("GH_CONFIG_DIR")
                .map(PathBuf::from)
                .or_else(|| config.map(|c| c.join("gh")))
                .map(|d| d.join("hosts.yml")),
            ..Self::new()
        }
    }

    pub fn new() -> Self {
        Self {
            copilot_home: None,
            apps_json: None,
            gh_hosts: None,
            gh: Some("gh".into()),
            keychain: Keychain::default(),
            http: Http::default(),
            url: URL.into(),
            cache: Cache::default(),
        }
    }

    /// The Copilot CLI's token, or `Err` when the Keychain withheld it.
    async fn cli_token(&self, grant: bool) -> Result<Option<String>, ()> {
        let Some(home) = &self.copilot_home else {
            return Ok(None);
        };
        let Ok(text) = std::fs::read_to_string(home.join("config.json")) else {
            return Ok(None);
        };
        let text: String = text
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let Some(user) = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("lastLoggedInUser").cloned())
        else {
            return Ok(None);
        };
        let (Some(host), Some(login)) = (
            support::text(user.get("host")),
            support::text(user.get("login")),
        ) else {
            return Ok(None);
        };
        if host != "https://github.com" {
            return Ok(None);
        }
        self.keychain
            .read(KEYCHAIN_SERVICE, Some(&format!("{host}:{login}")), grant)
            .await
    }

    fn apps_token(&self) -> Option<String> {
        let v = support::read_json(self.apps_json.as_ref()?).ok()??;
        let entries = v.as_object()?;
        entries
            .iter()
            .filter(|(k, _)| k.starts_with("github.com"))
            .chain(entries.iter())
            .find_map(|(_, e)| support::text(e.get("oauth_token")))
    }

    fn gh_hosts_token(&self) -> Option<String> {
        hosts_token(&std::fs::read_to_string(self.gh_hosts.as_ref()?).ok()?)
    }

    async fn gh_token(&self) -> Option<String> {
        let gh = self.gh.as_ref()?;
        let argv = [
            gh.clone(),
            "auth".into(),
            "token".into(),
            "--hostname".into(),
            "github.com".into(),
        ];
        match super::run_with(&argv, None, &[], Duration::from_secs(10)).await {
            Ok((true, out)) => out
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.contains(' '))
                .map(str::to_string),
            _ => None,
        }
    }

    pub async fn reading(&self, grant: bool) -> Value {
        if !grant {
            if let Some(v) = self.cache.get(PROVIDER) {
                return v;
            }
        }
        let v = self.read_now(grant).await;
        self.cache.put(PROVIDER, &v);
        v
    }

    async fn read_now(&self, grant: bool) -> Value {
        let now = OffsetDateTime::now_utc();
        let fail = |status: &str, error: &str, reason: Option<&str>| {
            failure(PROVIDER, SOURCE, now, status, error, reason)
        };
        let mut withheld = false;
        let mut tried = Vec::new();
        match self.cli_token(grant).await {
            Ok(Some(t)) => tried.push(t),
            Ok(None) => {}
            Err(()) => withheld = true,
        }
        tried.extend(self.apps_token());
        tried.extend(self.gh_hosts_token());
        let mut last: Option<(&str, String)> = None;
        for t in &tried {
            match self.read_with(t, now).await {
                Ok(v) => return v,
                Err(e) => last = Some(e),
            }
        }
        // `gh`'s keyring login, asked for only when no other login answered.
        if last.as_ref().is_none_or(|(s, _)| *s == "auth_required") {
            if let Some(t) = self.gh_token().await.filter(|t| !tried.contains(t)) {
                match self.read_with(&t, now).await {
                    Ok(v) => return v,
                    Err(e) => last = Some(e),
                }
            }
        }
        match last {
            Some((s, e)) => fail(s, &e, None),
            None if withheld => fail(
                "unavailable",
                &support::withheld_message("the Copilot CLI's sign-in"),
                Some(KEYCHAIN_REASON),
            ),
            None => fail("auth_required", SIGN_IN, None),
        }
    }

    async fn read_with(
        &self,
        token: &str,
        now: OffsetDateTime,
    ) -> Result<Value, (&'static str, String)> {
        let req = Request::get(&self.url)
            .bearer(token)
            .header("Accept", "application/json")
            .header("User-Agent", "GitHubCopilotCLI/1.0");
        match self.http.send(&req).await {
            Ok(r) if r.ok() => r
                .json()
                .and_then(|v| user_reading(&v, now))
                .ok_or(("unavailable", "GitHub Copilot quota unavailable".into())),
            Ok(r) if r.status == 403 && r.header("x-ratelimit-remaining") == Some("0") => Err((
                "rate_limited",
                "GitHub rate limited the Copilot quota request".into(),
            )),
            Ok(r) if r.status == 404 => Err((
                "auth_required",
                "This GitHub sign-in has no Copilot subscription".into(),
            )),
            Ok(r) => Err(support::http_failure("GitHub Copilot", r.status)),
            Err(e) => Err((
                "unavailable",
                format!("GitHub Copilot quota unavailable: {e}"),
            )),
        }
    }
}

impl Default for CopilotUsage {
    fn default() -> Self {
        Self::new()
    }
}

/// `github.com`'s `oauth_token` in `gh`'s `hosts.yml`.
fn hosts_token(yaml: &str) -> Option<String> {
    let mut in_host = false;
    for line in yaml.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let l = line.trim();
        if indent == 0 {
            in_host = l
                .trim_end_matches(':')
                .trim_matches(|c| c == '"' || c == '\'')
                == "github.com";
        } else if in_host && indent <= 4 {
            if let Some(v) = l.strip_prefix("oauth_token:") {
                let v = v.trim().trim_matches(|c| c == '"' || c == '\'');
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

fn user_reading(v: &Value, now: OffsetDateTime) -> Option<Value> {
    let fallback = v.get("quota_reset_date_utc").and_then(parse_time);
    let mut windows = Vec::new();
    for (id, s) in v
        .get("quota_snapshots")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        if s.get("unlimited").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(left) = num(s.get("percent_remaining")) else {
            continue;
        };
        let reset = s.get("quota_reset_at").and_then(parse_time).or(fallback);
        windows.push(Window::new(id, &id.replace('_', " "), "monthly", 100.0 - left).resets(reset));
    }
    let plan = ["copilot_plan", "access_type_sku", "sku"]
        .iter()
        .find_map(|k| support::text(v.get(*k)));
    if windows.is_empty() && plan.is_none() && v.get("login").is_none() {
        return None;
    }
    let mut extra = Map::new();
    if let Some(p) = plan {
        extra.insert("plan".into(), json!(p));
    }
    Some(reading(PROVIDER, SOURCE, now, &windows, &[], extra))
}

#[async_trait]
impl LocalReading for CopilotUsage {
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
    fn reads_snapshots_and_hosts() {
        let now = OffsetDateTime::now_utc();
        let v = json!({
            "copilot_plan": "individual",
            "quota_reset_date_utc": "2026-12-01T00:00:00Z",
            "quota_snapshots": {
                "chat": {"unlimited": true, "percent_remaining": 100},
                "premium_interactions": {"percent_remaining": 62.5},
            },
        });
        let r = user_reading(&v, now).unwrap();
        assert!(crate::quota::valid(&r), "{r}");
        let p = &r["providers"][0];
        assert_eq!(p["plan"], "individual");
        assert_eq!(p["windows"][0]["id"], "premium_interactions");
        assert_eq!(p["windows"][0]["percentRemaining"], json!(62.5));
        assert_eq!(p["quotaSemantics"]["status"], "unknown");

        let yaml = "ghe.io:\n    oauth_token: other\ngithub.com:\n    users:\n        me:\n            oauth_token: deep\n    oauth_token: gho_x\n    user: me\n";
        assert_eq!(hosts_token(yaml), Some("gho_x".into()));
        assert_eq!(hosts_token("github.com:\n    user: me\n"), None);
    }

    #[tokio::test]
    async fn tries_each_login() {
        let tmp = tempfile::tempdir().unwrap();
        let body = br#"{"copilot_plan":"business","quota_snapshots":{"premium_interactions":{"percent_remaining":10}}}"#;
        let mut r = CopilotUsage::new();
        r.http.curl = support::stub_curl(tmp.path(), &[("copilot_internal", 200, body)]);
        r.cache = Cache::none();
        r.keychain.enabled = false;
        r.gh = None;
        r.gh_hosts = Some(tmp.path().join("hosts.yml"));
        let v = r.read().await;
        assert_eq!(v["providers"][0]["state"]["status"], "auth_required");
        std::fs::write(
            tmp.path().join("hosts.yml"),
            "github.com:\n    oauth_token: t\n",
        )
        .unwrap();
        let v = r.read().await;
        assert_eq!(v["providers"][0]["state"]["status"], "fresh", "{v}");
    }
}
