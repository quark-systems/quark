//! The default dispatch rules: what every Project whose `dispatch.yaml` lists
//! no rules of its own follows.
//!
//! They are kept in `~/.quark/dispatch.yaml`, in the layout of a Project's
//! file without its `classifier` block (the user-level classifier lives in
//! `~/.quark/config.yaml`). Until that file is saved, Quark ships four tiers
//! and a default: Trivial, Standard, Complex, Scout, and the default for a
//! task none of them matches. The tiers' names and conditions are fixed; their
//! candidates are picked from the harnesses installed here that have an
//! enabled, signed-in account.

use std::path::{Path, PathBuf};

use quark_systems::{
    Account, AuthState, DispatchFloor, DispatchProfile, DispatchRuleSpec, DispatchRulesDraft,
    DispatchSelect, HarnessInfo,
};

use crate::crew_dispatch;

/// The file under Quark's home that holds the saved defaults.
pub const FILE: &str = "dispatch.yaml";

/// The default rules in effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Defaults {
    /// As `~/.quark/dispatch.yaml` holds them, or the shipped tiers rendered
    /// the same way.
    pub yaml: String,
    /// True while nothing is saved and the shipped tiers apply.
    pub shipped: bool,
}

impl Defaults {
    /// Names this version of the defaults: a hash of their text.
    pub fn revision(&self) -> String {
        // FNV-1a: stable across builds, which the std hasher is not.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in self.yaml.as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        format!("{h:016x}")
    }

    /// The editable rules.
    pub fn draft(&self) -> Result<DispatchRulesDraft, String> {
        crew_dispatch::as_written(&self.yaml)
            .map(|w| crew_dispatch::draft(&w))
            .map_err(|e| format!("default rules: {e}"))
    }
}

/// `~/.quark/dispatch.yaml` under `home`.
pub fn path(home: &Path) -> PathBuf {
    home.join(FILE)
}

/// The saved defaults' text, `None` when nothing is saved.
pub fn read_saved(home: &Path) -> Result<Option<String>, String> {
    let file = path(home);
    match std::fs::read_to_string(&file) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", file.display())),
    }
}

/// The defaults in effect: the saved ones, else the shipped tiers seeded from
/// `harnesses` and `accounts`.
pub fn load(
    home: &Path,
    harnesses: &[HarnessInfo],
    accounts: &[Account],
) -> Result<Defaults, String> {
    if let Some(yaml) = read_saved(home)? {
        return Ok(Defaults {
            yaml,
            shipped: false,
        });
    }
    Ok(Defaults {
        yaml: render(&shipped(harnesses, accounts))?,
        shipped: true,
    })
}

/// `draft` as `~/.quark/dispatch.yaml` holds it.
pub fn render(draft: &DispatchRulesDraft) -> Result<String, String> {
    let body = crew_dispatch::render(draft, Some(HEADER))?;
    Ok(body)
}

const HEADER: &str = "\
# Default dispatch rules: every Project that lists no rules of its own in its
# dispatch.yaml follows these. Edit them in the app under Dispatch defaults.
";

/// Save `draft` as the defaults; it is checked as a Project's rules are.
pub fn save(home: &Path, draft: &DispatchRulesDraft) -> Result<Defaults, String> {
    let yaml = render(draft)?;
    crew_dispatch::compile(&yaml, None)?;
    let file = path(home);
    let tmp = file.with_extension("yaml.tmp");
    std::fs::write(&tmp, &yaml)
        .and_then(|()| std::fs::rename(&tmp, &file))
        .map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(Defaults {
        yaml,
        shipped: false,
    })
}

/// Forget the saved defaults, so the shipped tiers apply again.
pub fn forget(home: &Path) -> Result<(), String> {
    let file = path(home);
    match std::fs::remove_file(&file) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{}: {e}", file.display())),
    }
}

/// Where the daemon reads the defaults from: its home, and the harnesses and
/// accounts the shipped tiers are seeded from.
#[derive(Clone)]
pub struct Source {
    pub home: PathBuf,
    pub harnesses: std::sync::Arc<crate::harness::HarnessRegistry>,
    pub accounts: std::sync::Arc<crate::accounts::Accounts>,
}

impl Source {
    /// The defaults in effect now.
    pub async fn load(&self) -> Result<Defaults, String> {
        let infos = self.harnesses.list(false).await;
        let accounts = self.accounts.list().await.map_err(|e| e.to_string())?;
        let home = self.home.clone();
        tokio::task::spawn_blocking(move || load(&home, &infos, &accounts))
            .await
            .map_err(|e| e.to_string())?
    }
}

/// One shipped tier.
pub struct Tier {
    pub name: &'static str,
    pub when: &'static str,
    pub why: &'static str,
}

pub const TRIVIAL: Tier = Tier {
    name: "Trivial",
    when: "The task is a trivial mechanical edit: a rote rename, formatting sweep, targeted typo fix, simple config change, or single-line bug fix with an obvious cause.",
    why: "Cheap and fast for narrow work.",
};
pub const STANDARD: Tier = Tier {
    name: "Standard",
    when: "The task is a standard single-concern feature, a well-scoped bug fix with a known root cause, adding tests, or a focused refactor within one module.",
    why: "The workhorse tier: a capable model at moderate cost.",
};
pub const COMPLEX: Tier = Tier {
    name: "Complex",
    when: "The task is a large multi-file feature, a risky refactor touching shared seams, a new subsystem, or work that requires holding many moving parts and making design decisions.",
    why: "Complex work benefits from the strongest model.",
};
pub const SCOUT: Tier = Tier {
    name: "Scout",
    when: "The task is a scout investigation, bug diagnosis, architecture research, or exploratory analysis that needs deep reading and reasoning but produces a report rather than code.",
    why: "Scouts need deep reading and strong reasoning, and write a report rather than code.",
};

/// The shipped tiers, in the order they are listed.
pub const TIERS: [Tier; 4] = [TRIVIAL, STANDARD, COMPLEX, SCOUT];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Trivial,
    Standard,
    Complex,
    Scout,
    /// The default: a task no tier matches.
    Fallback,
}

/// Harnesses the shipped tiers know models for, in the order they are tried
/// for cheap work and for hard work.
const CHEAP_ORDER: [&str; 4] = ["bob", "kiro", "claude-code", "codex"];
const HARD_ORDER: [&str; 3] = ["kiro", "claude-code", "codex"];

fn profile(harness: &str, level: Level) -> Option<DispatchProfile> {
    let p = |model: Option<&str>, effort: Option<&str>| DispatchProfile {
        harness: harness.into(),
        model: model.map(Into::into),
        effort: effort.map(Into::into),
        ..Default::default()
    };
    use Level::*;
    Some(match (harness, level) {
        ("claude-code", Trivial) => p(Some("haiku"), Some("high")),
        ("claude-code", Standard | Fallback) => p(Some("sonnet"), Some("medium")),
        ("claude-code", Complex) => p(Some("opus"), Some("medium")),
        ("claude-code", Scout) => p(Some("opus"), Some("high")),
        ("kiro", Trivial) => p(Some("claude-haiku-5.5"), Some("high")),
        ("kiro", Standard | Fallback) => p(Some("claude-sonnet-5.5"), Some("medium")),
        ("kiro", Complex) => p(Some("claude-opus-5.5"), Some("medium")),
        ("kiro", Scout) => p(Some("claude-opus-5.5"), Some("high")),
        ("codex", Trivial) => p(None, Some("low")),
        ("codex", Standard | Fallback) => p(None, Some("medium")),
        ("codex", Complex | Scout) => p(None, Some("high")),
        // Bob picks its own model; it covers cheap work while its quota
        // stays above the floor.
        ("bob", Trivial | Standard | Fallback) => DispatchProfile {
            provider: Some("bob".into()),
            floor: Some(DispatchFloor {
                scope: "all_models".into(),
                min_percent: 15.into(),
                provider: None,
            }),
            ..p(None, None)
        },
        _ => return None,
    })
}

/// Whether a worker could start with `info` here: installed, and with an
/// enabled account that is not known to lack a credential.
fn available(info: &HarnessInfo, accounts: &[Account]) -> bool {
    if !info.install.installed || !info.roles.contains(&quark_systems::AgentRole::Worker) {
        return false;
    }
    let mine: Vec<_> = accounts.iter().filter(|a| a.harness == info.id).collect();
    if mine.is_empty() {
        return info.auth.state != AuthState::NotConfigured;
    }
    mine.iter()
        .any(|a| a.enabled && !a.removing && a.health.state != AuthState::NotConfigured)
}

/// The shipped tiers with candidates from the harnesses available here. With
/// none of the known harnesses available, Claude Code's profiles stand in, so
/// the rules are never empty.
pub fn shipped(harnesses: &[HarnessInfo], accounts: &[Account]) -> DispatchRulesDraft {
    let here: Vec<&str> = harnesses
        .iter()
        .filter(|h| available(h, accounts))
        .map(|h| h.id.as_str())
        .collect();
    let candidates = |level: Level| -> Vec<DispatchProfile> {
        let order: &[&str] = match level {
            Level::Complex | Level::Scout => &HARD_ORDER,
            _ => &CHEAP_ORDER,
        };
        let list: Vec<_> = order
            .iter()
            .filter(|h| here.contains(h))
            .filter_map(|h| profile(h, level))
            .collect();
        if list.is_empty() {
            profile("claude-code", level).into_iter().collect()
        } else {
            list
        }
    };
    let rule = |t: &Tier, level: Level| DispatchRuleSpec {
        name: Some(t.name.into()),
        when: t.when.into(),
        candidates: candidates(level),
        select: Some(DispatchSelect::Ordered),
        why: Some(t.why.into()),
        approval: None,
        floor: None,
    };
    DispatchRulesDraft {
        default_select: Some(DispatchSelect::Ordered),
        rules: vec![
            rule(&TRIVIAL, Level::Trivial),
            rule(&STANDARD, Level::Standard),
            rule(&COMPLEX, Level::Complex),
            rule(&SCOUT, Level::Scout),
        ],
        default: candidates(Level::Fallback),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quark_systems::{
        AccountAvailability, AccountQuota, AgentRole, HarnessAuth, HarnessInstall, HarnessModels,
        HarnessSupervision, ModelSelection, QuotaState, SupervisionConfidence,
    };

    fn info(id: &str, installed: bool) -> HarnessInfo {
        HarnessInfo {
            id: id.into(),
            name: id.into(),
            roles: vec![AgentRole::Worker],
            install: HarnessInstall {
                installed,
                version: None,
                path: None,
                install_hint: String::new(),
            },
            models: HarnessModels {
                selection: ModelSelection::FreeForm,
                discovery: None,
            },
            efforts: vec![],
            auth: HarnessAuth {
                state: AuthState::Configured,
                detail: String::new(),
            },
            supervision: HarnessSupervision {
                confidence: SupervisionConfidence::High,
                source: String::new(),
            },
            transcript: false,
            account_env: None,
            key_env: None,
        }
    }

    fn account(harness: &str, enabled: bool) -> Account {
        serde_json::from_value(serde_json::json!({
            "id": format!("default-{harness}"),
            "harness": harness,
            "label": harness,
            "config_dir": null,
            "default": true,
            "pools": [],
            "health": {"state": "configured", "detail": ""},
            "quota": serde_json::to_value(AccountQuota::empty(QuotaState::Pending, None)).unwrap(),
            "active_tasks": 0,
            "launchable": true,
            "created_at": null,
            "enabled": enabled,
            "availability": serde_json::to_value(if enabled { AccountAvailability::Enabled } else { AccountAvailability::Disabled }).unwrap(),
            "removing": false,
        }))
        .unwrap()
    }

    fn harnesses(d: &DispatchRulesDraft, rule: usize) -> Vec<String> {
        d.rules[rule]
            .candidates
            .iter()
            .map(|c| c.harness.clone())
            .collect()
    }

    #[test]
    fn ships_four_tiers_and_a_default_from_what_is_installed() {
        let infos = [
            info("claude-code", true),
            info("codex", true),
            info("kiro", false),
            info("gemini", true),
        ];
        let d = shipped(&infos, &[]);
        let names: Vec<_> = d.rules.iter().map(|r| r.name.clone().unwrap()).collect();
        assert_eq!(names, ["Trivial", "Standard", "Complex", "Scout"]);
        assert_eq!(harnesses(&d, 0), ["claude-code", "codex"]);
        assert_eq!(d.rules[0].candidates[0].model.as_deref(), Some("haiku"));
        assert_eq!(d.rules[2].candidates[0].model.as_deref(), Some("opus"));
        assert_eq!(d.default[0].model.as_deref(), Some("sonnet"));
        // It renders and compiles as a Project's rules do.
        let yaml = render(&d).unwrap();
        crew_dispatch::compile(&yaml, None).unwrap();
        let back = Defaults {
            yaml,
            shipped: true,
        }
        .draft()
        .unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn disabled_accounts_drop_their_harness() {
        let infos = [
            info("bob", true),
            info("kiro", true),
            info("claude-code", true),
        ];
        let all = shipped(&infos, &[account("bob", true), account("kiro", true)]);
        assert_eq!(harnesses(&all, 0), ["bob", "kiro", "claude-code"]);
        // Bob covers cheap work only.
        assert_eq!(harnesses(&all, 2), ["kiro", "claude-code"]);
        let d = shipped(&infos, &[account("bob", false), account("kiro", true)]);
        assert_eq!(harnesses(&d, 0), ["kiro", "claude-code"]);
    }

    #[test]
    fn nothing_available_falls_back_to_claude_code() {
        let d = shipped(&[], &[]);
        assert!(d.rules.iter().all(|r| r.candidates.len() == 1));
        assert_eq!(d.default[0].harness, "claude-code");
    }

    #[test]
    fn saved_defaults_replace_the_shipped_tiers() {
        let home = tempfile::tempdir().unwrap();
        let shipped_now = load(home.path(), &[], &[]).unwrap();
        assert!(shipped_now.shipped);
        let mut d = shipped_now.draft().unwrap();
        d.rules.truncate(1);
        let saved = save(home.path(), &d).unwrap();
        let loaded = load(home.path(), &[], &[]).unwrap();
        assert_eq!(loaded, saved);
        assert!(!loaded.shipped);
        assert_ne!(loaded.revision(), shipped_now.revision());
        forget(home.path()).unwrap();
        assert!(load(home.path(), &[], &[]).unwrap().shipped);
    }
}
