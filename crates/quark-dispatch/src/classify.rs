//! The dispatch classifier: which rule fits a task.
//!
//! The System-1 API is the contract: one Choice question whose options are
//! every rule's `when` plus a fixed `default`, answered with the chosen
//! option, a confidence and a probability per option. Quark ships no
//! classifier model; [`SystemOne`] calls the provider the user configured.
//! The model never sees quota, profiles, approvals or `why`.

use std::collections::BTreeMap;
use std::process::Stdio;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

use crate::rules::{ClassifierSettings, DispatchConfig};

/// The `default` option's criterion: no listed rule applies.
pub const DEFAULT_WHEN: &str = "No listed rule applies to this task.";
/// The option meaning "no rule".
pub const DEFAULT_CHOICE: &str = "default";
/// The environment variable holding the System One API key when no
/// keychain credential is configured.
pub const KEY_ENV: &str = "TYPESAFE_API_KEY";
/// The System One endpoint.
pub const SYSTEM_ONE_URL: &str = "https://api.typesafe.ai/v1/systemone";

/// The classifier's answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    /// `rule_N` or `default`.
    pub choice: String,
    pub confidence: f64,
    #[serde(default)]
    pub probabilities: BTreeMap<String, f64>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub latency_ms: Option<u64>,
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
}

/// What asking the classifier produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Classification {
    Answered {
        answer: Answer,
    },
    /// No usable answer, and why.
    Failed {
        reason: String,
    },
}

/// Picks the rule for a task.
#[async_trait]
pub trait Classifier: Send + Sync {
    async fn classify(
        &self,
        project: Option<&str>,
        brief: &str,
        config: &DispatchConfig,
        settings: &ClassifierSettings,
    ) -> Classification;
}

/// Always gives the same classification: tests, and shadow mode, which
/// reuses the answer firstmate already got rather than asking twice.
#[derive(Debug, Clone)]
pub struct Given(pub Classification);

#[async_trait]
impl Classifier for Given {
    async fn classify(
        &self,
        _: Option<&str>,
        _: &str,
        _: &DispatchConfig,
        _: &ClassifierSettings,
    ) -> Classification {
        self.0.clone()
    }
}

/// The System One request for `brief`.
pub fn request(project: Option<&str>, brief: &str, config: &DispatchConfig, model: &str) -> Value {
    let mut criteria = serde_json::Map::new();
    for (i, r) in config.rules.iter().enumerate() {
        criteria.insert(DispatchConfig::rule_id(i), Value::String(r.when.clone()));
    }
    criteria.insert(DEFAULT_CHOICE.into(), Value::String(DEFAULT_WHEN.into()));
    json!({
        "model": model,
        "state": { "task": { "project": project.unwrap_or(""), "brief": brief } },
        "questions": { "rule": {
            "type": "choice",
            "instructions": "Which ONE dispatch rule best fits `task` (read `task.brief` and `task.project`)? Each option is the rule's own matching condition; pick `default` when no rule's condition is met, including when a rule's own exemption text excludes this task.",
            "criteria": criteria,
        }},
    })
}

/// The answer in a System One response, checked the way firstmate checks
/// it: a choice, a confidence in 0..=1, exactly one probability per option
/// in 0..=1 summing to 1 (within 0.01), and well-formed usage if present.
pub fn parse_answer(body: &Value, rule_count: usize) -> Result<Answer, String> {
    let bad = || "response is not a rule Choice answer".to_string();
    let a = body
        .get("answers")
        .and_then(|a| a.get("rule"))
        .ok_or_else(bad)?;
    let choice = a.get("choice").and_then(Value::as_str).ok_or_else(bad)?;
    let confidence = a
        .get("confidence")
        .and_then(Value::as_f64)
        .ok_or_else(bad)?;
    if !(0.0..=1.0).contains(&confidence) {
        return Err(bad());
    }
    let probs = a
        .get("probabilities")
        .and_then(Value::as_object)
        .ok_or_else(bad)?;
    let mut expected: Vec<String> = (0..rule_count).map(DispatchConfig::rule_id).collect();
    expected.push(DEFAULT_CHOICE.into());
    expected.sort();
    let mut keys: Vec<String> = probs.keys().cloned().collect();
    keys.sort();
    if keys != expected {
        return Err(bad());
    }
    let mut probabilities = BTreeMap::new();
    for (k, v) in probs {
        let p = v
            .as_f64()
            .filter(|p| (0.0..=1.0).contains(p))
            .ok_or_else(bad)?;
        probabilities.insert(k.clone(), p);
    }
    let total: f64 = probabilities.values().sum();
    if !(0.99..=1.01).contains(&total) {
        return Err(bad());
    }
    let (mut input_tokens, mut output_tokens) = (None, None);
    if let Some(u) = body.get("usage") {
        input_tokens = Some(
            u.get("input_tokens")
                .and_then(Value::as_u64)
                .ok_or_else(bad)?,
        );
        output_tokens = Some(
            u.get("output_tokens")
                .and_then(Value::as_u64)
                .ok_or_else(bad)?,
        );
    }
    Ok(Answer {
        choice: choice.to_string(),
        confidence,
        probabilities,
        model: body
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string),
        latency_ms: None,
        input_tokens,
        output_tokens,
    })
}

/// Calls the System One API with `curl`. The key comes from the keychain
/// credential when the settings name one (macOS `security`), else from
/// [`KEY_ENV`]; it reaches curl on stdin as a config line, never on argv,
/// and is never logged.
#[derive(Debug, Clone)]
pub struct SystemOne {
    pub url: String,
    pub curl: String,
}

impl Default for SystemOne {
    fn default() -> Self {
        Self {
            url: SYSTEM_ONE_URL.into(),
            curl: "curl".into(),
        }
    }
}

impl SystemOne {
    async fn key(&self, settings: &ClassifierSettings) -> Result<String, String> {
        if let Some(service) = settings
            .credential
            .as_deref()
            .and_then(|c| c.strip_prefix("keychain:"))
        {
            let out = tokio::time::timeout(
                Duration::from_secs(10),
                tokio::process::Command::new("security")
                    .args(["find-generic-password", "-s", service, "-w"])
                    .stdin(Stdio::null())
                    .kill_on_drop(true)
                    .output(),
            )
            .await
            .map_err(|_| "keychain read timed out".to_string())?
            .map_err(|e| format!("keychain read: {e}"))?;
            let key = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !out.status.success() || key.is_empty() {
                return Err(format!("no keychain item for service {service}"));
            }
            return Ok(key);
        }
        std::env::var(KEY_ENV)
            .ok()
            .filter(|k| !k.is_empty())
            .ok_or_else(|| format!("{KEY_ENV} is not set"))
    }

    async fn post(
        &self,
        key: &str,
        body: &str,
        timeout: Duration,
    ) -> Result<(u16, String), String> {
        // The body goes in a temporary file so stdin can carry the
        // header; curl reads its config (`-K -`) from stdin.
        let dir = std::env::temp_dir().join(format!("quark-dispatch-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| e.to_string())?;
        let file = dir.join(format!("{}.json", uuid_like()));
        tokio::fs::write(&file, body)
            .await
            .map_err(|e| e.to_string())?;
        let result = async {
            let mut child = tokio::process::Command::new(&self.curl)
                .args(["-sS", "--max-time"])
                .arg(format!("{:.3}", timeout.as_secs_f64()))
                .args([
                    "-w",
                    "\n%{http_code}",
                    "-X",
                    "POST",
                    "-H",
                    "Content-Type: application/json",
                    "-K",
                    "-",
                    "--data-binary",
                ])
                .arg(format!("@{}", file.display()))
                .arg(&self.url)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .map_err(|e| format!("curl: {e}"))?;
            let mut stdin = child.stdin.take().ok_or("curl stdin")?;
            stdin
                .write_all(format!("header = \"Authorization: Bearer {key}\"\n").as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            drop(stdin);
            let out =
                tokio::time::timeout(timeout + Duration::from_secs(2), child.wait_with_output())
                    .await
                    .map_err(|_| "timed out".to_string())?
                    .map_err(|e| e.to_string())?;
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            let (body, code) = text.rsplit_once('\n').unwrap_or(("", text.as_str()));
            Ok((code.trim().parse().unwrap_or(0), body.to_string()))
        }
        .await;
        let _ = tokio::fs::remove_file(&file).await;
        result
    }
}

fn uuid_like() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{t:x}")
}

#[async_trait]
impl Classifier for SystemOne {
    async fn classify(
        &self,
        project: Option<&str>,
        brief: &str,
        config: &DispatchConfig,
        settings: &ClassifierSettings,
    ) -> Classification {
        let failed = |reason: String| Classification::Failed { reason };
        let key = match self.key(settings).await {
            Ok(k) => k,
            Err(e) => return failed(e),
        };
        let body = request(project, brief, config, &settings.model).to_string();
        let started = Instant::now();
        let timeout = Duration::from_millis(settings.timeout_ms);
        let (code, text) = match self.post(&key, &body, timeout).await {
            Ok(r) => r,
            Err(e) => return failed(e),
        };
        let latency = started.elapsed().as_millis() as u64;
        if code != 200 {
            let snippet: String = text.chars().take(200).collect();
            return failed(format!(
                "http {code:03} after {latency} ms: {}",
                snippet.replace('\n', " ")
            ));
        }
        let parsed = serde_json::from_str::<Value>(&text)
            .map_err(|_| "response is not a rule Choice answer".to_string())
            .and_then(|v| parse_answer(&v, config.rules.len()));
        match parsed {
            Ok(mut a) => {
                a.latency_ms = Some(latency);
                Classification::Answered { answer: a }
            }
            Err(e) => failed(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> DispatchConfig {
        DispatchConfig::parse(
            r#"{"rules":[{"when":"a","use":{"harness":"claude"}},{"when":"b","use":{"harness":"codex"}}]}"#,
        )
        .unwrap()
    }

    #[test]
    fn request_lists_every_rule_and_default() {
        let r = request(Some("quark"), "Fix it.", &config(), "jev-latest");
        let c = &r["questions"]["rule"]["criteria"];
        assert_eq!(c["rule_1"], "a");
        assert_eq!(c["rule_2"], "b");
        assert_eq!(c["default"], DEFAULT_WHEN);
        assert_eq!(r["state"]["task"]["brief"], "Fix it.");
    }

    #[test]
    fn checks_answers() {
        let ok = json!({"model":"jev-1","answers":{"rule":{"choice":"rule_2","confidence":0.9,
            "probabilities":{"rule_1":0.05,"rule_2":0.9,"default":0.05}}},"usage":{"input_tokens":10,"output_tokens":2}});
        let a = parse_answer(&ok, 2).unwrap();
        assert_eq!(a.choice, "rule_2");
        assert_eq!(a.model.as_deref(), Some("jev-1"));
        assert_eq!(a.input_tokens, Some(10));

        let mut missing = ok.clone();
        missing["answers"]["rule"]["probabilities"]
            .as_object_mut()
            .unwrap()
            .remove("default");
        assert!(parse_answer(&missing, 2).is_err());
        let mut sum = ok.clone();
        sum["answers"]["rule"]["probabilities"]["rule_1"] = json!(0.5);
        assert!(parse_answer(&sum, 2).is_err());
        let mut usage = ok;
        usage["usage"] = json!({"input_tokens":"x"});
        assert!(parse_answer(&usage, 2).is_err());
    }
}
