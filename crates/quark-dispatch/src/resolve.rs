//! Dispatch resolution: from the classifier's answer, the rules and one
//! quota snapshot to one profile, with every candidate's evidence.
//!
//! A line-for-line port of the selection in firstmate's
//! `fm-dispatch-resolve.sh`, so shadow mode can hold the two side by side:
//!
//! 1. The answer picks a rule (`rule_N`) or the default profiles. A rule
//!    needing approval, or whose floor cannot be verified, escalates; a rule
//!    whose floor is below falls through to the default profiles.
//! 2. Below the confidence floor the result is `ambiguous`, unless
//!    `on_failure: default` sends it (and any classifier failure) to the
//!    default profiles.
//! 3. Each candidate is judged on the quota rows that bound it: exhausted
//!    runway, 0% left without overage, or a profile floor below make it
//!    ineligible; missing or unknown evidence leaves it eligible but
//!    unranked; a `pricing: budget` profile needs no quota.
//! 4. `ordered` takes the first eligible candidate in listed order (and
//!    escalates if its evidence is unverifiable); `quota-balanced` takes the
//!    highest spend priority among ranked candidates, a budget candidate only
//!    when none is ranked, and escalates on a tie.

use serde::{Deserialize, Serialize};

use crate::classify::{Answer, Classification, DEFAULT_CHOICE, DEFAULT_WHEN};
use crate::quota::{ProviderFamilies, QuotaRow, QuotaSnapshot};
use crate::rules::{ClassifierSettings, DispatchConfig, Floor, OnFailure, Profile, Select};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// A profile was selected.
    Clear,
    /// The classifier's confidence was below the floor.
    Ambiguous,
    /// A person or the coordinator must pick: approval needed, nothing
    /// rankable, a tie, or no rules.
    Escalate,
    /// The classifier or the quota read failed.
    Error,
    /// No classifier is configured.
    Off,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Clear => "clear",
            Status::Ambiguous => "ambiguous",
            Status::Escalate => "escalate",
            Status::Error => "error",
            Status::Off => "off",
        }
    }
}

/// How a candidate was judged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Verdict {
    /// Eligible and ranked by spend priority.
    Eligible,
    /// Eligible, but its quota evidence cannot be ranked. `unknown` when the
    /// evidence itself is missing or unverifiable (rather than only lacking
    /// a spend priority); ordered selection never picks past such a one.
    Unranked {
        reason: String,
        unknown: bool,
    },
    /// Eligible because it is priced by budget, with no quota.
    Budget,
    NotEligible {
        reason: String,
    },
}

impl Verdict {
    pub fn eligible(&self) -> bool {
        !matches!(self, Verdict::NotEligible { .. })
    }

    pub fn reason(&self) -> String {
        match self {
            Verdict::Eligible => "ok".into(),
            Verdict::Unranked { reason, .. } | Verdict::NotEligible { reason } => reason.clone(),
            Verdict::Budget => "budget-based: no quota".into(),
        }
    }
}

/// One quota row a candidate was judged on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bound {
    pub scope: String,
    pub status: String,
    pub percent_remaining: Option<f64>,
    pub runway: Option<String>,
    pub spend_priority: Option<f64>,
}

/// A candidate profile and its evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub profile: Profile,
    pub provider: Option<String>,
    pub verdict: Verdict,
    /// The row that decided it: the limiting row when ranked.
    pub scope: Option<String>,
    pub percent_remaining: Option<f64>,
    pub runway: Option<String>,
    pub spend_priority: Option<f64>,
    pub bounds: Vec<Bound>,
    pub overage_active: bool,
}

impl Candidate {
    fn new(profile: &Profile, provider: Option<&str>, verdict: Verdict) -> Self {
        Self {
            profile: profile.clone(),
            provider: provider.map(str::to_string),
            verdict,
            scope: None,
            percent_remaining: None,
            runway: None,
            spend_priority: None,
            bounds: Vec::new(),
            overage_active: false,
        }
    }

    fn at(mut self, row: Option<&QuotaRow>) -> Self {
        if let Some(r) = row {
            self.scope = Some(r.scope.clone());
            self.percent_remaining = r.percent_remaining;
            self.runway = r.runway.clone();
        }
        self
    }

    fn ranked(&self) -> bool {
        matches!(self.verdict, Verdict::Eligible)
    }

    /// The evidence as firstmate prints it on a candidate line.
    pub fn evidence(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(p) = &self.provider {
            parts.push(format!("provider={p}"));
        }
        if let Some(s) = &self.scope {
            let show = |v: Option<f64>| v.map_or("-".to_string(), fmt_num);
            parts.push(format!("scope={s}"));
            parts.push(format!("remaining={}%", show(self.percent_remaining)));
            parts.push(format!("spendPriority={}", show(self.spend_priority)));
            parts.push(format!("runway={}", self.runway.as_deref().unwrap_or("-")));
        }
        if self.overage_active {
            parts.push("overage=active".into());
        }
        (!parts.is_empty()).then(|| parts.join(" "))
    }
}

fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// The resolution of one task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Resolution {
    pub status: Status,
    pub reason: Option<String>,
    pub notes: Vec<String>,
    /// The classifier's answer, when it gave one.
    pub answer: Option<Answer>,
    /// `rule_N` or `default`, when the classifier answered.
    pub rule: Option<String>,
    /// The rule's condition, cut to 60 characters as firstmate shows it.
    pub rule_when: Option<String>,
    /// Why the classifier's answer was not used (`on_failure: default`).
    pub fallback: Option<String>,
    pub select: Option<Select>,
    pub candidates: Vec<Candidate>,
    /// The selected candidate's index in `candidates`.
    pub chosen: Option<usize>,
    /// How the choice was made, when not the plain argmax.
    pub selected: Option<String>,
}

impl Resolution {
    fn bare(status: Status, reason: Option<String>) -> Self {
        Self {
            status,
            reason,
            notes: Vec::new(),
            answer: None,
            rule: None,
            rule_when: None,
            fallback: None,
            select: None,
            candidates: Vec::new(),
            chosen: None,
            selected: None,
        }
    }

    pub fn off() -> Self {
        Self::bare(Status::Off, None)
    }

    pub fn error(reason: impl Into<String>) -> Self {
        Self::bare(Status::Error, Some(reason.into()))
    }

    pub fn no_rules() -> Self {
        Self::bare(Status::Escalate, Some("no rules to match".into()))
    }

    /// The selected profile.
    pub fn profile(&self) -> Option<&Profile> {
        self.chosen.map(|i| &self.candidates[i].profile)
    }
}

/// Every provider family the config's quota-priced profiles name, so a
/// quota source can fetch readings it would otherwise lack.
pub fn providers(config: &DispatchConfig, families: &ProviderFamilies) -> Vec<String> {
    let mut out: Vec<String> = config
        .rules
        .iter()
        .flat_map(|r| &r.profiles)
        .chain(&config.default)
        .filter(|p| !p.is_budget())
        .filter_map(|p| provider_of(p, families).map(str::to_string))
        .collect();
    out.sort();
    out.dedup();
    out
}

fn provider_of<'a>(p: &'a Profile, families: &'a ProviderFamilies) -> Option<&'a str> {
    p.provider.as_deref().or_else(|| families.get(&p.harness))
}

struct Judge<'a> {
    quota: &'a QuotaSnapshot,
    families: &'a ProviderFamilies,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FloorState {
    None,
    Unknown,
    Below,
    Ok,
}

impl Judge<'_> {
    fn rows(&self, provider: &str) -> &[QuotaRow] {
        self.quota
            .provider(provider)
            .map(|p| p.rows.as_slice())
            .unwrap_or_default()
    }

    fn applicable(&self, provider: &str, model: &str) -> Vec<&QuotaRow> {
        let bare = model.rsplit('/').next().unwrap_or(model);
        self.rows(provider)
            .iter()
            .filter(|r| {
                r.scope == "all_models"
                    || r.scope == "all_products"
                    || (!model.is_empty()
                        && (r.scope == format!("model:{bare}")
                            || r.scope == format!("product:{bare}")))
            })
            .collect()
    }

    fn floor_state(&self, floor: Option<&Floor>, provider: Option<&str>) -> FloorState {
        let Some(f) = floor else {
            return FloorState::None;
        };
        let Some(p) = provider.and_then(|p| self.quota.provider(p)) else {
            return FloorState::Unknown;
        };
        if !p.measured() {
            return FloorState::Unknown;
        }
        let matches: Vec<&QuotaRow> = p.rows.iter().filter(|r| r.scope == f.scope).collect();
        if matches.is_empty() || matches.iter().any(|r| r.status != "known") {
            FloorState::Unknown
        } else if matches.iter().any(|r| below(r, f.min_percent)) {
            FloorState::Below
        } else {
            FloorState::Ok
        }
    }

    fn evaluate(&self, c: &Profile) -> Candidate {
        let p = provider_of(c, self.families);
        if c.is_budget() {
            return Candidate::new(c, p, Verdict::Budget);
        }
        let Some(p) = p else {
            return Candidate::new(
                c,
                None,
                Verdict::NotEligible {
                    reason: format!(
                        "no provider family for harness {}; declare provider on the profile",
                        c.harness
                    ),
                },
            );
        };
        let Some(prov) = self.quota.provider(p) else {
            return Candidate::new(
                c,
                Some(p),
                Verdict::Unranked {
                    reason: format!("provider {p} not in the quota snapshot"),
                    unknown: true,
                },
            );
        };
        let rows = self.applicable(p, c.model.as_deref().unwrap_or(""));
        let bounds: Vec<Bound> = rows
            .iter()
            .map(|r| Bound {
                scope: r.scope.clone(),
                status: r.status.clone(),
                percent_remaining: r.percent_remaining,
                runway: r.runway.clone(),
                spend_priority: r.spend_priority,
            })
            .collect();
        let floor = self.floor_state(c.floor.as_ref(), Some(p));
        let with = |verdict: Verdict, row: Option<&QuotaRow>| {
            let mut cand = Candidate::new(c, Some(p), verdict).at(row);
            cand.bounds = bounds.clone();
            cand
        };
        let floor_row = || {
            c.floor
                .as_ref()
                .and_then(|f| self.rows(p).iter().find(|r| r.scope == f.scope))
        };

        if let Some(bad) = rows
            .iter()
            .find(|r| r.runway.as_deref() == Some("exhausted_now"))
        {
            return with(
                Verdict::NotEligible {
                    reason: format!("runway exhausted_now at {}", bad.scope),
                },
                Some(bad),
            );
        }
        if let Some(bad) = rows.iter().find(|r| {
            r.status == "known"
                && r.percent_remaining.is_some_and(|v| v <= 0.0)
                && !r.overage_allowed
        }) {
            return with(
                Verdict::NotEligible {
                    reason: format!("0% remaining at {}", bad.scope),
                },
                Some(bad),
            );
        }
        if floor == FloorState::Below {
            let f = c.floor.as_ref().expect("floor");
            let row = self
                .rows(p)
                .iter()
                .find(|r| r.scope == f.scope && below(r, f.min_percent));
            let mut cand = with(
                Verdict::NotEligible {
                    reason: format!(
                        "profile floor {} below {}%",
                        f.scope,
                        fmt_num(f.min_percent)
                    ),
                },
                row,
            );
            cand.scope = Some(row.map_or(f.scope.clone(), |r| r.scope.clone()));
            return cand;
        }
        if !prov.measured() {
            return with(
                Verdict::Unranked {
                    reason: format!("provider {p} unmeasured ({})", prov.status),
                    unknown: true,
                },
                rows.first().copied(),
            );
        }
        if rows.is_empty() {
            return with(
                Verdict::Unranked {
                    reason: format!("no applicable quota row for provider {p}"),
                    unknown: true,
                },
                None,
            );
        }
        if floor == FloorState::Unknown {
            let f = c.floor.as_ref().expect("floor");
            let mut cand = with(
                Verdict::Unranked {
                    reason: format!("profile floor {} is unverifiable: not rankable", f.scope),
                    unknown: true,
                },
                floor_row(),
            );
            cand.scope = Some(f.scope.clone());
            return cand;
        }
        if let Some(bad) = rows.iter().find(|r| r.status != "known") {
            let mut cand = with(
                Verdict::Unranked {
                    reason: format!("quota row {} unknown: not rankable", bad.scope),
                    unknown: true,
                },
                None,
            );
            cand.scope = Some(bad.scope.clone());
            return cand;
        }
        if let Some(bad) = rows.iter().find(|r| r.spend_priority.is_none()) {
            return with(
                Verdict::Unranked {
                    reason: format!(
                        "spendPriority missing or non-numeric at {}: not rankable",
                        bad.scope
                    ),
                    unknown: false,
                },
                Some(bad),
            );
        }
        let limiting = rows
            .iter()
            .copied()
            .reduce(|a, b| {
                if b.spend_priority < a.spend_priority {
                    b
                } else {
                    a
                }
            })
            .expect("rows");
        let mut cand = with(Verdict::Eligible, Some(limiting));
        cand.spend_priority = limiting.spend_priority;
        cand.overage_active = rows.iter().any(|r| r.overage_active);
        cand
    }
}

/// `jq` compares `null < n` as true, so a missing percentage is below any
/// floor.
fn below(r: &QuotaRow, min: f64) -> bool {
    r.percent_remaining.is_none_or(|v| v < min)
}

/// What the answer selects from, before quota is weighed.
enum Answered<'a> {
    Invalid(String),
    Use {
        profiles: &'a [Profile],
        select: Select,
        note: String,
        /// `rule_N`, or `default` for the default profiles.
        source: String,
    },
    Escalate(String),
}

/// Resolve one task. `classification` is `None` when the classifier is off
/// (the result is then [`Status::Off`]); `quota` is the snapshot, or the
/// reason it could not be read.
pub fn resolve(
    config: &DispatchConfig,
    settings: &ClassifierSettings,
    classification: Option<&Classification>,
    quota: Result<&QuotaSnapshot, &str>,
    families: &ProviderFamilies,
) -> Resolution {
    let Some(classification) = classification else {
        return Resolution::off();
    };
    if config.rules.is_empty() {
        return Resolution::no_rules();
    }
    let (answer, failure) = match classification {
        Classification::Answered { answer } => (Some(answer), None),
        Classification::Failed { reason } => {
            if settings.on_failure != OnFailure::Default {
                return Resolution::error(reason.clone());
            }
            (None, Some(reason.clone()))
        }
    };
    let quota = match quota {
        Ok(q) => q,
        Err(e) => return Resolution::error(e),
    };
    let judge = Judge { quota, families };
    let default_select = config.default_select.unwrap_or_default();
    let defaults = |note: String| Answered::Use {
        profiles: &config.default,
        select: default_select,
        note,
        source: DEFAULT_CHOICE.to_string(),
    };

    let choice = answer.map_or(DEFAULT_CHOICE, |a| a.choice.as_str());
    let rule = config.rule(choice);
    let answer_use: &[Profile] = if choice != DEFAULT_CHOICE && rule.is_none() {
        &[]
    } else if let Some(r) = rule {
        &r.profiles
    } else {
        &config.default
    };
    let answered = if choice != DEFAULT_CHOICE && rule.is_none() {
        Answered::Invalid(format!("rule {choice} is not in the rules file"))
    } else if let Some(r) = rule {
        let floor = judge.floor_state(
            r.floor.as_ref(),
            r.floor.as_ref().and_then(|f| f.provider.as_deref()),
        );
        if r.approval.as_deref() == Some("captain") {
            Answered::Escalate(
                "rule requires the captain's explicit approval before dispatch".into(),
            )
        } else if floor == FloorState::Unknown {
            let f = r.floor.as_ref().expect("floor");
            Answered::Escalate(format!(
                "rule {choice} floor {}/{} is unverifiable",
                f.provider.as_deref().unwrap_or("null"),
                f.scope
            ))
        } else if floor == FloorState::Below {
            let f = r.floor.as_ref().expect("floor");
            defaults(format!(
                "rule {choice} floor {} below {}%: fall through to default",
                f.scope,
                fmt_num(f.min_percent)
            ))
        } else {
            Answered::Use {
                profiles: &r.profiles,
                select: r.select.unwrap_or_default(),
                note: "rule matched".into(),
                source: choice.to_string(),
            }
        }
    } else {
        defaults("no rule matched".into())
    };

    let below_floor = answer.is_some_and(|a| a.confidence < settings.confidence_floor);
    let fallback = if let Some(f) = failure {
        Some(f)
    } else if settings.on_failure != OnFailure::Default {
        None
    } else if let Answered::Invalid(e) = &answered {
        Some(e.clone())
    } else if below_floor {
        Some(format!(
            "confidence {} below floor {}",
            fmt_num(answer.expect("answer").confidence),
            fmt_num(settings.confidence_floor)
        ))
    } else {
        None
    };
    let sel = match &fallback {
        None => answered,
        Some(f) => defaults(format!(
            "classifier fell back to the default profiles (on_failure default): {f}"
        )),
    };

    let mut out = Resolution::bare(Status::Clear, None);
    out.fallback = fallback.clone();
    if let Some(a) = answer {
        out.answer = Some(a.clone());
        out.rule = Some(choice.to_string());
        let when = rule.map_or(DEFAULT_WHEN, |r| r.when.as_str());
        out.rule_when = Some(when.chars().take(60).collect());
    }
    let evaluate = |ps: &[Profile]| ps.iter().map(|p| judge.evaluate(p)).collect::<Vec<_>>();

    match sel {
        Answered::Invalid(e) => {
            out.status = Status::Error;
            out.reason = Some(e);
        }
        _ if fallback.is_none() && below_floor => {
            let a = answer.expect("answer");
            out.status = Status::Ambiguous;
            out.reason = Some(format!(
                "confidence {} below floor {}",
                fmt_num(a.confidence),
                fmt_num(settings.confidence_floor)
            ));
            out.candidates = evaluate(answer_use);
        }
        Answered::Escalate(e) => {
            out.status = Status::Escalate;
            out.reason = Some(e);
            out.candidates = evaluate(answer_use);
        }
        Answered::Use {
            profiles: [],
            note,
            source,
            ..
        } => {
            out.status = Status::Escalate;
            out.reason = Some(format!("no profiles configured for {source}"));
            out.notes.push(note);
        }
        Answered::Use {
            profiles,
            select: Select::Ordered,
            note,
            ..
        } => {
            out.select = Some(Select::Ordered);
            out.notes.push(note);
            out.candidates = evaluate(profiles);
            match out.candidates.iter().position(|c| c.verdict.eligible()) {
                None => {
                    out.status = Status::Escalate;
                    out.reason = Some("no eligible candidate in listed order".into());
                }
                Some(i)
                    if matches!(
                        out.candidates[i].verdict,
                        Verdict::Unranked { unknown: true, .. }
                    ) =>
                {
                    let c = &out.candidates[i];
                    out.status = Status::Escalate;
                    out.reason = Some(format!(
                        "first eligible candidate in listed order {} has unverifiable quota evidence: {}",
                        c.profile.label(),
                        c.verdict.reason()
                    ));
                }
                Some(i) => {
                    out.chosen = Some(i);
                    out.selected = Some("first eligible profile in listed order".into());
                }
            }
        }
        Answered::Use {
            profiles,
            select: Select::QuotaBalanced,
            note,
            ..
        } => {
            out.notes.push(note);
            out.candidates = evaluate(profiles);
            let cands = &out.candidates;
            let ranked: Vec<usize> = (0..cands.len()).filter(|&i| cands[i].ranked()).collect();
            let budget = (0..cands.len()).find(|&i| cands[i].verdict == Verdict::Budget);
            let mut unranked: Vec<&str> = cands
                .iter()
                .filter(|c| matches!(c.verdict, Verdict::Unranked { .. }))
                .filter_map(|c| c.provider.as_deref())
                .collect();
            let unranked_count = cands
                .iter()
                .filter(|c| matches!(c.verdict, Verdict::Unranked { .. }))
                .count();
            unranked.sort();
            unranked.dedup();
            let unranked_note = (unranked_count > 0).then(|| {
                format!(
                    "{unranked_count} eligible candidate(s) unranked ({})",
                    unranked.join(", ")
                )
            });
            if ranked.is_empty() {
                if let Some(b) = budget {
                    out.chosen = Some(b);
                    out.selected =
                        Some("budget-based, after every rankable quota candidate".into());
                    out.notes.extend(unranked_note);
                } else {
                    out.status = Status::Escalate;
                    out.reason = Some("no rankable eligible candidate".into());
                }
            } else {
                let best = ranked
                    .iter()
                    .map(|&i| cands[i].spend_priority.unwrap_or(f64::MIN))
                    .fold(f64::MIN, f64::max);
                let top: Vec<usize> = ranked
                    .iter()
                    .copied()
                    .filter(|&i| cands[i].spend_priority == Some(best))
                    .collect();
                if top.len() > 1 {
                    out.status = Status::Escalate;
                    out.reason = Some("genuine spendPriority tie".into());
                } else {
                    out.chosen = Some(top[0]);
                    out.notes.extend(unranked_note);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::OnFailure;

    fn config(json: &str) -> DispatchConfig {
        DispatchConfig::parse(json).unwrap()
    }

    fn settings() -> ClassifierSettings {
        ClassifierSettings::from_config(&config(r#"{"classifier":{"provider":"system1"}}"#), false)
    }

    fn answered(choice: &str, confidence: f64) -> Classification {
        Classification::Answered {
            answer: Answer {
                choice: choice.into(),
                confidence,
                probabilities: Default::default(),
                model: Some("jev-1".into()),
                latency_ms: None,
                input_tokens: None,
                output_tokens: None,
            },
        }
    }

    fn row(scope: &str, pct: f64, sp: f64) -> serde_json::Value {
        serde_json::json!({"scope": scope, "status": "known", "effectivePercentRemaining": pct,
            "runway": {"status": "ok"}, "selection": {"spendPriority": sp}})
    }

    fn quota(providers: serde_json::Value) -> QuotaSnapshot {
        QuotaSnapshot::from_value(&serde_json::json!({ "providers": providers })).unwrap()
    }

    fn two(claude: f64, codex: f64) -> QuotaSnapshot {
        quota(serde_json::json!([
            {"provider":"claude","quotaSemantics":{"status":"known","effectiveAvailability":[row("all_models", 70.0, claude)]}},
            {"provider":"codex","quotaSemantics":{"status":"known","effectiveAvailability":[row("all_models", 40.0, codex)]}}
        ]))
    }

    const RULES: &str = r#"{"rules":[
        {"when":"trivial","use":[{"harness":"claude","model":"claude-sonnet-5"},{"harness":"codex","model":"gpt-5.5"}]},
        {"when":"risky","approval":"captain","use":{"harness":"claude"}},
        {"when":"ordered","select":"ordered","use":[{"harness":"pi"},{"harness":"codex"},{"harness":"claude"}]}],
      "default":[{"harness":"claude","effort":"medium"}]}"#;

    fn run(c: &Classification, q: &QuotaSnapshot) -> Resolution {
        resolve(
            &config(RULES),
            &settings(),
            Some(c),
            Ok(q),
            &ProviderFamilies::builtin(),
        )
    }

    #[test]
    fn off_and_no_rules() {
        let q = two(1.0, 2.0);
        let f = ProviderFamilies::builtin();
        let r = resolve(&config(RULES), &settings(), None, Ok(&q), &f);
        assert_eq!(r.status, Status::Off);
        let r = resolve(
            &config("{}"),
            &settings(),
            Some(&answered("default", 1.0)),
            Ok(&q),
            &f,
        );
        assert_eq!(r.status, Status::Escalate);
        assert_eq!(r.reason.as_deref(), Some("no rules to match"));
    }

    #[test]
    fn quota_balanced_takes_highest_spend_priority() {
        let r = run(&answered("rule_1", 0.9), &two(0.2, 0.7));
        assert_eq!(r.status, Status::Clear);
        assert_eq!(r.profile().unwrap().harness, "codex");
        assert_eq!(r.rule.as_deref(), Some("rule_1"));
        assert_eq!(
            r.candidates[1].evidence().unwrap(),
            "provider=codex scope=all_models remaining=40% spendPriority=0.7 runway=ok"
        );
        let r = run(&answered("rule_1", 0.9), &two(0.5, 0.5));
        assert_eq!(r.status, Status::Escalate);
        assert_eq!(r.reason.as_deref(), Some("genuine spendPriority tie"));
    }

    #[test]
    fn approval_escalates_with_candidates() {
        let r = run(&answered("rule_2", 0.9), &two(0.2, 0.7));
        assert_eq!(r.status, Status::Escalate);
        assert!(r.reason.unwrap().contains("approval"));
        assert_eq!(r.candidates.len(), 1);
    }

    #[test]
    fn ordered_skips_ineligible_but_not_unverifiable() {
        // pi has no provider family: not eligible; codex next.
        let r = run(&answered("rule_3", 0.9), &two(0.2, 0.7));
        assert_eq!(r.status, Status::Clear);
        assert_eq!(r.profile().unwrap().harness, "codex");
        assert!(!r.candidates[0].verdict.eligible());
        // codex missing from the snapshot: eligible but unverifiable, so
        // ordered escalates rather than skip it.
        let q = quota(serde_json::json!([
            {"provider":"claude","quotaSemantics":{"status":"known","effectiveAvailability":[row("all_models", 70.0, 0.4)]}}
        ]));
        let r = run(&answered("rule_3", 0.9), &q);
        assert_eq!(r.status, Status::Escalate);
        assert!(r.reason.unwrap().contains("codex:-"));
    }

    #[test]
    fn below_confidence_is_ambiguous_or_falls_back() {
        let r = run(&answered("rule_1", 0.3), &two(0.2, 0.7));
        assert_eq!(r.status, Status::Ambiguous);
        assert_eq!(r.reason.as_deref(), Some("confidence 0.3 below floor 0.6"));
        assert_eq!(r.candidates.len(), 2);

        let mut s = settings();
        s.on_failure = OnFailure::Default;
        let q = two(0.2, 0.7);
        let f = ProviderFamilies::builtin();
        let r = resolve(
            &config(RULES),
            &s,
            Some(&answered("rule_1", 0.3)),
            Ok(&q),
            &f,
        );
        assert_eq!(r.status, Status::Clear);
        assert_eq!(
            r.fallback.as_deref(),
            Some("confidence 0.3 below floor 0.6")
        );
        assert_eq!(r.profile().unwrap().effort.as_deref(), Some("medium"));

        let failed = Classification::Failed {
            reason: "http 500".into(),
        };
        let r = resolve(&config(RULES), &s, Some(&failed), Ok(&q), &f);
        assert_eq!(r.status, Status::Clear);
        assert!(r.rule.is_none());
        let r = resolve(&config(RULES), &settings(), Some(&failed), Ok(&q), &f);
        assert_eq!(r.status, Status::Error);
        assert_eq!(r.reason.as_deref(), Some("http 500"));
    }

    #[test]
    fn exhausted_and_zero_are_not_eligible() {
        let q = quota(serde_json::json!([
            {"provider":"claude","quotaSemantics":{"status":"known","effectiveAvailability":[
                {"scope":"all_models","status":"known","effectivePercentRemaining":0,"runway":{"status":"ok"},"selection":{"spendPriority":0.9}}]}},
            {"provider":"codex","quotaSemantics":{"status":"known","effectiveAvailability":[
                {"scope":"model:gpt-5.5","status":"known","effectivePercentRemaining":30,"runway":{"status":"exhausted_now"},"selection":{"spendPriority":0.1}}]}}
        ]));
        let r = run(&answered("rule_1", 0.9), &q);
        assert_eq!(r.status, Status::Escalate);
        assert_eq!(
            r.candidates[0].verdict.reason(),
            "0% remaining at all_models"
        );
        assert_eq!(
            r.candidates[1].verdict.reason(),
            "runway exhausted_now at model:gpt-5.5"
        );
    }

    #[test]
    fn overage_is_a_budget_not_a_block() {
        let q = quota(serde_json::json!([
            {"provider":"claude","quotaSemantics":{"status":"known","effectiveAvailability":[
                {"scope":"all_models","status":"known","effectivePercentRemaining":0,"runway":{"status":"ok"},
                 "selection":{"spendPriority":0.9},"overage":{"allowed":true,"active":true}}]}},
            {"provider":"codex","quotaSemantics":{"status":"known","effectiveAvailability":[row("all_models", 40.0, 0.2)]}}
        ]));
        let r = run(&answered("rule_1", 0.9), &q);
        assert_eq!(r.profile().unwrap().harness, "claude");
        assert!(r.candidates[0].overage_active);
    }

    #[test]
    fn budget_only_when_nothing_ranks() {
        let c = config(
            r#"{"rules":[{"when":"x","use":[{"harness":"claude"},{"harness":"local","pricing":"budget"}]}]}"#,
        );
        let f = ProviderFamilies::builtin();
        let empty = quota(serde_json::json!([]));
        let r = resolve(
            &c,
            &settings(),
            Some(&answered("rule_1", 0.9)),
            Ok(&empty),
            &f,
        );
        assert_eq!(r.status, Status::Clear);
        assert_eq!(r.profile().unwrap().harness, "local");
        assert_eq!(r.notes[1], "1 eligible candidate(s) unranked (claude)");
        let q = two(0.3, 0.1);
        let r = resolve(&c, &settings(), Some(&answered("rule_1", 0.9)), Ok(&q), &f);
        assert_eq!(r.profile().unwrap().harness, "claude");
    }

    #[test]
    fn floors() {
        let c = config(
            r#"{"rules":[{"when":"x","floor":{"provider":"claude","scope":"all_models","min_percent":80},
                 "use":{"harness":"codex"}},
                {"when":"y","use":{"harness":"claude","floor":{"scope":"all_models","min_percent":90}}}],
               "default":{"harness":"claude","effort":"low"}}"#,
        );
        let f = ProviderFamilies::builtin();
        let q = two(0.3, 0.1);
        // Rule floor below (70 < 80): fall through to the default.
        let r = resolve(&c, &settings(), Some(&answered("rule_1", 0.9)), Ok(&q), &f);
        assert_eq!(r.status, Status::Clear);
        assert_eq!(r.profile().unwrap().effort.as_deref(), Some("low"));
        assert!(r.notes[0].contains("fall through"));
        // Profile floor below (70 < 90): not eligible.
        let r = resolve(&c, &settings(), Some(&answered("rule_2", 0.9)), Ok(&q), &f);
        assert_eq!(r.status, Status::Escalate);
        assert_eq!(
            r.candidates[0].verdict.reason(),
            "profile floor all_models below 90%"
        );
        // Rule floor provider missing from the snapshot: unverifiable.
        let only_codex = quota(serde_json::json!([
            {"provider":"codex","quotaSemantics":{"status":"known","effectiveAvailability":[row("all_models", 40.0, 0.2)]}}
        ]));
        let r = resolve(
            &c,
            &settings(),
            Some(&answered("rule_1", 0.9)),
            Ok(&only_codex),
            &f,
        );
        assert_eq!(r.status, Status::Escalate);
        assert!(r.reason.unwrap().contains("unverifiable"));
    }

    #[test]
    fn invalid_choice_and_quota_failure() {
        let q = two(0.3, 0.1);
        let r = run(&answered("rule_9", 0.9), &q);
        assert_eq!(r.status, Status::Error);
        assert_eq!(
            r.reason.as_deref(),
            Some("rule rule_9 is not in the rules file")
        );
        let f = ProviderFamilies::builtin();
        let r = resolve(
            &config(RULES),
            &settings(),
            Some(&answered("rule_1", 0.9)),
            Err("quota-axi --json failed"),
            &f,
        );
        assert_eq!(r.status, Status::Error);
    }

    #[test]
    fn model_scopes_bound_only_their_model() {
        let q = quota(serde_json::json!([
            {"provider":"claude","quotaSemantics":{"status":"known","effectiveAvailability":[
                row("all_models", 70.0, 0.6), row("model:claude-sonnet-5", 10.0, 0.1), row("model:claude-opus-5", 90.0, 0.9)]}},
            {"provider":"codex","quotaSemantics":{"status":"known","effectiveAvailability":[row("all_models", 40.0, 0.3)]}}
        ]));
        let r = run(&answered("rule_1", 0.9), &q);
        // claude-sonnet-5 is limited by its own model row (0.1).
        assert_eq!(r.candidates[0].spend_priority, Some(0.1));
        assert_eq!(
            r.candidates[0].scope.as_deref(),
            Some("model:claude-sonnet-5")
        );
        assert_eq!(r.candidates[0].bounds.len(), 2);
        assert_eq!(r.profile().unwrap().harness, "codex");
    }
}
