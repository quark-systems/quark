//! Disabling or removing an account: what it touches across every Project,
//! and moving the dispatch profiles it leaves without an account.
//!
//! An account is instance wide, so disabling one reaches every Project's
//! dispatch rules (`dispatch.yaml`) and every coordinator's own agent. A
//! profile reaches the account through a pool the account is in, or, naming
//! no pool, as its harness's default account. [`plan`] lists each such
//! profile with the enabled accounts it keeps; one left with none is
//! *stranded*, and Quark proposes what to do with it: another pool of the
//! harness, the harness's default account, taking the candidate out of its
//! rule, or leaving it for the coordinator to place. Nothing changes until
//! the person confirms a choice for every stranded profile ([`check`]);
//! [`apply_rules`] and [`apply_agent`] then make those changes.
//!
//! Work already running under the account is listed and left to finish.

use std::collections::BTreeMap;

use quark_systems::{
    Account, AccountChangeAction, AccountChangePlan, AccountDisposition, AccountPlace,
    AccountReference, AccountWork, AgentConfig, DispatchProfile, DispatchRulesDraft,
};

/// One Project as the plan sees it.
#[derive(Debug, Clone, Default)]
pub struct ProjectProfiles {
    pub id: String,
    pub name: String,
    /// Its dispatch rules, when it has a Project repo with readable ones.
    pub rules: Option<DispatchRulesDraft>,
    /// Its coordinator's own agent, when one is configured.
    pub coordinator: Option<AgentConfig>,
}

/// The id a reference has in a plan and its confirmation.
fn rule_ref(project: &str, rule: usize, candidate: usize) -> String {
    format!("{project}/rules/{rule}/{candidate}")
}

fn default_ref(project: &str, candidate: usize) -> String {
    format!("{project}/default/{candidate}")
}

fn coordinator_ref(project: &str) -> String {
    format!("{project}/coordinator")
}

/// What disabling or removing `target` touches. `accounts` are every
/// account (`target` among them); `resolve` turns a profile's harness name
/// into Quark's harness id.
pub fn plan(
    target: &Account,
    action: AccountChangeAction,
    accounts: &[Account],
    projects: &[ProjectProfiles],
    running: Vec<AccountWork>,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> AccountChangePlan {
    let refused = match action {
        AccountChangeAction::Remove if target.default => {
            Some("a harness's default account cannot be removed; disable it instead".to_string())
        }
        _ => None,
    };
    let mut references = Vec::new();
    for p in projects {
        let mut add = |id: String,
                       place: AccountPlace,
                       rule: Option<String>,
                       harness: &str,
                       model: &Option<String>,
                       effort: &Option<String>,
                       pool: &Option<String>,
                       siblings: usize| {
            if resolve(harness).as_deref() != Some(target.harness.as_str()) {
                return;
            }
            let reaches = match pool {
                Some(pool) => target.pools.contains(pool),
                None => target.default,
            };
            if !reaches {
                return;
            }
            let remaining = keeps(target, accounts, pool.as_deref());
            let stranded = remaining.is_empty();
            let options = if stranded {
                options(target, accounts, pool.as_deref(), place, siblings)
            } else {
                Vec::new()
            };
            references.push(AccountReference {
                id,
                project_id: p.id.clone(),
                project_name: p.name.clone(),
                place,
                rule,
                harness: harness.to_string(),
                model: model.clone(),
                effort: effort.clone(),
                pool: pool.clone(),
                remaining,
                stranded,
                options,
            });
        };
        if let Some(rules) = &p.rules {
            for (i, r) in rules.rules.iter().enumerate() {
                let name = r.name.clone().unwrap_or_else(|| format!("rule {}", i + 1));
                for (j, c) in r.candidates.iter().enumerate() {
                    add(
                        rule_ref(&p.id, i, j),
                        AccountPlace::Rule,
                        Some(name.clone()),
                        &c.harness,
                        &c.model,
                        &c.effort,
                        &c.pool,
                        r.candidates.len(),
                    );
                }
            }
            for (j, c) in rules.default.iter().enumerate() {
                add(
                    default_ref(&p.id, j),
                    AccountPlace::Default,
                    None,
                    &c.harness,
                    &c.model,
                    &c.effort,
                    &c.pool,
                    rules.default.len(),
                );
            }
        }
        if let Some(a) = &p.coordinator {
            add(
                coordinator_ref(&p.id),
                AccountPlace::Coordinator,
                None,
                &a.harness,
                &a.model,
                &a.effort,
                &a.pool,
                1,
            );
        }
    }
    AccountChangePlan {
        account_id: target.id.clone(),
        action,
        running,
        references,
        refused,
    }
}

/// The enabled accounts, other than `target`, a profile of `target`'s
/// harness naming `pool` still runs under.
fn keeps(target: &Account, accounts: &[Account], pool: Option<&str>) -> Vec<String> {
    accounts
        .iter()
        .filter(|a| a.harness == target.harness && a.id != target.id && a.enabled)
        .filter(|a| match pool {
            Some(p) => a.pools.iter().any(|x| x == p),
            None => a.default,
        })
        .map(|a| a.label.clone())
        .collect()
}

/// What a stranded profile can do instead, the proposal first: the pool of
/// the harness with the most enabled accounts, then the default account,
/// then dropping the candidate, then leaving it.
fn options(
    target: &Account,
    accounts: &[Account],
    pool: Option<&str>,
    place: AccountPlace,
    siblings: usize,
) -> Vec<AccountDisposition> {
    let mut pools: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for a in accounts
        .iter()
        .filter(|a| a.harness == target.harness && a.id != target.id && a.enabled)
    {
        for p in &a.pools {
            if Some(p.as_str()) != pool {
                pools.entry(p).or_default().push(a.label.clone());
            }
        }
    }
    let mut pools: Vec<_> = pools.into_iter().collect();
    pools.sort_by(|(a, x), (b, y)| y.len().cmp(&x.len()).then(a.cmp(b)));
    let mut out: Vec<AccountDisposition> = pools
        .into_iter()
        .map(|(pool, accounts)| AccountDisposition::UsePool {
            pool: pool.to_string(),
            accounts,
        })
        .collect();
    if pool.is_some() && !keeps(target, accounts, None).is_empty() {
        out.push(AccountDisposition::UseDefault);
    }
    if place != AccountPlace::Coordinator && siblings > 1 {
        out.push(AccountDisposition::Drop);
    }
    out.push(AccountDisposition::Keep);
    out
}

/// Whether `choices` settle the plan: one of its options for every stranded
/// reference, and nothing for references the plan does not have.
pub fn check(
    plan: &AccountChangePlan,
    choices: &BTreeMap<String, AccountDisposition>,
) -> Result<(), String> {
    if let Some(why) = &plan.refused {
        return Err(why.clone());
    }
    for id in choices.keys() {
        let Some(r) = plan.references.iter().find(|r| &r.id == id) else {
            return Err(format!(
                "`{id}` is not a profile this change touches any more; load the plan again"
            ));
        };
        if !r.stranded && !matches!(choices[id], AccountDisposition::Keep) {
            return Err(format!(
                "`{id}` keeps an enabled account and needs no change"
            ));
        }
    }
    let open: Vec<&str> = plan
        .references
        .iter()
        .filter(|r| r.stranded)
        .filter(|r| match choices.get(&r.id) {
            Some(c) => !r.options.iter().any(|o| o.same(c)),
            None => true,
        })
        .map(|r| r.id.as_str())
        .collect();
    if open.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "choose what happens to each profile left without an enabled account: {}",
            open.join(", ")
        ))
    }
}

/// Makes the chosen changes to one Project's dispatch rules. True when
/// anything changed.
pub fn apply_rules(
    project: &str,
    rules: &mut DispatchRulesDraft,
    choices: &BTreeMap<String, AccountDisposition>,
) -> Result<bool, String> {
    let mut changed = false;
    let mut edit = |list: &mut Vec<DispatchProfile>,
                    id: &dyn Fn(usize) -> String,
                    what: &str|
     -> Result<(), String> {
        let mut drop = Vec::new();
        for (j, c) in list.iter_mut().enumerate() {
            match choices.get(&id(j)) {
                None | Some(AccountDisposition::Keep) => {}
                Some(AccountDisposition::UsePool { pool, .. }) => {
                    c.pool = Some(pool.clone());
                    changed = true;
                }
                Some(AccountDisposition::UseDefault) => {
                    c.pool = None;
                    changed = true;
                }
                Some(AccountDisposition::Drop) => drop.push(j),
            }
        }
        if !drop.is_empty() && drop.len() == list.len() {
            return Err(format!(
                "{what} would be left with no candidates; keep or move one of them"
            ));
        }
        for j in drop.into_iter().rev() {
            list.remove(j);
            changed = true;
        }
        Ok(())
    };
    for (i, r) in rules.rules.iter_mut().enumerate() {
        let what = match &r.name {
            Some(n) => format!("rule `{n}`"),
            None => format!("rule {}", i + 1),
        };
        edit(&mut r.candidates, &|j| rule_ref(project, i, j), &what)?;
    }
    edit(
        &mut rules.default,
        &|j| default_ref(project, j),
        "the default",
    )?;
    Ok(changed)
}

/// The coordinator's agent after the chosen change, when it changes.
pub fn apply_agent(
    project: &str,
    agent: &AgentConfig,
    choices: &BTreeMap<String, AccountDisposition>,
) -> Option<AgentConfig> {
    let pool = match choices.get(&coordinator_ref(project))? {
        AccountDisposition::UsePool { pool, .. } => Some(pool.clone()),
        AccountDisposition::UseDefault => None,
        AccountDisposition::Drop | AccountDisposition::Keep => return None,
    };
    Some(AgentConfig {
        pool,
        ..agent.clone()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use quark_systems::{
        AccountAvailability, AccountQuota, AuthState, DispatchRuleSpec, HarnessAuth, QuotaState,
    };

    fn account(id: &str, harness: &str, pools: &[&str], enabled: bool) -> Account {
        Account {
            id: id.into(),
            harness: harness.into(),
            label: id.into(),
            config_dir: None,
            api_key: false,
            team_id: None,
            default: id.starts_with("default-"),
            pools: pools.iter().map(|p| p.to_string()).collect(),
            health: HarnessAuth {
                state: AuthState::Configured,
                detail: String::new(),
            },
            quota: AccountQuota::empty(QuotaState::Known, None),
            active_tasks: 0,
            launchable: true,
            created_at: None,
            enabled,
            availability: AccountAvailability::Enabled,
            removing: false,
        }
    }

    fn profile(harness: &str, pool: Option<&str>) -> DispatchProfile {
        DispatchProfile {
            harness: harness.into(),
            model: None,
            effort: None,
            pool: pool.map(str::to_string),
            provider: None,
            floor: None,
            pricing: None,
        }
    }

    fn project() -> ProjectProfiles {
        ProjectProfiles {
            id: "prj_1".into(),
            name: "Selah".into(),
            rules: Some(DispatchRulesDraft {
                default_select: None,
                rules: vec![DispatchRuleSpec {
                    name: Some("hard".into()),
                    when: "hard work".into(),
                    candidates: vec![profile("claude-code", Some("max")), profile("codex", None)],
                    ..DispatchRuleSpec::default()
                }],
                default: vec![
                    profile("claude-code", Some("team")),
                    profile("claude-code", None),
                ],
            }),
            coordinator: Some(AgentConfig {
                harness: "claude".into(),
                model: Some("opus".into()),
                effort: None,
                pool: Some("max".into()),
            }),
        }
    }

    fn resolve(name: &str) -> Option<String> {
        Some(
            if name == "claude" {
                "claude-code"
            } else {
                name
            }
            .to_string(),
        )
    }

    fn accounts() -> Vec<Account> {
        vec![
            account("default-claude-code", "claude-code", &[], true),
            account("acc_max", "claude-code", &["max"], true),
            account("acc_team", "claude-code", &["team"], true),
            account("acc_team2", "claude-code", &["team", "batch"], true),
            account("acc_off", "claude-code", &["batch"], false),
            account("default-codex", "codex", &[], true),
        ]
    }

    #[test]
    fn a_pool_losing_its_only_account_is_stranded_with_proposals() {
        let all = accounts();
        let p = plan(
            &all[1],
            AccountChangeAction::Disable,
            &all,
            &[project()],
            Vec::new(),
            &resolve,
        );
        assert!(p.refused.is_none());
        let ids: Vec<_> = p.references.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["prj_1/rules/0/0", "prj_1/coordinator"]);
        let rule = &p.references[0];
        assert!(rule.stranded);
        assert_eq!(rule.rule.as_deref(), Some("hard"));
        // The pool with the most enabled accounts first; the disabled one
        // does not count.
        assert_eq!(
            rule.options,
            [
                AccountDisposition::UsePool {
                    pool: "team".into(),
                    accounts: vec!["acc_team".into(), "acc_team2".into()]
                },
                AccountDisposition::UsePool {
                    pool: "batch".into(),
                    accounts: vec!["acc_team2".into()]
                },
                AccountDisposition::UseDefault,
                AccountDisposition::Drop,
                AccountDisposition::Keep,
            ]
        );
        // A coordinator cannot drop its agent.
        let c = &p.references[1];
        assert!(!c.options.contains(&AccountDisposition::Drop));
    }

    #[test]
    fn a_pool_with_other_accounts_keeps_running() {
        let all = accounts();
        let p = plan(
            &all[2],
            AccountChangeAction::Remove,
            &all,
            &[project()],
            Vec::new(),
            &resolve,
        );
        assert_eq!(p.references.len(), 1);
        let r = &p.references[0];
        assert_eq!(r.id, "prj_1/default/0");
        assert!(!r.stranded);
        assert_eq!(r.remaining, ["acc_team2"]);
        assert!(r.options.is_empty());
        assert!(check(&p, &BTreeMap::new()).is_ok());
    }

    #[test]
    fn the_default_account_reaches_profiles_without_a_pool() {
        let all = accounts();
        let p = plan(
            &all[0],
            AccountChangeAction::Disable,
            &all,
            &[project()],
            Vec::new(),
            &resolve,
        );
        assert_eq!(p.references.len(), 1);
        assert_eq!(p.references[0].id, "prj_1/default/1");
        assert!(p.references[0].stranded);
        assert!(!p.references[0]
            .options
            .contains(&AccountDisposition::UseDefault));
        let removing = plan(
            &all[0],
            AccountChangeAction::Remove,
            &all,
            &[project()],
            Vec::new(),
            &resolve,
        );
        assert!(removing.refused.is_some());
    }

    #[test]
    fn every_stranded_profile_needs_an_offered_choice() {
        let all = accounts();
        let p = plan(
            &all[1],
            AccountChangeAction::Disable,
            &all,
            &[project()],
            Vec::new(),
            &resolve,
        );
        let mut choices = BTreeMap::from([(
            "prj_1/rules/0/0".to_string(),
            AccountDisposition::UsePool {
                pool: "team".into(),
                accounts: Vec::new(),
            },
        )]);
        let err = check(&p, &choices).unwrap_err();
        assert!(err.contains("prj_1/coordinator"), "{err}");
        choices.insert(
            "prj_1/coordinator".into(),
            AccountDisposition::UsePool {
                pool: "nowhere".into(),
                accounts: Vec::new(),
            },
        );
        assert!(check(&p, &choices).is_err());
        choices.insert("prj_1/coordinator".into(), AccountDisposition::UseDefault);
        check(&p, &choices).unwrap();
        choices.insert("prj_9/default/0".into(), AccountDisposition::Keep);
        assert!(check(&p, &choices).is_err());
    }

    #[test]
    fn choices_change_the_rules_and_the_coordinator() {
        let p = project();
        let mut rules = p.rules.clone().unwrap();
        let choices = BTreeMap::from([
            ("prj_1/rules/0/0".to_string(), AccountDisposition::Drop),
            (
                "prj_1/default/1".to_string(),
                AccountDisposition::UsePool {
                    pool: "team".into(),
                    accounts: Vec::new(),
                },
            ),
            (
                "prj_1/coordinator".to_string(),
                AccountDisposition::UseDefault,
            ),
        ]);
        assert!(apply_rules("prj_1", &mut rules, &choices).unwrap());
        assert_eq!(rules.rules[0].candidates, [profile("codex", None)]);
        assert_eq!(rules.default[1].pool.as_deref(), Some("team"));
        let agent = apply_agent("prj_1", p.coordinator.as_ref().unwrap(), &choices).unwrap();
        assert_eq!(agent.pool, None);
        assert_eq!(agent.model.as_deref(), Some("opus"));

        // Dropping every candidate of a rule is refused.
        let mut rules = p.rules.clone().unwrap();
        let both = BTreeMap::from([
            ("prj_1/rules/0/0".to_string(), AccountDisposition::Drop),
            ("prj_1/rules/0/1".to_string(), AccountDisposition::Drop),
        ]);
        assert!(apply_rules("prj_1", &mut rules, &both).is_err());
        // Keeping changes nothing.
        let keep = BTreeMap::from([("prj_1/rules/0/0".to_string(), AccountDisposition::Keep)]);
        let mut rules = p.rules.unwrap();
        assert!(!apply_rules("prj_1", &mut rules, &keep).unwrap());
    }
}
