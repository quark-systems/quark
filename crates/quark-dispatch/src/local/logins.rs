//! The quota of every provider a multi-provider harness (Pi, OpenCode) is
//! signed in to, read from its `auth.json`.
//!
//! Pi keeps one entry per provider in `<agent dir>/auth.json`, OpenCode in
//! `~/.local/share/opencode/auth.json`. Each entry whose provider has a
//! reader is read with that entry's token: Claude (`anthropic`), Codex
//! (`openai-codex`, OpenCode's `openai`), Kimi Code (`kimi-coding`,
//! `kimi-for-coding`), Grok (`xai` OAuth) and the key-based providers in
//! [`super::keyed`]. The reading holds one provider object per entry, each
//! with a `label`; Accounts shows them as rows under the account.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{json, Value};
use time::OffsetDateTime;

use super::keyed::{entry_key, Api, Key, KeyedUsage};
use super::support::{self, num, Cache};
use super::windows::failure;
use super::{ClaudeUsage, CodexUsage, GrokUsage, KimiUsage, LocalReading};

/// Which harness's logins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    Pi,
    OpenCode,
}

impl Harness {
    pub fn id(self) -> &'static str {
        match self {
            Harness::Pi => "pi",
            Harness::OpenCode => "opencode",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Harness::Pi => "Pi",
            Harness::OpenCode => "OpenCode",
        }
    }
}

/// A provider an entry signs in to.
#[derive(Debug, Clone, PartialEq)]
enum Target {
    Claude,
    Codex,
    Kimi,
    Grok,
    Keyed(Api, Option<&'static str>),
}

impl Target {
    fn for_entry(name: &str) -> Option<Target> {
        Some(match name {
            "anthropic" => Target::Claude,
            "openai-codex" | "openai" => Target::Codex,
            "kimi-coding" | "kimi-for-coding" => Target::Kimi,
            "xai" => Target::Grok,
            n => {
                let (api, base) = Api::for_entry(n)?;
                Target::Keyed(api, base)
            }
        })
    }

    fn provider(&self) -> &'static str {
        match self {
            Target::Claude => "claude",
            Target::Codex => "codex",
            Target::Kimi => "kimi",
            Target::Grok => "grok",
            Target::Keyed(api, _) => api.id(),
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Target::Claude => "Claude",
            Target::Codex => "ChatGPT (Codex)",
            Target::Kimi => "Kimi Code",
            Target::Grok => "Grok",
            Target::Keyed(api, _) => api.label(),
        }
    }
}

/// Reads every provider a Pi or OpenCode login holds.
#[derive(Debug, Clone)]
pub struct LoginsUsage {
    pub harness: Harness,
    /// The default login's `auth.json`.
    pub default_file: Option<PathBuf>,
    pub claude: ClaudeUsage,
    pub codex: CodexUsage,
    pub kimi: KimiUsage,
    pub grok: GrokUsage,
    /// A template for the key-based readers (its HTTP and cache).
    pub keyed: KeyedUsage,
    pub cache: Cache,
}

impl LoginsUsage {
    pub fn from_env(harness: Harness) -> Self {
        let default_file = match harness {
            Harness::Pi => support::pi_dir().map(|d| d.join("auth.json")),
            Harness::OpenCode => support::env("XDG_DATA_HOME")
                .map(PathBuf::from)
                .or_else(|| support::user_home().map(|h| h.join(".local").join("share")))
                .map(|d| d.join("opencode").join("auth.json")),
        };
        Self {
            harness,
            default_file,
            claude: ClaudeUsage::from_env(),
            codex: CodexUsage::from_env(),
            kimi: KimiUsage::from_env(),
            grok: GrokUsage::from_env(),
            keyed: KeyedUsage::new(Api::Mimo),
            cache: Cache::default(),
        }
    }

    /// The reading for the login in `dir` (`None`: the default login).
    pub async fn reading_for(&self, dir: Option<&Path>) -> Value {
        let file = dir
            .map(|d| d.join("auth.json"))
            .or_else(|| self.default_file.clone());
        let key = format!(
            "logins\n{}\n{}",
            self.harness.id(),
            file.as_deref().unwrap_or(Path::new("")).display()
        );
        if let Some(v) = self.cache.get(&key) {
            return v;
        }
        let v = self.read_now(file.as_deref()).await;
        self.cache.put(&key, &v);
        v
    }

    async fn read_now(&self, file: Option<&Path>) -> Value {
        let now = OffsetDateTime::now_utc();
        let source = format!("quarkd: {} logins", self.harness.label());
        let fail = |status: &str, error: &str| {
            failure(self.harness.id(), &source, now, status, error, None)
        };
        let entries = match file.map(support::read_json) {
            Some(Ok(Some(v))) => v,
            Some(Err(e)) => return fail("unavailable", &e),
            _ => {
                return fail(
                    "auth_required",
                    &format!("{} has no provider logins here", self.harness.label()),
                )
            }
        };
        let mut targets: Vec<(Target, &Value)> = Vec::new();
        for (name, e) in entries.as_object().into_iter().flatten() {
            if let Some(t) = Target::for_entry(name) {
                if !targets.iter().any(|(x, _)| x.provider() == t.provider()) {
                    targets.push((t, e));
                }
            }
        }
        if targets.is_empty() {
            return fail(
                "unavailable",
                &format!(
                    "None of {}'s logins is a provider Quark reads quota for",
                    self.harness.label()
                ),
            );
        }
        let reads = targets.iter().map(|(t, e)| self.read_entry(t, e, now));
        let providers: Vec<Value> = futures_util::future::join_all(reads)
            .await
            .into_iter()
            .zip(&targets)
            .filter_map(|(v, (t, _))| {
                let mut p = v["providers"][0].clone();
                p.as_object()?;
                p["label"] = json!(t.label());
                Some(p)
            })
            .collect();
        json!({
            "schemaVersion": 5,
            "generatedAt": super::utc_iso(now),
            "source": source,
            "providers": providers,
        })
    }

    async fn read_entry(&self, t: &Target, e: &Value, now: OffsetDateTime) -> Value {
        let token = entry_key(e);
        let oauth = e.get("type").and_then(Value::as_str) == Some("oauth");
        let expired = oauth
            && num(e.get("expires")).is_some_and(|x| x <= now.unix_timestamp() as f64 * 1000.0);
        let Some(token) = token else {
            return failure(
                t.provider(),
                "quarkd",
                now,
                "unavailable",
                "the login holds no token",
                None,
            );
        };
        if expired {
            return failure(
                t.provider(),
                "quarkd",
                now,
                "auth_required",
                &format!(
                    "{}'s {} sign-in expired; it renews the next time {} uses it",
                    self.harness.label(),
                    t.label(),
                    self.harness.label()
                ),
                None,
            );
        }
        match t {
            Target::Claude if oauth => self.claude.read_token(&token).await,
            Target::Codex if oauth => {
                let account = support::text(e.get("accountId"));
                self.codex.read_token(&token, account.as_deref()).await
            }
            Target::Kimi => self.kimi.read_token(&token).await,
            Target::Grok if oauth => self.grok.read_token(&token).await,
            Target::Keyed(api, base) => {
                let mut r = self.keyed.clone();
                r.api = *api;
                let k = Key {
                    token,
                    base: base.map(str::to_string),
                };
                match r.read_key(&k).await {
                    Ok(v) => v,
                    Err((s, msg)) => failure(api.id(), "quarkd", now, s, &msg, None),
                }
            }
            _ => failure(
                t.provider(),
                "quarkd",
                now,
                "unavailable",
                &format!(
                    "{} signs in with an API key, which has no plan quota",
                    t.label()
                ),
                None,
            ),
        }
    }
}

#[async_trait]
impl LocalReading for LoginsUsage {
    fn provider(&self) -> &str {
        self.harness.id()
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

    #[tokio::test]
    async fn reads_each_login() {
        let tmp = tempfile::tempdir().unwrap();
        let curl = support::stub_curl(
            tmp.path(),
            &[
                (
                    "openrouter.ai",
                    200,
                    br#"{"data":{"limit":10,"limit_remaining":4}}"#,
                ),
                (
                    "api.kimi.com",
                    200,
                    br#"{"usages":{"limit_5h":{"used_ratio":0.5}}}"#,
                ),
            ],
        );
        let mut r = LoginsUsage::from_env(Harness::OpenCode);
        r.keyed.http.curl = curl.clone();
        r.kimi.http.curl = curl;
        r.kimi.cache = Cache::none();
        r.cache = Cache::none();
        r.default_file = Some(tmp.path().join("none.json"));
        let v = r.read().await;
        assert_eq!(v["providers"][0]["state"]["status"], "auth_required");
        let dir = tmp.path().join("oc");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("auth.json"),
            r#"{"openrouter":{"type":"api","key":"k"},"kimi-for-coding":{"type":"api","key":"m"},
                "anthropic":{"type":"oauth","access":"a","expires":1},"github-copilot":{"type":"oauth"}}"#,
        )
        .unwrap();
        let v = r.read_account(Some(&dir), false).await.unwrap();
        let ps = v["providers"].as_array().unwrap();
        assert_eq!(ps.len(), 3, "{v}");
        let by = |id: &str| ps.iter().find(|p| p["provider"] == id).unwrap();
        assert_eq!(by("openrouter")["label"], "OpenRouter");
        assert_eq!(by("openrouter")["state"]["status"], "fresh");
        assert_eq!(by("kimi")["state"]["status"], "fresh", "{v}");
        assert_eq!(by("claude")["state"]["status"], "auth_required");
    }
}
