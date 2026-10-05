//! Test a task description against a Project's dispatch rules (ADR-11).
//!
//! The engine decides here as it does for a real task: its dispatch
//! resolution runs on the description, and [`build`] reports the rule it
//! matched and the profile it selected, in the words a dispatch record uses
//! ([`crate::dispatch`]). With no classifier the coordinator would pick, so
//! every profile the rules list is a candidate.
//!
//! Each candidate also says whether a worker could be started with it now:
//! harness installed and model accepted from the harness adapters, account
//! health from the accounts the profile's pool names, and quota headroom
//! from the resolution's own quota evidence and each account's reading.
//! Nothing is dispatched or recorded.

use quark_systems::{
    Account, AgentConfig, AgentRole, AuthState, DispatchCandidate, DispatchCheck,
    DispatchCheckKind, DispatchChoice, DispatchDecider, DispatchResolution, DispatchRule,
    DispatchStatus, DispatchTest, DispatchTestCandidate, HarnessAuth, HarnessInfo, QuotaState,
};

use crate::crew_dispatch::{CrewDispatchConfig, Profile};
use crate::dispatch::{self, Resolved};
use crate::harness::HarnessRegistry;

/// The id of the rule that lists the default profiles.
pub const DEFAULT_RULE: &str = "default";

/// What this machine can run, as of the test.
pub struct Host<'a> {
    pub harnesses: &'a HarnessRegistry,
    pub infos: &'a [HarnessInfo],
    pub accounts: &'a [Account],
}

/// A profile and the rule that lists it.
struct Listed<'a> {
    rule: DispatchRule,
    rule_name: Option<String>,
    profile: &'a Profile,
}

/// The outcome for a description `resolved` came from, against the rules in
/// `config`.
pub fn build(
    project_id: &str,
    config: &CrewDispatchConfig,
    resolved: Resolved,
    host: &Host,
) -> DispatchTest {
    let listed = listed(config);
    let all = || -> Vec<DispatchTestCandidate> {
        listed.iter().map(|l| candidate(l, None, host)).collect()
    };
    let mut test = DispatchTest {
        project_id: project_id.into(),
        decided_by: DispatchDecider::Coordinator,
        summary: String::new(),
        rule: None,
        resolution: DispatchResolution {
            status: DispatchStatus::NotConsulted,
            reason: None,
            notes: Vec::new(),
            output: None,
        },
        classifier: dispatch::no_classifier(),
        chosen: None,
        candidates: Vec::new(),
    };

    let lead = match resolved {
        Resolved::NotRun(reason) => {
            test.candidates = all();
            test.resolution.reason = Some(reason.clone());
            format!("{reason}; the coordinator would pick.")
        }
        Resolved::Failed(error) => {
            test.candidates = all();
            test.resolution.status = DispatchStatus::Error;
            test.resolution.reason = Some(error.clone());
            format!("The dispatch resolution failed ({error}); the coordinator would pick.")
        }
        Resolved::Ran(r) => {
            let r = *r;
            test.classifier = dispatch::classifier(&r);
            let selected = r
                .profile
                .as_ref()
                .filter(|_| r.status == DispatchStatus::Clear);
            let lead = match (r.status, selected) {
                (DispatchStatus::Off, _) => {
                    "No classifier is configured (provider: none), so the coordinator would pick."
                        .to_string()
                }
                (_, Some(p)) => {
                    let chosen = DispatchChoice {
                        harness: harness_id(&p.harness, host),
                        account: None,
                        ..p.clone()
                    };
                    let lead = format!(
                        "{} The resolution would select {}.",
                        dispatch::matched(&r),
                        dispatch::label(&chosen)
                    );
                    test.decided_by = dispatch::decider(&r);
                    test.chosen = Some(chosen);
                    lead
                }
                (status, None) => {
                    let why = r
                        .reason
                        .as_deref()
                        .map(|w| format!(": {w}"))
                        .unwrap_or_default();
                    format!(
                        "The dispatch resolution was {}{why}, so the coordinator would pick.",
                        dispatch::status_word(status)
                    )
                }
            };
            let applied = dispatch::rule(&r);
            test.candidates = if r.candidates.is_empty() {
                all()
            } else {
                weighed(&r.candidates, applied.as_ref(), &listed, host)
            };
            // The engine excerpts the rule's condition; the rules file has
            // all of it.
            test.rule = applied.map(|rule| {
                let when = listed
                    .iter()
                    .find(|l| l.rule.id == rule.id)
                    .and_then(|l| l.rule.when.clone())
                    .or(rule.when);
                DispatchRule { id: rule.id, when }
            });
            test.resolution = DispatchResolution {
                status: r.status,
                reason: r.reason,
                notes: r.notes,
                output: r.output,
            };
            lead
        }
    };
    let passing = test.candidates.iter().filter(|c| c.passed).count();
    test.summary = format!(
        "{lead} {passing} of {} candidate{} could start a worker now.",
        test.candidates.len(),
        if test.candidates.len() == 1 { "" } else { "s" }
    );
    test
}

/// Every profile the rules list, rule by rule, then the default profiles.
/// Rule ids are the engine's: `rule_<n>` from one, and `default`.
fn listed(config: &CrewDispatchConfig) -> Vec<Listed<'_>> {
    let rules = config.rules.iter().enumerate().flat_map(|(i, rule)| {
        rule.profiles.as_slice().iter().map(move |profile| Listed {
            rule: DispatchRule {
                id: format!("rule_{}", i + 1),
                when: Some(rule.when.clone()),
            },
            rule_name: rule.name.clone(),
            profile,
        })
    });
    let default = config
        .default
        .iter()
        .flat_map(|d| d.as_slice())
        .map(|profile| Listed {
            rule: DispatchRule {
                id: DEFAULT_RULE.into(),
                when: None,
            },
            rule_name: None,
            profile,
        });
    rules.chain(default).collect()
}

/// The candidates the resolution weighed, each joined to the profile the
/// rules list for it: one of the matched rule's, or a default profile when
/// the rule fell through to the default.
fn weighed(
    engine: &[DispatchCandidate],
    matched: Option<&DispatchRule>,
    listed: &[Listed],
    host: &Host,
) -> Vec<DispatchTestCandidate> {
    let matched_id = matched.map_or(DEFAULT_RULE, |r| r.id.as_str());
    let mut used = vec![false; listed.len()];
    engine
        .iter()
        .map(|c| {
            let same = |l: &Listed| l.profile.harness == c.harness && l.profile.model == c.model;
            let found = [matched_id, DEFAULT_RULE].iter().find_map(|id| {
                listed
                    .iter()
                    .enumerate()
                    .position(|(i, l)| !used[i] && l.rule.id == *id && same(l))
            });
            match found {
                Some(i) => {
                    used[i] = true;
                    candidate(&listed[i], Some(c), host)
                }
                None => {
                    let profile = Profile {
                        harness: c.harness.clone(),
                        model: c.model.clone(),
                        effort: None,
                        provider: None,
                        floor: None,
                        pricing: None,
                        pool: None,
                    };
                    let unlisted = Listed {
                        rule: matched.cloned().unwrap_or(DispatchRule {
                            id: DEFAULT_RULE.into(),
                            when: None,
                        }),
                        rule_name: None,
                        profile: &profile,
                    };
                    candidate(&unlisted, Some(c), host)
                }
            }
        })
        .collect()
}

fn candidate(
    listed: &Listed,
    engine: Option<&DispatchCandidate>,
    host: &Host,
) -> DispatchTestCandidate {
    let p = listed.profile;
    let checks = checks(p, engine, host);
    let failed = checks.iter().find(|c| !c.passed);
    let quota = checks
        .iter()
        .find(|c| c.check == DispatchCheckKind::QuotaHeadroom)
        .map(|c| c.detail.clone());
    DispatchTestCandidate {
        rule: listed.rule.clone(),
        rule_name: listed.rule_name.clone(),
        harness: harness_id(&p.harness, host),
        model: p.model.clone(),
        effort: p.effort.clone(),
        pool: p.pool.clone(),
        passed: failed.is_none(),
        reason: match (failed, engine) {
            (Some(f), _) => f.detail.clone(),
            (None, Some(e)) => e.reason.clone(),
            (None, None) => "eligible".into(),
        },
        evidence: engine.and_then(|e| e.evidence.clone()).or(quota),
        checks,
    }
}

/// Quark's id for a harness the rules or the engine name.
fn harness_id(name: &str, host: &Host) -> String {
    host.harnesses
        .resolve(name)
        .map_or(name, |h| h.id())
        .to_string()
}

/// The accounts a profile can run under.
enum Runs<'a> {
    /// None, for this reason.
    Nowhere(String),
    /// A harness without accounts: its own credential.
    Harness(&'a HarnessAuth, &'a str),
    Accounts(Vec<&'a Account>),
}

fn checks(p: &Profile, engine: Option<&DispatchCandidate>, host: &Host) -> Vec<DispatchCheck> {
    let check = |check, passed, detail: String| DispatchCheck {
        check,
        passed,
        detail,
    };
    let harness = host.harnesses.resolve(&p.harness);
    let info = harness.and_then(|h| host.infos.iter().find(|i| i.id == h.id()));
    let unknown = || format!("unknown harness `{}`", p.harness);

    let installed = match info {
        Some(i) if i.install.installed => {
            let mut found = i.name.clone();
            for part in [&i.install.version, &i.install.path].into_iter().flatten() {
                found = format!("{found} {part}");
            }
            (true, found)
        }
        Some(i) => (
            false,
            format!("{} is not installed: {}", i.name, i.install.install_hint),
        ),
        None => (false, unknown()),
    };

    // The pool is the account check's concern.
    let config = AgentConfig {
        harness: harness_id(&p.harness, host),
        model: p.model.clone(),
        effort: p.effort.clone(),
        pool: None,
    };
    let validation = host.harnesses.validate(&config, AgentRole::Worker);
    let messages = |issues: &[quark_systems::ConfigIssue]| {
        issues
            .iter()
            .map(|i| i.message.clone())
            .collect::<Vec<_>>()
            .join("; ")
    };
    let accepted = if validation.valid {
        let mut what = match &p.model {
            Some(m) => format!("model {m}"),
            None => "the harness's default model".to_string(),
        };
        if let Some(e) = &p.effort {
            what = format!("{what}, {e} effort");
        }
        if !validation.warnings.is_empty() {
            what = format!("{what} ({})", messages(&validation.warnings));
        }
        (true, what)
    } else {
        (false, messages(&validation.errors))
    };

    let runs = match (harness, info) {
        (Some(h), Some(info)) => {
            let mine = || host.accounts.iter().filter(|a| a.harness == h.id());
            match (&p.pool, h.account_env()) {
                (Some(_), None) => {
                    Runs::Nowhere(format!("{} supports only its default account", h.name()))
                }
                (None, None) => Runs::Harness(&info.auth, h.name()),
                (None, Some(_)) => match mine().find(|a| a.default) {
                    Some(a) => Runs::Accounts(vec![a]),
                    None => Runs::Harness(&info.auth, h.name()),
                },
                (Some(pool), Some(_)) => {
                    let members: Vec<_> = mine().filter(|a| a.pools.contains(pool)).collect();
                    if members.is_empty() {
                        Runs::Nowhere(format!("pool `{pool}` has no {} accounts", h.name()))
                    } else {
                        Runs::Accounts(members)
                    }
                }
            }
        }
        _ => Runs::Nowhere(unknown()),
    };
    let usable = |a: &Account| a.launchable && a.health.state != AuthState::NotConfigured;
    let health = match &runs {
        Runs::Nowhere(why) => (false, why.clone()),
        Runs::Harness(auth, name) => (
            auth.state != AuthState::NotConfigured,
            format!("{name}: {}", auth_words(auth)),
        ),
        Runs::Accounts(accounts) => (
            accounts.iter().any(|a| usable(a)),
            accounts
                .iter()
                .map(|a| {
                    let launch = if a.launchable {
                        ""
                    } else {
                        "; the engine cannot start a worker under it yet"
                    };
                    format!("{}: {}{launch}", a.label, auth_words(&a.health))
                })
                .collect::<Vec<_>>()
                .join("; "),
        ),
    };
    let own_quota = match &runs {
        Runs::Nowhere(_) => (true, "no account to read quota for".to_string()),
        Runs::Harness(_, name) => (true, format!("quota is not read per account for {name}")),
        Runs::Accounts(accounts) => {
            // Quota left on an account no worker can start under is no headroom.
            let ready: Vec<_> = accounts.iter().filter(|a| usable(a)).collect();
            let judged = if ready.is_empty() {
                accounts.iter().collect()
            } else {
                ready
            };
            (
                judged.iter().any(|a| !a.quota.exhausted()),
                accounts
                    .iter()
                    .map(|a| format!("{}: {}", a.label, quota_words(a)))
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        }
    };
    let quota = match engine {
        None => own_quota,
        Some(e) => {
            let evidence = e
                .evidence
                .as_deref()
                .map(|v| format!(" ({v})"))
                .unwrap_or_default();
            (
                e.passed && own_quota.0,
                format!("resolution: {}{evidence}; {}", e.reason, own_quota.1),
            )
        }
    };

    vec![
        check(
            DispatchCheckKind::HarnessInstalled,
            installed.0,
            installed.1,
        ),
        check(DispatchCheckKind::ModelAccepted, accepted.0, accepted.1),
        check(DispatchCheckKind::AccountHealth, health.0, health.1),
        check(DispatchCheckKind::QuotaHeadroom, quota.0, quota.1),
    ]
}

fn auth_words(auth: &HarnessAuth) -> String {
    let state = match auth.state {
        AuthState::Configured => "logged in",
        AuthState::NotConfigured => "not logged in",
        AuthState::Unknown => "login unknown",
    };
    format!("{state} ({})", auth.detail)
}

fn quota_words(a: &Account) -> String {
    let state = match a.quota.state {
        QuotaState::Known => {
            return match a.quota.remaining_percent {
                Some(p) => format!("{p:.0}% remaining"),
                None => "quota read without a figure".into(),
            }
        }
        QuotaState::Pending => "quota not read yet",
        QuotaState::Unavailable => "quota unavailable",
        QuotaState::Error => "quota read failed",
        QuotaState::Unsupported => "quota not read for this harness",
    };
    match &a.quota.detail {
        Some(d) => format!("{state} ({d})"),
        None => state.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::EngineResolution;
    use crate::harness::{builtin, HostEnv};
    use quark_systems::{AccountQuota, HarnessInstall};

    const YAML: &str = r#"
classifier:
  provider: "none"
rules:
  - name: "trivial-edit"
    when: "A trivial mechanical edit such as a rename, typo or one-line fix, with nothing else to decide."
    use:
      - { harness: "claude-code", model: "claude-sonnet-5", effort: "low", pool: "max" }
      - { harness: "codex", model: "gpt-5.5", effort: "low" }
  - when: "Fresh news."
    use: { harness: "grok", effort: "ultra" }
default:
  - { harness: "claude-code", model: "claude-sonnet-5", effort: "medium" }
"#;

    fn account(id: &str, harness: &str, pools: &[&str], auth: AuthState, left: f64) -> Account {
        Account {
            id: id.into(),
            harness: harness.into(),
            label: id.into(),
            config_dir: None,
            default: id.starts_with("default-"),
            pools: pools.iter().map(|p| p.to_string()).collect(),
            health: HarnessAuth {
                state: auth,
                detail: "credentials file".into(),
            },
            quota: AccountQuota {
                remaining_percent: Some(left),
                ..AccountQuota::empty(QuotaState::Known, None)
            },
            active_tasks: 0,
            launchable: true,
            created_at: None,
        }
    }

    /// Every built-in harness, with only `installed` on the machine.
    async fn infos(registry: &HarnessRegistry, installed: &[&str]) -> Vec<HarnessInfo> {
        let mut infos = registry.list(true).await;
        for i in &mut infos {
            let on = installed.contains(&i.id.as_str());
            i.install = HarnessInstall {
                installed: on,
                version: on.then(|| "1.2.3".to_string()),
                path: on.then(|| format!("/bin/{}", i.id)),
                install_hint: format!("install {}", i.id),
            };
        }
        infos
    }

    fn registry() -> HarnessRegistry {
        HarnessRegistry::new(builtin(), HostEnv::default())
    }

    fn off() -> EngineResolution {
        EngineResolution {
            status: DispatchStatus::Off,
            rule: None,
            reason: None,
            notes: vec![],
            candidates: vec![],
            profile: None,
            fallback: None,
            classifier_consulted: false,
            classifier_model: None,
            confidence: None,
            output: None,
        }
    }

    fn check(c: &DispatchTestCandidate, kind: DispatchCheckKind) -> &DispatchCheck {
        c.checks.iter().find(|x| x.check == kind).unwrap()
    }

    #[tokio::test]
    async fn without_a_classifier_every_listed_profile_is_a_candidate() {
        let registry = registry();
        let infos = infos(&registry, &["claude-code", "grok"]).await;
        let accounts = [
            account(
                "default-claude-code",
                "claude-code",
                &[],
                AuthState::Configured,
                64.0,
            ),
            account(
                "acc_1",
                "claude-code",
                &["max"],
                AuthState::NotConfigured,
                90.0,
            ),
            account("acc_2", "claude-code", &["max"], AuthState::Configured, 0.0),
        ];
        let host = Host {
            harnesses: &registry,
            infos: &infos,
            accounts: &accounts,
        };
        let config = crate::crew_dispatch::compile(YAML, None).unwrap();
        let t = build("prj_1", &config, Resolved::Ran(Box::new(off())), &host);

        assert_eq!(t.decided_by, DispatchDecider::Coordinator);
        assert_eq!(t.classifier.provider, "none");
        assert_eq!(t.resolution.status, DispatchStatus::Off);
        assert_eq!((t.rule.clone(), t.chosen.clone()), (None, None));
        assert_eq!(
            t.summary,
            "No classifier is configured (provider: none), so the coordinator would pick. \
             1 of 4 candidates could start a worker now."
        );
        let ids: Vec<_> = t
            .candidates
            .iter()
            .map(|c| (c.rule.id.as_str(), c.harness.as_str()))
            .collect();
        assert_eq!(
            ids,
            [
                ("rule_1", "claude-code"),
                ("rule_1", "codex"),
                ("rule_2", "grok"),
                ("default", "claude-code")
            ]
        );

        // The pool's only logged-in account is out of quota.
        let pooled = &t.candidates[0];
        assert_eq!(pooled.rule_name.as_deref(), Some("trivial-edit"));
        assert_eq!(pooled.pool.as_deref(), Some("max"));
        assert!(!pooled.passed);
        assert!(check(pooled, DispatchCheckKind::HarnessInstalled).passed);
        assert!(check(pooled, DispatchCheckKind::ModelAccepted).passed);
        assert!(check(pooled, DispatchCheckKind::AccountHealth).passed);
        let quota = check(pooled, DispatchCheckKind::QuotaHeadroom);
        assert!(!quota.passed);
        assert_eq!(quota.detail, "acc_1: 90% remaining; acc_2: 0% remaining");
        assert_eq!(pooled.reason, quota.detail);

        // Codex is not on this machine.
        let codex = &t.candidates[1];
        assert!(!codex.passed);
        assert_eq!(codex.reason, "Codex is not installed: install codex");

        // Grok takes no `ultra` effort.
        let grok = &t.candidates[2];
        assert!(check(grok, DispatchCheckKind::HarnessInstalled).passed);
        assert!(!check(grok, DispatchCheckKind::ModelAccepted).passed);
        assert!(grok.reason.contains("ultra"), "{}", grok.reason);

        // The default profile runs under the default account.
        let default = &t.candidates[3];
        assert!(default.passed, "{default:?}");
        assert_eq!(default.reason, "eligible");
        assert_eq!(
            default.evidence.as_deref(),
            Some("default-claude-code: 64% remaining")
        );
        assert_eq!(
            check(default, DispatchCheckKind::HarnessInstalled).detail,
            "Claude Code 1.2.3 /bin/claude-code"
        );
    }

    #[tokio::test]
    async fn a_matched_rule_lists_the_candidates_the_resolution_weighed() {
        let registry = registry();
        let infos = infos(&registry, &["claude-code", "codex"]).await;
        let accounts = [
            account(
                "acc_1",
                "claude-code",
                &["max"],
                AuthState::Configured,
                79.0,
            ),
            account("default-codex", "codex", &[], AuthState::Configured, 40.0),
        ];
        let host = Host {
            harnesses: &registry,
            infos: &infos,
            accounts: &accounts,
        };
        let config = crate::crew_dispatch::compile(YAML, None).unwrap();
        let clear = EngineResolution {
            status: DispatchStatus::Clear,
            rule: Some(DispatchRule {
                id: "rule_1".into(),
                when: Some("A trivial mechanical edit such as a rename, typo or one-li".into()),
            }),
            notes: vec!["rule matched".into()],
            candidates: vec![
                DispatchCandidate {
                    harness: "claude".into(),
                    model: Some("claude-sonnet-5".into()),
                    passed: true,
                    reason: "eligible".into(),
                    evidence: Some("provider=claude remaining=79%".into()),
                },
                DispatchCandidate {
                    harness: "codex".into(),
                    model: Some("gpt-5.5".into()),
                    passed: false,
                    reason: "0% remaining at all_models".into(),
                    evidence: Some("provider=codex remaining=0%".into()),
                },
            ],
            profile: Some(DispatchChoice {
                harness: "claude".into(),
                model: Some("claude-sonnet-5".into()),
                effort: Some("low".into()),
                account: None,
            }),
            fallback: None,
            classifier_consulted: true,
            classifier_model: Some("jev-1.13.0".into()),
            confidence: Some(0.91),
            output: Some("dispatch-resolve:\n  status: clear".into()),
            ..off()
        };
        let t = build(
            "prj_1",
            &config,
            Resolved::Ran(Box::new(clear.clone())),
            &host,
        );

        assert_eq!(t.decided_by, DispatchDecider::Classifier);
        assert_eq!(t.classifier.provider, dispatch::SYSTEM1);
        let rule = t.rule.as_ref().unwrap();
        assert_eq!(rule.id, "rule_1");
        assert!(rule
            .when
            .as_deref()
            .unwrap()
            .ends_with("nothing else to decide."));
        let chosen = t.chosen.as_ref().unwrap();
        assert_eq!(chosen.harness, "claude-code");
        assert!(t
            .summary
            .starts_with("The classifier matched rule rule_1 ("));
        assert!(t.summary.ends_with(
            "at 0.91 confidence. The resolution would select claude-code:claude-sonnet-5 \
             (low effort). 1 of 2 candidates could start a worker now."
        ));

        assert_eq!(t.candidates.len(), 2);
        let claude = &t.candidates[0];
        assert_eq!(
            (claude.harness.as_str(), claude.pool.as_deref()),
            ("claude-code", Some("max"))
        );
        assert_eq!(claude.effort.as_deref(), Some("low"));
        assert!(claude.passed);
        assert_eq!(
            claude.evidence.as_deref(),
            Some("provider=claude remaining=79%")
        );
        // The engine's quota verdict fails a candidate Quark's own reading passes.
        let codex = &t.candidates[1];
        assert!(!codex.passed);
        let quota = check(codex, DispatchCheckKind::QuotaHeadroom);
        assert_eq!(
            quota.detail,
            "resolution: 0% remaining at all_models (provider=codex remaining=0%); \
             default-codex: 40% remaining"
        );
        assert_eq!(codex.reason, quota.detail);

        // A rule whose floor fell through weighs the default profiles.
        let mut fell = clear;
        fell.candidates.truncate(1);
        fell.candidates[0].model = Some("claude-sonnet-5".into());
        fell.rule.as_mut().unwrap().id = "rule_2".into();
        let t = build("prj_1", &config, Resolved::Ran(Box::new(fell)), &host);
        assert_eq!(t.candidates[0].rule.id, "default");
        assert_eq!(t.candidates[0].effort.as_deref(), Some("medium"));
    }

    #[tokio::test]
    async fn an_unclear_or_failed_resolution_leaves_the_pick_to_the_coordinator() {
        let registry = registry();
        let infos = infos(&registry, &[]).await;
        let host = Host {
            harnesses: &registry,
            infos: &infos,
            accounts: &[],
        };
        let config = crate::crew_dispatch::compile(
            "rules: []\ndefault: { harness: \"nope\", pool: \"max\" }\n",
            None,
        )
        .unwrap();
        let escalate = EngineResolution {
            status: DispatchStatus::Escalate,
            reason: Some("no rules to match".into()),
            ..off()
        };
        let t = build("p", &config, Resolved::Ran(Box::new(escalate)), &host);
        assert_eq!(t.decided_by, DispatchDecider::Coordinator);
        assert_eq!(
            t.summary,
            "The dispatch resolution was escalated: no rules to match, so the coordinator \
             would pick. 0 of 1 candidate could start a worker now."
        );
        let c = &t.candidates[0];
        assert_eq!(c.reason, "unknown harness `nope`");
        assert!(c.checks.iter().take(3).all(|x| !x.passed));

        let t = build("p", &config, Resolved::Failed("timed out".into()), &host);
        assert_eq!(t.resolution.status, DispatchStatus::Error);
        assert!(t
            .summary
            .starts_with("The dispatch resolution failed (timed out)"));
        let t = build("p", &config, Resolved::NotRun("No workspace".into()), &host);
        assert_eq!(t.resolution.status, DispatchStatus::NotConsulted);
        assert!(t
            .summary
            .starts_with("No workspace; the coordinator would pick."));
    }
}
