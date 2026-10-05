//! Dispatch records: why each task got its agent (ADR-11).
//!
//! The engine decides; the daemon only records. Each time a task's worker is
//! spawned (a new engine spawn generation), the projector reads which agent
//! it was started with and, for a fresh first spawn, the engine's own
//! dispatch resolution of the task's brief. [`build`] turns those into a
//! [`DispatchRecord`]: the rule matched, every candidate with its pass or
//! fail reason, the chosen agent, the classifier, and who decided.

use quark_systems::{
    DispatchChoice, DispatchClassifier, DispatchDecider, DispatchRecord, DispatchResolution,
    DispatchStatus, DispatchTrigger,
};

use crate::engine::{EngineResolution, EngineSpawn};

/// A first spawn older than this when the daemon sees it is recorded without
/// running the resolution, whose answer could no longer be the one the
/// coordinator acted on.
pub const FRESH_SPAWN_SECS: i64 = 10 * 60;

/// The classifier provider recorded when none was consulted.
pub const NO_CLASSIFIER: &str = "none";

/// The provider name for a classifier behind the System-1 API, which is
/// what the engine's resolution calls.
pub const SYSTEM1: &str = "system1";

/// What became of the engine's dispatch resolution for one spawn.
#[derive(Debug, Clone)]
pub enum Resolved {
    /// It ran and reported this.
    Ran(Box<EngineResolution>),
    /// It could not run or its output could not be read.
    Failed(String),
    /// It was not run, for this reason.
    NotRun(String),
}

/// Whether a first spawn is recent enough to resolve now.
pub fn fresh(spawn: &EngineSpawn, now_secs: i64) -> bool {
    spawn
        .spawned_at
        .is_none_or(|t| now_secs - t <= FRESH_SPAWN_SECS)
}

/// The record for one spawn. `id` and `recorded_at` are left for the store.
pub fn build(
    task_id: &str,
    project_id: &str,
    spawn: &EngineSpawn,
    trigger: DispatchTrigger,
    resolved: Resolved,
) -> DispatchRecord {
    let chosen = DispatchChoice {
        harness: spawn.harness.clone(),
        model: spawn.model.clone(),
        effort: spawn.effort.clone(),
        account: None,
    };
    let agent = label(&chosen);
    let none = DispatchClassifier {
        provider: NO_CLASSIFIER.into(),
        model: None,
        confidence: None,
    };
    let not_consulted = |reason: String| DispatchResolution {
        status: DispatchStatus::NotConsulted,
        reason: Some(reason),
        notes: Vec::new(),
        output: None,
    };
    let mut record = DispatchRecord {
        id: String::new(),
        task_id: task_id.into(),
        project_id: project_id.into(),
        trigger,
        decided_by: DispatchDecider::Coordinator,
        summary: String::new(),
        rule: None,
        resolution: not_consulted(String::new()),
        candidates: Vec::new(),
        chosen: chosen.clone(),
        classifier: none,
        recorded_at: String::new(),
    };

    match resolved {
        Resolved::NotRun(reason) => {
            if trigger == DispatchTrigger::Relaunch {
                record.decided_by = DispatchDecider::Relaunch;
                record.summary = format!(
                    "Relaunched in the same worktree with {agent}; dispatch rules were not consulted again."
                );
            } else {
                record.summary = format!("{reason}; the coordinator picked {agent}.");
            }
            record.resolution = not_consulted(reason);
        }
        Resolved::Failed(error) => {
            record.summary = format!(
                "The dispatch resolution failed ({error}); the coordinator picked {agent}."
            );
            record.resolution = DispatchResolution {
                status: DispatchStatus::Error,
                reason: Some(error),
                notes: Vec::new(),
                output: None,
            };
        }
        Resolved::Ran(r) => {
            let r = *r;
            if r.classifier_consulted {
                record.classifier = DispatchClassifier {
                    provider: SYSTEM1.into(),
                    model: r.classifier_model.clone(),
                    confidence: r.confidence,
                };
            }
            let mut notes = r.notes.clone();
            let selected = r
                .profile
                .as_ref()
                .filter(|_| r.status == DispatchStatus::Clear);
            record.summary = match (r.status, selected) {
                (DispatchStatus::Off, _) => format!(
                    "No classifier is configured (provider: none), so the coordinator picked {agent}."
                ),
                (DispatchStatus::Clear, Some(p)) if same_agent(p, &chosen) => {
                    record.decided_by = DispatchDecider::Classifier;
                    format!("{} The resolution selected {agent}.", matched(&r))
                }
                (DispatchStatus::Clear, Some(p)) => {
                    let note = format!(
                        "The coordinator overrode the selected profile {}.",
                        label(p)
                    );
                    notes.push(note.clone());
                    format!("{} {note} It picked {agent}.", matched(&r))
                }
                (status, _) => {
                    let why = r
                        .reason
                        .as_deref()
                        .map(|w| format!(": {w}"))
                        .unwrap_or_default();
                    format!(
                        "The dispatch resolution was {}{why}, so the coordinator picked {agent}.",
                        status_word(status)
                    )
                }
            };
            record.rule = r.rule;
            record.candidates = r.candidates;
            record.resolution = DispatchResolution {
                status: r.status,
                reason: r.reason,
                notes,
                output: r.output,
            };
        }
    }
    record
}

/// `harness`, `harness:model` or `harness:model (effort)`.
pub fn label(c: &DispatchChoice) -> String {
    let mut s = c.harness.clone();
    if let Some(m) = &c.model {
        s = format!("{s}:{m}");
    }
    if let Some(e) = &c.effort {
        s = format!("{s} ({e} effort)");
    }
    s
}

fn same_agent(a: &DispatchChoice, b: &DispatchChoice) -> bool {
    a.harness == b.harness && a.model == b.model && a.effort == b.effort
}

fn matched(r: &EngineResolution) -> String {
    let rule = match &r.rule {
        Some(rule) => match &rule.when {
            Some(when) => format!("rule {} ({when})", rule.id),
            None => format!("rule {}", rule.id),
        },
        None => "a rule".into(),
    };
    match r.confidence {
        Some(c) => format!("The classifier matched {rule} at {c:.2} confidence."),
        None => format!("The classifier matched {rule}."),
    }
}

fn status_word(s: DispatchStatus) -> &'static str {
    match s {
        DispatchStatus::Clear => "clear",
        DispatchStatus::Ambiguous => "ambiguous",
        DispatchStatus::Escalate => "escalated",
        DispatchStatus::Error => "an error",
        DispatchStatus::Off => "off",
        DispatchStatus::NotConsulted => "not consulted",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quark_systems::{DispatchCandidate, DispatchRule};

    fn spawn(harness: &str, model: Option<&str>) -> EngineSpawn {
        EngineSpawn {
            generation: "s1.1.1".into(),
            harness: harness.into(),
            model: model.map(str::to_string),
            effort: None,
            spawned_at: Some(1_000),
            project: Some("quark".into()),
        }
    }

    fn clear(profile: (&str, Option<&str>)) -> EngineResolution {
        EngineResolution {
            status: DispatchStatus::Clear,
            rule: Some(DispatchRule {
                id: "rule_2".into(),
                when: Some("A trivial mechanical edit.".into()),
            }),
            reason: None,
            notes: vec!["rule matched".into()],
            candidates: vec![
                DispatchCandidate {
                    harness: "claude".into(),
                    model: Some("sonnet".into()),
                    passed: true,
                    reason: "eligible".into(),
                    evidence: Some("provider=claude remaining=79%".into()),
                },
                DispatchCandidate {
                    harness: "codex".into(),
                    model: Some("gpt-5.6-luna".into()),
                    passed: false,
                    reason: "0% remaining at all_models".into(),
                    evidence: None,
                },
            ],
            profile: Some(DispatchChoice {
                harness: profile.0.into(),
                model: profile.1.map(str::to_string),
                effort: None,
                account: None,
            }),
            classifier_consulted: true,
            classifier_model: Some("jev-1.13.0".into()),
            confidence: Some(0.91),
            output: Some("dispatch-resolve:\n  status: clear".into()),
        }
    }

    #[test]
    fn classifier_decides_when_the_selected_profile_was_used() {
        let r = build(
            "tsk_1",
            "prj_1",
            &spawn("claude", Some("sonnet")),
            DispatchTrigger::Spawn,
            Resolved::Ran(Box::new(clear(("claude", Some("sonnet"))))),
        );
        assert_eq!(r.decided_by, DispatchDecider::Classifier);
        assert_eq!(r.rule.as_ref().unwrap().id, "rule_2");
        assert_eq!(r.candidates.len(), 2);
        assert!(!r.candidates[1].passed);
        assert_eq!(r.classifier.provider, SYSTEM1);
        assert_eq!(r.classifier.confidence, Some(0.91));
        assert_eq!(r.resolution.status, DispatchStatus::Clear);
        assert_eq!(
            r.summary,
            "The classifier matched rule rule_2 (A trivial mechanical edit.) at 0.91 confidence. \
             The resolution selected claude:sonnet."
        );
    }

    #[test]
    fn a_different_agent_means_the_coordinator_overrode() {
        let r = build(
            "tsk_1",
            "prj_1",
            &spawn("codex", None),
            DispatchTrigger::Spawn,
            Resolved::Ran(Box::new(clear(("claude", Some("sonnet"))))),
        );
        assert_eq!(r.decided_by, DispatchDecider::Coordinator);
        assert!(r
            .summary
            .contains("overrode the selected profile claude:sonnet"));
        assert!(r.resolution.notes.iter().any(|n| n.contains("overrode")));
        assert_eq!(r.chosen.harness, "codex");
    }

    #[test]
    fn no_classifier_means_the_coordinator_picked() {
        let off = EngineResolution {
            status: DispatchStatus::Off,
            rule: None,
            reason: None,
            notes: vec![],
            candidates: vec![],
            profile: None,
            classifier_consulted: false,
            classifier_model: None,
            confidence: None,
            output: None,
        };
        let r = build(
            "tsk_1",
            "prj_1",
            &spawn("claude", None),
            DispatchTrigger::Spawn,
            Resolved::Ran(Box::new(off)),
        );
        assert_eq!(r.decided_by, DispatchDecider::Coordinator);
        assert_eq!(r.classifier.provider, "none");
        assert_eq!(r.classifier.model, None);
        assert_eq!(r.chosen.account, None);
        assert_eq!(
            r.summary,
            "No classifier is configured (provider: none), so the coordinator picked claude."
        );
    }

    #[test]
    fn unclear_failed_and_relaunched() {
        let mut esc = clear(("claude", Some("sonnet")));
        esc.status = DispatchStatus::Escalate;
        esc.profile = None;
        esc.reason = Some("genuine spendPriority tie".into());
        let r = build(
            "t",
            "p",
            &spawn("claude", None),
            DispatchTrigger::Spawn,
            Resolved::Ran(Box::new(esc)),
        );
        assert_eq!(r.decided_by, DispatchDecider::Coordinator);
        assert_eq!(
            r.classifier.provider, SYSTEM1,
            "the classifier was still asked"
        );
        assert!(r.summary.contains("escalated: genuine spendPriority tie"));

        let r = build(
            "t",
            "p",
            &spawn("claude", None),
            DispatchTrigger::Spawn,
            Resolved::Failed("timed out".into()),
        );
        assert_eq!(r.resolution.status, DispatchStatus::Error);
        assert_eq!(r.resolution.reason.as_deref(), Some("timed out"));

        let r = build(
            "t",
            "p",
            &spawn("codex", Some("gpt-5.6-luna")),
            DispatchTrigger::Relaunch,
            Resolved::NotRun("relaunched".into()),
        );
        assert_eq!(r.decided_by, DispatchDecider::Relaunch);
        assert_eq!(r.resolution.status, DispatchStatus::NotConsulted);
        assert!(r
            .summary
            .starts_with("Relaunched in the same worktree with codex:gpt-5.6-luna"));
    }

    #[test]
    fn freshness() {
        let s = spawn("claude", None);
        assert!(fresh(&s, 1_000 + FRESH_SPAWN_SECS));
        assert!(!fresh(&s, 1_001 + FRESH_SPAWN_SECS));
        let undated = EngineSpawn {
            spawned_at: None,
            ..s
        };
        assert!(fresh(&undated, i64::MAX));
    }
}
