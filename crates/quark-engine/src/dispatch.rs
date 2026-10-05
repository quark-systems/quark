//! Typed view of `fm-dispatch-resolve.sh` output, the engine's dispatch
//! resolution for one task brief.
//!
//! The script header owns the format: a TOON-style block of two-space
//! indented `key: value` lines under `dispatch-resolve:`. With no classifier
//! key the script prints nothing on stdout, which reads as [`Resolution::off`].
//! Lines this parser does not know are ignored, so additive engine changes do
//! not break it.

use serde::{Deserialize, Serialize};

/// What the resolution reported.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Resolution {
    /// `clear`, `ambiguous`, `escalate`, `error`, or `off` when the resolver
    /// is not enabled.
    pub status: String,
    /// The classifier model that answered, when one was asked.
    pub model: Option<String>,
    /// Matched rule id (`rule_<n>` or `default`).
    pub rule: Option<String>,
    /// The matched rule's `when`, as excerpted by the engine.
    pub rule_when: Option<String>,
    pub confidence: Option<f64>,
    pub reason: Option<String>,
    pub notes: Vec<String>,
    pub candidates: Vec<Candidate>,
    /// Why the classifier's answer was not used and the default profiles
    /// were resolved instead (`on_failure: default`): a confidence below the
    /// floor, a timeout or a failure.
    pub fallback: Option<String>,
    /// The profile the resolution selected (`clear` only).
    pub profile: Option<Profile>,
    /// The block as printed, empty when off.
    pub raw: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub harness: String,
    pub model: Option<String>,
    pub eligible: bool,
    /// The verdict after `->`, without the eligibility word when it adds
    /// nothing: `eligible`, `unranked: ...`, `budget-based: no quota`, or the
    /// reason it is not eligible.
    pub reason: String,
    /// Quota evidence between the profile and the verdict.
    pub evidence: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub harness: String,
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl Resolution {
    /// The resolver is not enabled: firstmate dispatches as it always has.
    pub fn off() -> Self {
        Resolution {
            status: "off".into(),
            model: None,
            rule: None,
            rule_when: None,
            confidence: None,
            reason: None,
            notes: Vec::new(),
            candidates: Vec::new(),
            fallback: None,
            profile: None,
            raw: String::new(),
        }
    }

    /// Whether the classifier answered. An escalation for an empty rule set,
    /// a failure before the request and a fallback for a classifier that
    /// never answered carry no answer.
    pub fn classifier_consulted(&self) -> bool {
        self.model.is_some() || self.confidence.is_some()
    }
}

/// Parses resolver stdout. Empty output is the off state; anything else must
/// carry a `status:` line.
pub fn parse(stdout: &str) -> Option<Resolution> {
    if stdout.trim().is_empty() {
        return Some(Resolution::off());
    }
    let mut r = Resolution::off();
    r.raw = stdout.trim_end().to_string();
    let mut status = None;
    for line in stdout.lines() {
        let Some((key, value)) = line.trim().split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key {
            "status" => status = Some(value.to_string()),
            "model" => {
                let m = value.split("   ").next().unwrap_or("").trim();
                r.model = known(m);
            }
            "rule" => parse_rule(value, &mut r),
            "fallback" => r.fallback = parse_fallback(value),
            "reason" => r.reason = known(value),
            "note" => r.notes.extend(known(value)),
            "candidate" => r.candidates.extend(parse_candidate(value)),
            "profile" => r.profile = parse_profile(value),
            _ => {}
        }
    }
    r.status = status?;
    Some(r)
}

/// `default (confidence 0.4 below floor 0.6)`
fn parse_fallback(value: &str) -> Option<String> {
    let why = value.strip_prefix("default").unwrap_or(value).trim();
    let why = why
        .strip_prefix('(')
        .and_then(|w| w.strip_suffix(')'))
        .unwrap_or(why);
    Some(why.to_string())
}

/// `rule_4 (A simple bug fix.)   confidence: 0.9`
fn parse_rule(value: &str, r: &mut Resolution) {
    let (rule, confidence) = match value.rsplit_once("confidence:") {
        Some((rule, c)) => (rule.trim(), c.trim().parse().ok()),
        None => (value, None),
    };
    r.confidence = confidence;
    let (id, when) = match rule.split_once(' ') {
        Some((id, rest)) => (id, rest.trim()),
        None => (rule, ""),
    };
    r.rule = known(id);
    r.rule_when = when
        .strip_prefix('(')
        .and_then(|w| w.strip_suffix(')'))
        .and_then(known);
}

/// `claude:sonnet  provider=claude  scope=all_models  remaining=79%  -> eligible`
fn parse_candidate(value: &str) -> Option<Candidate> {
    let (head, verdict) = value.rsplit_once("->")?;
    let head = head.trim();
    let (profile, evidence) = match head.split_once("  ") {
        Some((p, e)) => (p, known(e.trim())),
        None => (head, None),
    };
    let (harness, model) = profile.split_once(':').unwrap_or((profile, "-"));
    let verdict = verdict.trim();
    let (eligible, reason) = if let Some(r) = verdict.strip_prefix("not eligible:") {
        (false, r.trim())
    } else if let Some(r) = verdict.strip_prefix("eligible,") {
        (true, r.trim())
    } else {
        (verdict == "eligible", verdict)
    };
    Some(Candidate {
        harness: harness.to_string(),
        model: known(model),
        eligible,
        reason: reason.to_string(),
        evidence: evidence.map(|e| e.split_whitespace().collect::<Vec<_>>().join(" ")),
    })
}

/// `--harness 'cursor' --model 'cursor-grok-4.6-medium' --effort 'high'`
fn parse_profile(value: &str) -> Option<Profile> {
    let words = shell_words(value)?;
    let mut p = Profile {
        harness: String::new(),
        model: None,
        effort: None,
    };
    let mut it = words.into_iter();
    while let Some(flag) = it.next() {
        let v = it.next()?;
        match flag.as_str() {
            "--harness" => p.harness = v,
            "--model" => p.model = Some(v),
            "--effort" => p.effort = Some(v),
            _ => {}
        }
    }
    (!p.harness.is_empty()).then_some(p)
}

/// Splits jq `@sh` output: bare words and single-quoted strings, where a
/// quote inside a value is written `'\''`.
fn shell_words(s: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => cur.push(c),
                    }
                }
            }
            '\\' => {
                in_word = true;
                cur.push(chars.next()?);
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    Some(words)
}

/// The engine prints `-` for an absent value.
fn known(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty() && s != "-").then(|| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLEAR: &str = "dispatch-resolve:
  status: clear
  model: jev-1.13.0   latency_ms: 214   tokens: 3114/151
  rule: rule_4 (A simple bug fix with a stated root cause.)   confidence: 0.9
  probabilities: default=0.02 rule_1=0.03 rule_2=0.02 rule_3=0.03 rule_4=0.9
  note: rule matched
  note: 1 eligible candidate(s) unranked (kimi)
  candidate: claude:sonnet  provider=claude  scope=all_models  remaining=79%  spendPriority=-0.4627  runway=projected_exhaustion  -> eligible
  candidate: kimi:kimi-code/k3  provider=kimi  -> eligible, unranked: provider kimi unmeasured (unknown): disclosed uncertainty
  candidate: bob:-  provider=bob  scope=all_models  remaining=14%  spendPriority=-  runway=unknown  -> not eligible: profile floor all_models below 15%
  candidate: cursor:cursor-grok-4.6-medium  provider=cursor  scope=all_models  remaining=88%  spendPriority=0.31  runway=through_reset  -> eligible
  profile: --harness 'cursor' --model 'cursor-grok-4.6-medium' --effort 'it'\\''s'
";

    #[test]
    fn clear_resolution() {
        let r = parse(CLEAR).unwrap();
        assert_eq!(r.status, "clear");
        assert_eq!(r.model.as_deref(), Some("jev-1.13.0"));
        assert_eq!(r.rule.as_deref(), Some("rule_4"));
        assert_eq!(
            r.rule_when.as_deref(),
            Some("A simple bug fix with a stated root cause.")
        );
        assert_eq!(r.confidence, Some(0.9));
        assert_eq!(r.notes.len(), 2);
        assert!(r.classifier_consulted());
        assert_eq!(
            r.profile,
            Some(Profile {
                harness: "cursor".into(),
                model: Some("cursor-grok-4.6-medium".into()),
                effort: Some("it's".into()),
            })
        );
        let c = &r.candidates;
        assert_eq!(c.len(), 4);
        assert_eq!(
            (c[0].harness.as_str(), c[0].model.as_deref()),
            ("claude", Some("sonnet"))
        );
        assert!(c[0].eligible);
        assert_eq!(c[0].reason, "eligible");
        assert_eq!(
            c[0].evidence.as_deref(),
            Some("provider=claude scope=all_models remaining=79% spendPriority=-0.4627 runway=projected_exhaustion")
        );
        assert!(c[1].eligible);
        assert_eq!(c[1].model.as_deref(), Some("kimi-code/k3"));
        assert!(c[1]
            .reason
            .starts_with("unranked: provider kimi unmeasured"));
        assert!(!c[2].eligible);
        assert_eq!(c[2].model, None);
        assert_eq!(c[2].reason, "profile floor all_models below 15%");
    }

    #[test]
    fn off_and_no_rules() {
        let off = parse("").unwrap();
        assert_eq!(off.status, "off");
        assert!(!off.classifier_consulted());
        let none =
            parse("dispatch-resolve:\n  status: escalate\n  reason: no rules to match\n").unwrap();
        assert_eq!(none.status, "escalate");
        assert_eq!(none.reason.as_deref(), Some("no rules to match"));
        assert!(!none.classifier_consulted());
        assert!(none.candidates.is_empty());
    }

    #[test]
    fn ambiguous_keeps_candidates_without_a_profile() {
        let r = parse(
            "dispatch-resolve:
  status: ambiguous
  model: jev-1.13.0   latency_ms: 200   tokens: -/-
  rule: default (No listed rule applies to this task.)   confidence: 0.41
  reason: confidence 0.41 below floor 0.6
  candidate: pi:amazon-bedrock/global.anthropic.claude-sonnet-5-5  provider=amazon-bedrock  -> eligible, budget-based: no quota
",
        )
        .unwrap();
        assert_eq!(r.rule.as_deref(), Some("default"));
        assert_eq!(r.confidence, Some(0.41));
        assert_eq!(r.profile, None);
        assert_eq!(
            r.candidates[0].model.as_deref(),
            Some("amazon-bedrock/global.anthropic.claude-sonnet-5-5")
        );
        assert_eq!(r.candidates[0].reason, "budget-based: no quota");
    }

    #[test]
    fn output_without_a_status_is_refused() {
        assert_eq!(parse("dispatch-resolve:\n  reason: x\n"), None);
    }

    #[test]
    fn parses_a_fallback_to_the_default_profiles() {
        // The classifier never answered: no model, rule or probabilities.
        let failed = parse(
            "dispatch-resolve:
  status: clear
  fallback: default (classifier request timed out after 2000ms)
  note: classifier fell back to the default profiles (on_failure default): classifier request timed out after 2000ms
  select: ordered
  candidate: claude:sonnet  provider=claude  scope=all_models  remaining=79%  spendPriority=1.2  runway=ok  -> eligible
  selected: claude:sonnet (first eligible profile in listed order)
  profile: --harness 'claude' --model 'sonnet'
",
        )
        .unwrap();
        assert_eq!(failed.status, "clear");
        assert_eq!(
            failed.fallback.as_deref(),
            Some("classifier request timed out after 2000ms")
        );
        assert!(!failed.classifier_consulted());
        assert_eq!(failed.rule, None);
        assert_eq!(failed.profile.unwrap().harness, "claude");

        // It answered below the floor: its answer is still reported.
        let low = parse(
            "dispatch-resolve:
  status: clear
  model: jev-1.13.0   latency_ms: 412   tokens: 310/12
  rule: rule_1 (A simple bug fix.)   confidence: 0.4
  probabilities: rule_1=0.4 default=0.6
  fallback: default (confidence 0.4 below floor 0.6)
  profile: --harness 'codex'
",
        )
        .unwrap();
        assert_eq!(
            low.fallback.as_deref(),
            Some("confidence 0.4 below floor 0.6")
        );
        assert!(low.classifier_consulted());
        assert_eq!(low.confidence, Some(0.4));
        assert_eq!(low.rule.as_deref(), Some("rule_1"));
    }
}
