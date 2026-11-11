//! Alibaba Coding Plan's usage, read the way quota-axi read it, without it.
//!
//! `bl usage token-plan --output json` prints the plan's weekly usage and
//! any per-model limits. The weekly window bounds `all_models`; each model
//! limit is its own `model:<id>` scope.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use time::OffsetDateTime;

use super::support::{self, num, pick, Cache};
use super::windows::{failure, reading, Window};
use super::{parse_time, LocalReading};

pub const PROVIDER: &str = "alibaba";
const SOURCE: &str = "quarkd: bl usage token-plan";
const LABEL: &str = "Alibaba Coding Plan";

/// Reads Alibaba Coding Plan's usage through its `bl` CLI.
#[derive(Debug, Clone)]
pub struct AlibabaUsage {
    pub bl: String,
    pub timeout: Duration,
    pub cache: Cache,
}

impl Default for AlibabaUsage {
    fn default() -> Self {
        Self {
            bl: "bl".into(),
            timeout: Duration::from_secs(15),
            cache: Cache::default(),
        }
    }
}

impl AlibabaUsage {
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
        let argv = [
            self.bl.clone(),
            "usage".into(),
            "token-plan".into(),
            "--output".into(),
            "json".into(),
        ];
        let text = match super::run_with(&argv, None, &[], self.timeout).await {
            Ok((_, t)) => t,
            Err(e) if e.contains("No such file") || e.contains("not found") => {
                return fail("unavailable", "Alibaba's CLI (bl) is not installed")
            }
            Err(e) => return fail("unavailable", &e),
        };
        let parsed = text
            .find('{')
            .and_then(|i| serde_json::from_str::<Value>(text[i..].trim()).ok());
        match parsed.and_then(|v| usage_reading(&v, now)) {
            Some(v) => v,
            None if text.to_ascii_lowercase().contains("login") => fail(
                "auth_required",
                "Alibaba's CLI is not signed in here; run `bl login`",
            ),
            None => fail("unavailable", "bl printed no token-plan usage"),
        }
    }
}

fn percent_used(v: f64) -> f64 {
    if v <= 1.0 {
        v * 100.0
    } else {
        v
    }
}

fn usage_reading(v: &Value, now: OffsetDateTime) -> Option<Value> {
    if !["planName", "plan", "per1WeekPercentage", "limits"]
        .iter()
        .any(|k| v.get(*k).is_some())
    {
        return None;
    }
    let mut windows = Vec::new();
    if let Some(u) = num(v.get("per1WeekPercentage")) {
        let reset = v.get("per1WeekResetTime").and_then(parse_time);
        windows.push(
            Window::new("weekly", "week", "weekly", percent_used(u))
                .resets(reset)
                .lasting(Some(604_800.0)),
        );
    }
    for e in v
        .get("limits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let details = ["limit", "quota", "modelLimit", "model_limit"]
            .iter()
            .find_map(|k| e.get(*k).filter(|d| d.is_object()));
        let get = |keys: &[&str]| {
            details
                .and_then(|d| pick(d, keys))
                .or_else(|| pick(e, keys))
        };
        let Some(model) = support::text(get(&["model", "modelName", "model_name"])) else {
            continue;
        };
        let used = num(get(&["percentRemaining", "remainingPercent"]))
            .map(|r| 100.0 - r)
            .or_else(|| num(get(&["per1WeekPercentage"])).map(percent_used))
            .or_else(|| {
                num(get(&[
                    "percentUsed",
                    "usedPercent",
                    "usagePercent",
                    "percentage",
                    "percent",
                ]))
            });
        let Some(used) = used else { continue };
        let reset = get(&[
            "resetsAt",
            "resetAt",
            "reset_at",
            "per1WeekResetTime",
            "nextResetTime",
        ])
        .and_then(parse_time);
        let mut id = format!("model:{model}");
        let mut n = 2;
        while windows.iter().any(|w: &Window| w.id == id) {
            id = format!("model:{model}:{n}");
            n += 1;
        }
        windows.push(Window::new(&id, &model, "model", used).resets(reset));
    }
    let weekly: Vec<&Window> = windows.iter().filter(|w| w.id == "weekly").collect();
    let mut scopes = vec![("all_models".to_string(), weekly)];
    for w in windows.iter().filter(|w| w.kind == "model") {
        scopes.push((w.id.clone(), vec![w]));
    }
    let mut extra = Map::new();
    let plan = support::text(v.get("planName"))
        .or_else(|| support::text(v.get("plan")))
        .unwrap_or_else(|| LABEL.into());
    extra.insert("plan".into(), json!(plan));
    Some(reading(PROVIDER, SOURCE, now, &windows, &scopes, extra))
}

#[async_trait]
impl LocalReading for AlibabaUsage {
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

    #[test]
    fn reads_week_and_models() {
        let v = usage_reading(
            &json!({"planName": "Lite", "per1WeekPercentage": 0.3,
                    "limits": [{"model": "qwen3-coder", "limit": {"percentRemaining": 40}}]}),
            OffsetDateTime::now_utc(),
        )
        .unwrap();
        assert!(crate::quota::valid(&v), "{v}");
        let rows = v["providers"][0]["quotaSemantics"]["effectiveAvailability"]
            .as_array()
            .unwrap();
        assert_eq!(rows[0]["effectivePercentRemaining"], json!(70));
        assert_eq!(rows[1]["scope"], "model:qwen3-coder");
        assert_eq!(rows[1]["effectivePercentRemaining"], json!(40));
        assert!(usage_reading(&json!({"x": 1}), OffsetDateTime::now_utc()).is_none());
    }

    #[tokio::test]
    async fn runs_the_cli() {
        let tmp = tempfile::tempdir().unwrap();
        let bl = tmp.path().join("bl");
        std::fs::write(
            &bl,
            "#!/bin/sh\necho '{\"planName\":\"Pro\",\"per1WeekPercentage\":12}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&bl, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let r = AlibabaUsage {
            bl: bl.to_string_lossy().into_owned(),
            cache: Cache::none(),
            ..Default::default()
        };
        let v = r.read().await;
        assert_eq!(v["providers"][0]["state"]["status"], "fresh", "{v}");
    }
}
