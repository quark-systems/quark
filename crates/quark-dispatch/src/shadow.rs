//! Shadow comparison against firstmate's dispatch resolution.
//!
//! In shadow mode firstmate resolves and spawns as it always has. The native
//! resolution runs beside it on the same rules with the classifier answer
//! firstmate already got (so the classifier is asked once and the two judge
//! the same answer) and a quota snapshot read at the same moment. What a
//! caller compares is the decision: the status, the selected profile, and
//! which candidates were eligible. Reasons are prose and not compared.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::classify::{Answer, Classification};
use crate::resolve::Resolution;

/// What firstmate's resolution reported, in neutral fields.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Observed {
    /// `clear`, `ambiguous`, `escalate`, `error` or `off`.
    pub status: String,
    pub reason: Option<String>,
    /// Whether the classifier answered.
    pub consulted: bool,
    /// `rule_N` or `default`.
    pub rule: Option<String>,
    pub confidence: Option<f64>,
    pub classifier_model: Option<String>,
    pub fallback: Option<String>,
    /// harness, model, effort.
    pub profile: Option<(String, Option<String>, Option<String>)>,
    /// harness, model, eligible.
    pub candidates: Vec<(String, Option<String>, bool)>,
}

/// The classification firstmate's resolution was made from; `None` when the
/// classifier was off.
pub fn classification(o: &Observed) -> Option<Classification> {
    if o.status == "off" {
        return None;
    }
    if let (true, Some(rule), Some(confidence)) = (o.consulted, &o.rule, o.confidence) {
        return Some(Classification::Answered {
            answer: Answer {
                choice: rule.clone(),
                confidence,
                probabilities: Default::default(),
                model: o.classifier_model.clone(),
                latency_ms: None,
                input_tokens: None,
                output_tokens: None,
            },
        });
    }
    let reason = o
        .fallback
        .clone()
        .or_else(|| o.reason.clone())
        .unwrap_or_else(|| "the classifier gave no answer".into());
    Some(Classification::Failed { reason })
}

/// The compared decision of firstmate's resolution.
pub fn decision_of_observed(o: &Observed) -> Value {
    json!({
        "status": o.status,
        "profile": o.profile.as_ref().map(|(h, m, e)| json!({"harness": h, "model": m, "effort": e})),
        "candidates": o.candidates.iter().map(|(h, m, ok)| json!({"harness": h, "model": m, "eligible": ok})).collect::<Vec<_>>(),
    })
}

/// The compared decision of a native resolution.
pub fn decision(r: &Resolution) -> Value {
    json!({
        "status": r.status.as_str(),
        "profile": r.profile().map(|p| json!({"harness": p.harness, "model": p.model, "effort": p.effort})),
        "candidates": r.candidates.iter().map(|c| json!({
            "harness": c.profile.harness, "model": c.profile.model, "eligible": c.verdict.eligible()
        })).collect::<Vec<_>>(),
    })
}

/// `(bash, native)` decisions when they differ.
pub fn compare(o: &Observed, native: &Resolution) -> Option<(Value, Value)> {
    let (b, n) = (decision_of_observed(o), decision(native));
    (b != n).then_some((b, n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quota::{ProviderFamilies, QuotaSnapshot};
    use crate::resolve::resolve;
    use crate::rules::{ClassifierSettings, DispatchConfig};

    #[test]
    fn agrees_on_the_same_answer_and_quota() {
        let config = DispatchConfig::parse(
            r#"{"classifier":{"provider":"system1"},"rules":[{"when":"x","use":[{"harness":"claude","effort":"low"},{"harness":"codex"}]}]}"#,
        )
        .unwrap();
        let q = QuotaSnapshot::parse(
            r#"{"providers":[{"provider":"claude","quotaSemantics":{"status":"known","effectiveAvailability":[
              {"scope":"all_models","status":"known","effectivePercentRemaining":50,"runway":{"status":"ok"},"selection":{"spendPriority":0.8}}]}}]}"#,
        )
        .unwrap();
        let observed = Observed {
            status: "clear".into(),
            consulted: true,
            rule: Some("rule_1".into()),
            confidence: Some(0.9),
            profile: Some(("claude".into(), None, Some("low".into()))),
            candidates: vec![("claude".into(), None, true), ("codex".into(), None, true)],
            ..Observed::default()
        };
        let c = classification(&observed).unwrap();
        let settings = ClassifierSettings::from_config(&config, false);
        let f = ProviderFamilies::builtin();
        let native = resolve(&config, &settings, Some(&c), Ok(&q), &f);
        assert_eq!(compare(&observed, &native), None);

        let mut other = observed.clone();
        other.profile = Some(("codex".into(), None, None));
        let (b, n) = compare(&other, &native).unwrap();
        assert_eq!(b["profile"]["harness"], "codex");
        assert_eq!(n["profile"]["harness"], "claude");
    }

    #[test]
    fn off_and_failures() {
        let off = Observed {
            status: "off".into(),
            ..Observed::default()
        };
        assert!(classification(&off).is_none());
        let err = Observed {
            status: "error".into(),
            reason: Some("http 500".into()),
            ..Observed::default()
        };
        assert_eq!(
            classification(&err),
            Some(Classification::Failed {
                reason: "http 500".into()
            })
        );
    }
}
