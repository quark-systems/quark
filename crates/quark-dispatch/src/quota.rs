//! Quota evidence: one `quota-axi --json` snapshot, read leniently.
//!
//! Only the fields dispatch weighs are kept. A field of the wrong type reads
//! as missing rather than failing the snapshot, the way firstmate's `jq`
//! treats it.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use quark_core::harness::HarnessManifest;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One applicable quota window of a provider.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QuotaRow {
    /// `all_models`, `all_products`, `model:<id>` or `product:<id>`.
    pub scope: String,
    /// `known`, `partial`, `unknown`, ...
    pub status: String,
    pub percent_remaining: Option<f64>,
    /// `exhausted_now`, `ok`, ...
    pub runway: Option<String>,
    pub spend_priority: Option<f64>,
    /// The provider bills usage past the plan, so 0% is a budget, not a
    /// block.
    pub overage_allowed: bool,
    pub overage_active: bool,
}

/// One provider's reading.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProviderQuota {
    pub provider: String,
    /// `quotaSemantics.status`: `known`, `partial`, `unknown`, ...
    pub status: String,
    pub rows: Vec<QuotaRow>,
}

impl ProviderQuota {
    /// Whether the reading can be ranked at all.
    pub fn measured(&self) -> bool {
        matches!(self.status.as_str(), "known" | "partial")
    }
}

/// Every provider quota-axi reported.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QuotaSnapshot {
    pub providers: Vec<ProviderQuota>,
}

impl QuotaSnapshot {
    /// Parse `quota-axi --json` output. Refuses anything without a
    /// `providers` list of objects naming their provider.
    pub fn parse(json: &str) -> Result<Self, String> {
        let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        Self::from_value(&v)
    }

    /// Parse `quota-axi --json` output that must also pass firstmate's
    /// schema check ([`valid`]).
    pub fn parse_checked(json: &str) -> Result<Self, String> {
        let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        if !valid(&v) {
            return Err("not a valid schema-5 quota snapshot".into());
        }
        Self::from_value(&v)
    }

    pub fn from_value(v: &Value) -> Result<Self, String> {
        let list = v
            .get("providers")
            .and_then(Value::as_array)
            .ok_or("no providers list")?;
        let mut providers = Vec::new();
        for p in list {
            let provider = p
                .get("provider")
                .and_then(Value::as_str)
                .ok_or("a provider has no name")?;
            let sem = p.get("quotaSemantics");
            let status = sem
                .and_then(|s| s.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let rows = sem
                .and_then(|s| s.get("effectiveAvailability"))
                .and_then(Value::as_array)
                .map(|rows| rows.iter().map(row).collect())
                .unwrap_or_default();
            providers.push(ProviderQuota {
                provider: provider.to_string(),
                status,
                rows,
            });
        }
        Ok(Self { providers })
    }

    pub fn provider(&self, name: &str) -> Option<&ProviderQuota> {
        self.providers.iter().find(|p| p.provider == name)
    }

    /// Adds `other`'s providers that this snapshot lacks.
    pub fn merge_missing(&mut self, other: QuotaSnapshot) {
        for p in other.providers {
            if self.provider(&p.provider).is_none() {
                self.providers.push(p);
            }
        }
    }
}

/// firstmate's `fm_quota_json_valid`: schema version 5, unique provider ids
/// of lower-case words joined by dashes, a known semantics status, and rows
/// whose fields agree with their status (a known row has a percentage in
/// 0..=100 and a runway status; an unknown row has neither a percentage nor
/// a runway other than `unknown` or `exhausted_now`).
pub fn valid(v: &Value) -> bool {
    let Some(providers) = v
        .get("schemaVersion")
        .and_then(Value::as_u64)
        .filter(|n| *n == 5)
        .and(v.get("providers"))
        .and_then(Value::as_array)
    else {
        return false;
    };
    let mut seen = std::collections::HashSet::new();
    providers.iter().all(|p| {
        let Some(id) = p.get("provider").and_then(Value::as_str) else {
            return false;
        };
        if !seen.insert(id) || !provider_id_ok(id) {
            return false;
        }
        let Some(sem) = p.get("quotaSemantics").filter(|s| s.is_object()) else {
            return false;
        };
        let status = sem.get("status").and_then(Value::as_str).unwrap_or("");
        let Some(rows) = sem.get("effectiveAvailability").and_then(Value::as_array) else {
            return false;
        };
        let row_status = |r: &Value| {
            r.get("status")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let semantics_ok = match status {
            "known" => {
                !rows.is_empty()
                    && rows
                        .iter()
                        .all(|r| matches!(row_status(r).as_str(), "known" | "unknown"))
            }
            "unknown" => rows.iter().all(|r| row_status(r) == "unknown"),
            "partial" => true,
            _ => false,
        };
        semantics_ok && rows.iter().all(row_ok)
    })
}

fn provider_id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.split('-').all(|w| {
            !w.is_empty()
                && w.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

fn row_ok(r: &Value) -> bool {
    let Some(scope) = r.get("scope").and_then(Value::as_str) else {
        return false;
    };
    if scope.is_empty() || scope.trim() != scope {
        return false;
    }
    let runway = r.get("runway");
    let runway_status = runway.and_then(|w| w.get("status")).and_then(Value::as_str);
    match r.get("status").and_then(Value::as_str) {
        Some("known") => {
            r.get("effectivePercentRemaining")
                .and_then(Value::as_f64)
                .is_some_and(|p| (0.0..=100.0).contains(&p))
                && runway.is_some_and(Value::is_object)
                && runway_status.is_some_and(|s| {
                    matches!(
                        s,
                        "through_reset" | "projected_exhaustion" | "exhausted_now" | "unknown"
                    )
                })
        }
        Some("unknown") => {
            r.get("effectivePercentRemaining").is_none()
                && (runway.is_none()
                    || (runway.is_some_and(Value::is_object)
                        && runway_status.is_some_and(|s| matches!(s, "unknown" | "exhausted_now"))))
        }
        _ => false,
    }
}

fn row(v: &Value) -> QuotaRow {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    QuotaRow {
        scope: s("scope").unwrap_or_default(),
        status: s("status").unwrap_or_default(),
        percent_remaining: v.get("effectivePercentRemaining").and_then(Value::as_f64),
        runway: v
            .get("runway")
            .and_then(|r| r.get("status"))
            .and_then(Value::as_str)
            .map(str::to_string),
        spend_priority: v
            .get("selection")
            .and_then(|r| r.get("spendPriority"))
            .and_then(Value::as_f64),
        overage_allowed: v
            .get("overage")
            .and_then(|o| o.get("allowed"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        overage_active: v
            .get("overage")
            .and_then(|o| o.get("active"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

/// Where quota evidence comes from.
#[async_trait]
pub trait QuotaSource: Send + Sync {
    /// One snapshot covering at least `providers` where it can.
    async fn snapshot(&self, providers: &[String]) -> Result<QuotaSnapshot, String>;
}

/// A fixed snapshot, for tests and for replaying a decision.
#[derive(Debug, Clone, Default)]
pub struct FixedQuota(pub Option<QuotaSnapshot>);

#[async_trait]
impl QuotaSource for FixedQuota {
    async fn snapshot(&self, _providers: &[String]) -> Result<QuotaSnapshot, String> {
        self.0
            .clone()
            .ok_or_else(|| "quota-axi --json failed".to_string())
    }
}

/// `quota-axi --json`, plus a local reading command for a provider it does
/// not report (firstmate's Bob and Kiro readers). A local reading joins the
/// snapshot only when it parses and holds exactly that provider; any failure
/// leaves the provider unmeasured.
#[derive(Debug, Clone)]
pub struct QuotaAxi {
    pub bin: PathBuf,
    /// Provider to the argv that prints its reading.
    pub local: BTreeMap<String, Vec<String>>,
    pub timeout: Duration,
}

impl QuotaAxi {
    pub fn new(bin: impl Into<PathBuf>) -> Self {
        Self {
            bin: bin.into(),
            local: BTreeMap::new(),
            timeout: Duration::from_secs(15),
        }
    }

    async fn run(&self, argv: &[String]) -> Result<String, String> {
        let (exe, args) = argv.split_first().ok_or("empty command")?;
        let out = tokio::time::timeout(
            self.timeout,
            tokio::process::Command::new(exe)
                .args(args)
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| format!("{exe} timed out"))?
        .map_err(|e| format!("{exe}: {e}"))?;
        if !out.status.success() {
            return Err(format!("{exe} exited {}", out.status));
        }
        String::from_utf8(out.stdout).map_err(|e| e.to_string())
    }
}

#[async_trait]
impl QuotaSource for QuotaAxi {
    async fn snapshot(&self, providers: &[String]) -> Result<QuotaSnapshot, String> {
        let argv = vec![self.bin.to_string_lossy().into_owned(), "--json".into()];
        let out = self
            .run(&argv)
            .await
            .map_err(|_| "quota-axi --json failed".to_string())?;
        let mut snap = QuotaSnapshot::parse_checked(&out)
            .map_err(|_| "quota-axi --json returned an invalid snapshot".to_string())?;
        for p in providers {
            if snap.provider(p).is_some() {
                continue;
            }
            let Some(argv) = self.local.get(p) else {
                continue;
            };
            let Ok(out) = self.run(argv).await else {
                continue;
            };
            match QuotaSnapshot::parse_checked(&out) {
                Ok(r) if r.providers.len() == 1 && &r.providers[0].provider == p => {
                    snap.merge_missing(r)
                }
                _ => {}
            }
        }
        Ok(snap)
    }
}

/// Firstmate's single-provider table: the quota family of a harness that
/// bills one provider. Harnesses that reach several (pi, opencode, omp)
/// have none and need `provider` on the profile.
const FAMILIES: &[(&str, &str)] = &[
    ("claude", "claude"),
    ("codex", "codex"),
    ("grok", "grok"),
    ("kimi", "kimi"),
    ("cursor", "cursor"),
    ("agy", "agy"),
    ("muse", "meta"),
    ("kiro", "kiro"),
];

/// Which quota provider family each harness name belongs to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderFamilies(BTreeMap<String, String>);

impl ProviderFamilies {
    /// The built-in table only, keyed by firstmate's adapter names.
    pub fn builtin() -> Self {
        Self(
            FAMILIES
                .iter()
                .map(|(h, p)| (h.to_string(), p.to_string()))
                .collect(),
        )
    }

    /// The built-in table, extended to each manifest's id and aliases: a
    /// manifest's own `quota.provider` wins, else the table's entry for its
    /// id or an alias.
    pub fn from_manifests<'a>(manifests: impl IntoIterator<Item = &'a HarnessManifest>) -> Self {
        let mut map = Self::builtin().0;
        for m in manifests {
            let names: Vec<&str> = std::iter::once(m.id.as_str())
                .chain(m.aliases.iter().map(String::as_str))
                .collect();
            let declared = m
                .quota
                .as_ref()
                .map(|q| q.provider.clone())
                .filter(|p| p != "none");
            let family = declared.or_else(|| names.iter().find_map(|n| map.get(*n).cloned()));
            if let Some(f) = family {
                for n in names {
                    map.insert(n.to_string(), f.clone());
                }
            }
        }
        Self(map)
    }

    pub fn get(&self, harness: &str) -> Option<&str> {
        self.0.get(harness).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_leniently() {
        let s = QuotaSnapshot::parse(
            r#"{"schema":5,"providers":[{"provider":"claude","quotaSemantics":{"status":"known",
              "effectiveAvailability":[{"scope":"all_models","status":"known","effectivePercentRemaining":79,
                "runway":{"status":"ok"},"selection":{"spendPriority":"high"}}]}}]}"#,
        )
        .unwrap();
        let r = &s.provider("claude").unwrap().rows[0];
        assert_eq!(r.percent_remaining, Some(79.0));
        assert_eq!(r.spend_priority, None);
        assert_eq!(r.runway.as_deref(), Some("ok"));
        assert!(QuotaSnapshot::parse(r#"{"schema":5}"#).is_err());
    }

    #[test]
    fn families_follow_aliases() {
        let f = ProviderFamilies::builtin();
        assert_eq!(f.get("muse"), Some("meta"));
        assert_eq!(f.get("pi"), None);
    }
}
