//! Triggers: typed event sources plus condition-to-action rules.
//!
//! A [`Rule`] says "when this holds, do that". Conditions come from typed
//! sources: an event in the log, a clock, or a command polled until it
//! holds (firstmate's when-then watches). Actions are deterministic and
//! reversible: wake the coordinator with a note, drop a note in the inbox,
//! steer a task, or run a command. Anything needing judgment stays with the
//! coordinator, which a rule can wake.
//!
//! Rules live in the log (`trigger.defined`, `trigger.removed`), so the
//! dashboard edits them by appending events and every host sees the same
//! set. A fire is claimed with `trigger.fired` before its action runs and
//! closed with `trigger.outcome`, so an action runs at most once even
//! across a crash; a claim with no outcome is reported as ambiguous rather
//! than retried.

use std::collections::{BTreeMap, BTreeSet};

use quark_core::{CoreError, Event, EventKind, ProjectId, Result, Seq, TaskId};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// The prefix every trigger event kind starts with.
pub const PREFIX: &str = "trigger";

/// A rule's name, unique within its Project.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RuleId(pub String);

impl RuleId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for RuleId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl std::fmt::Display for RuleId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a command's standard output must look like for its condition to
/// hold, on top of exiting 0.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", content = "value", rename_all = "snake_case")]
pub enum Expect {
    /// Trimmed output equals this.
    Equals(String),
    /// Trimmed output differs from this, such as an issue's state when the
    /// rule was made.
    Differs(String),
}

/// When a rule fires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum Condition {
    /// An event in the log. `kind` is exact, or a prefix ending in `.*`.
    /// Each `fields` entry is a JSON pointer into the payload and the value
    /// it must equal.
    Event {
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<TaskId>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        fields: BTreeMap<String, serde_json::Value>,
    },
    /// Every `secs` seconds.
    Every { secs: u64 },
    /// Once, at `at`.
    At {
        #[serde(with = "time::serde::rfc3339")]
        at: OffsetDateTime,
    },
    /// A command polled every `interval_secs` until it exits 0 (and, with
    /// `expect`, prints the right thing) on `stable` polls in a row. Exit 1
    /// is a clean "not yet"; anything else, or a timeout, is an error.
    Command {
        argv: Vec<String>,
        #[serde(default = "default_interval")]
        interval_secs: u64,
        #[serde(default = "default_stable")]
        stable: u32,
        #[serde(default = "default_timeout")]
        timeout_secs: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect: Option<Expect>,
        /// Give up, and wake the coordinator, after this many errors in a
        /// row.
        #[serde(default = "default_error_budget")]
        error_budget: u32,
    },
}

fn default_interval() -> u64 {
    60
}
fn default_stable() -> u32 {
    2
}
fn default_timeout() -> u64 {
    60
}
fn default_error_budget() -> u32 {
    3
}

/// What a rule does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Action {
    /// Wake the coordinator with a note.
    Wake { note: String },
    /// Put a note in the Project's inbox.
    Inbox { body: String },
    /// Send a steering message to a task's worker.
    Steer { task: TaskId, text: String },
    /// Run a command, bounded. It must be safe to run and reversible.
    Command {
        argv: Vec<String>,
        #[serde(default = "default_action_timeout")]
        timeout_secs: u64,
    },
}

fn default_action_timeout() -> u64 {
    1800
}

/// A condition and an action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub id: RuleId,
    /// What it is for, in the user's words.
    #[serde(default)]
    pub description: String,
    pub when: Condition,
    pub then: Action,
    /// Fire at most once, then stop. Command and `At` conditions always
    /// fire once.
    #[serde(default)]
    pub once: bool,
    #[serde(default = "enabled")]
    pub enabled: bool,
}

fn enabled() -> bool {
    true
}

impl Rule {
    pub fn new(id: impl Into<String>, when: Condition, then: Action) -> Self {
        Self {
            id: RuleId::new(id),
            description: String::new(),
            when,
            then,
            once: false,
            enabled: true,
        }
    }

    /// Whether the rule stops after its first fire.
    pub fn fires_once(&self) -> bool {
        self.once || matches!(self.when, Condition::At { .. } | Condition::Command { .. })
    }

    /// Refuse a rule that could never run safely.
    pub fn validate(&self) -> Result<()> {
        let bad = |m: &str| Err(CoreError::Invalid(format!("rule {}: {m}", self.id)));
        let id = self.id.as_str();
        if id.is_empty()
            || id.len() > 64
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return bad("id must be 1-64 letters, digits, '-', '_' or '.'");
        }
        match &self.when {
            Condition::Event { kind, fields, .. } => {
                if kind.is_empty() {
                    return bad("event kind is empty");
                }
                if kind.starts_with("trigger.") || kind == "trigger.*" {
                    // A rule woken by its own fires could loop.
                    return bad("a rule can't watch trigger events");
                }
                if let Some(p) = fields.keys().find(|p| !p.is_empty() && !p.starts_with('/')) {
                    return bad(&format!("field {p:?} is not a JSON pointer"));
                }
            }
            Condition::Every { secs } if *secs == 0 => return bad("interval is zero"),
            Condition::Every { .. } | Condition::At { .. } => {}
            Condition::Command {
                argv,
                interval_secs,
                stable,
                timeout_secs,
                ..
            } => {
                if argv.is_empty() || argv[0].is_empty() {
                    return bad("condition command is empty");
                }
                if *interval_secs == 0 || *stable == 0 || *timeout_secs == 0 {
                    return bad("interval, stable count and timeout must be at least 1");
                }
            }
        }
        match &self.then {
            Action::Command { argv, timeout_secs } => {
                if argv.is_empty() || argv[0].is_empty() {
                    return bad("action command is empty");
                }
                if *timeout_secs == 0 {
                    return bad("action timeout is zero");
                }
            }
            Action::Wake { note } if note.trim().is_empty() => return bad("wake note is empty"),
            Action::Inbox { body } if body.trim().is_empty() => return bad("inbox note is empty"),
            Action::Steer { text, .. } if text.trim().is_empty() => {
                return bad("steering text is empty")
            }
            _ => {}
        }
        Ok(())
    }
}

/// Whether an event condition matches `e`.
pub fn event_matches(cond: &Condition, e: &Event) -> bool {
    let Condition::Event { kind, task, fields } = cond else {
        return false;
    };
    let kind_ok = match kind.strip_suffix(".*") {
        Some(prefix) => e
            .kind
            .as_str()
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('.')),
        None => e.kind.as_str() == kind,
    };
    if !kind_ok || e.kind.prefix() == PREFIX {
        return false;
    }
    if task.as_ref().is_some_and(|t| e.task.as_ref() != Some(t)) {
        return false;
    }
    fields
        .iter()
        .all(|(ptr, want)| e.payload.pointer(ptr) == Some(want))
}

/// How a fire ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The action ran and succeeded.
    Ok,
    /// The action ran and failed.
    ActionFailed,
    /// The condition could not be evaluated within its error budget.
    ConditionError,
    /// A fire was claimed but its outcome was never recorded (a crash
    /// mid-action). The action may or may not have run; it is not retried.
    Ambiguous,
}

/// Payload of every `trigger.*` event; the kind is `trigger.<type>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TriggerEvent {
    /// A rule was added or replaced.
    Defined { rule: Rule, by: String },
    /// A rule was removed.
    Removed { id: RuleId, by: String },
    /// The rule fired and its action is about to run (native mode).
    Fired {
        id: RuleId,
        fire: String,
        /// The event that satisfied an event condition.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<Seq>,
    },
    /// How a fire ended.
    Outcome {
        id: RuleId,
        fire: String,
        status: Status,
        detail: String,
    },
    /// The rule would have fired; shadow mode records instead of acting.
    WouldFire {
        id: RuleId,
        fire: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<Seq>,
    },
    /// Events up to `through` have been evaluated.
    Cursor { through: Seq },
}

impl TriggerEvent {
    pub fn kind(&self) -> EventKind {
        let name = match self {
            TriggerEvent::Defined { .. } => "defined",
            TriggerEvent::Removed { .. } => "removed",
            TriggerEvent::Fired { .. } => "fired",
            TriggerEvent::Outcome { .. } => "outcome",
            TriggerEvent::WouldFire { .. } => "would_fire",
            TriggerEvent::Cursor { .. } => "cursor",
        };
        EventKind::new(format!("{PREFIX}.{name}"))
    }
}

/// A rule as currently defined, with the position it was defined at.
#[derive(Debug, Clone, PartialEq)]
pub struct Defined {
    pub rule: Rule,
    /// The `trigger.defined` event; fire ids include it, so a changed rule
    /// fires afresh.
    pub since: Seq,
    pub at: OffsetDateTime,
}

impl Defined {
    /// The id of this definition's fire `key`.
    pub fn fire_id(&self, key: &str) -> String {
        format!("{}:{key}", self.since.0)
    }
}

/// Rules and fires, folded from `trigger.*` events.
#[derive(Debug, Clone, Default)]
pub struct RuleSet {
    rules: BTreeMap<ProjectId, BTreeMap<RuleId, Defined>>,
    /// (project, rule) -> fire ids claimed or recorded.
    fires: BTreeMap<(ProjectId, RuleId), BTreeSet<String>>,
    /// Fires with a claim and no outcome yet: (project, rule, fire).
    open: BTreeSet<(ProjectId, RuleId, String)>,
    /// (project, rule) whose once-only fire already happened.
    spent: BTreeSet<(ProjectId, RuleId)>,
    cursor: Seq,
}

impl RuleSet {
    pub fn apply(&mut self, e: &Event) {
        if e.kind.prefix() != PREFIX {
            return;
        }
        let Ok(ev) = e.decode::<TriggerEvent>() else {
            tracing::warn!(
                seq = e.seq.0,
                kind = e.kind.as_str(),
                "unreadable trigger event"
            );
            return;
        };
        let p = e.project.clone();
        match ev {
            TriggerEvent::Defined { rule, .. } => {
                let key = (p.clone(), rule.id.clone());
                let rules = self.rules.entry(p).or_default();
                if rules.get(&rule.id).is_some_and(|d| d.rule == rule) {
                    // The same rule again changes nothing.
                    return;
                }
                // A new or changed rule starts over: new fire ids, not spent.
                self.spent.remove(&key);
                rules.insert(
                    rule.id.clone(),
                    Defined {
                        rule,
                        since: e.seq,
                        at: e.ts,
                    },
                );
            }
            TriggerEvent::Removed { id, .. } => {
                if let Some(r) = self.rules.get_mut(&p) {
                    r.remove(&id);
                }
            }
            TriggerEvent::Fired { id, fire, .. } | TriggerEvent::WouldFire { id, fire, .. } => {
                let shadow = e.kind.as_str().ends_with("would_fire");
                let key = (p.clone(), id.clone());
                self.fires
                    .entry(key.clone())
                    .or_default()
                    .insert(fire.clone());
                if self
                    .rule(&p, &id)
                    .is_some_and(|d| d.rule.fires_once() && d.since <= e.seq)
                {
                    self.spent.insert(key);
                }
                if !shadow {
                    self.open.insert((p, id, fire));
                }
            }
            TriggerEvent::Outcome { id, fire, .. } => {
                self.open.remove(&(p, id, fire));
            }
            TriggerEvent::Cursor { through } => self.cursor = self.cursor.max(through),
        }
    }

    pub fn rule(&self, project: &ProjectId, id: &RuleId) -> Option<&Defined> {
        self.rules.get(project).and_then(|r| r.get(id))
    }

    /// Every defined rule of `project`.
    pub fn rules(&self, project: &ProjectId) -> Vec<&Defined> {
        self.rules
            .get(project)
            .map(|r| r.values().collect())
            .unwrap_or_default()
    }

    /// Every Project with rules, and its enabled rules that can still fire.
    pub fn live(&self) -> Vec<(ProjectId, Defined)> {
        let mut out = Vec::new();
        for (p, rules) in &self.rules {
            for d in rules.values() {
                if d.rule.enabled && !self.spent.contains(&(p.clone(), d.rule.id.clone())) {
                    out.push((p.clone(), d.clone()));
                }
            }
        }
        out
    }

    pub fn has_fired(&self, project: &ProjectId, id: &RuleId, fire: &str) -> bool {
        self.fires
            .get(&(project.clone(), id.clone()))
            .is_some_and(|f| f.contains(fire))
    }

    /// How many fires `id` has had.
    pub fn fire_count(&self, project: &ProjectId, id: &RuleId) -> usize {
        self.fires
            .get(&(project.clone(), id.clone()))
            .map_or(0, |f| f.len())
    }

    /// Fires claimed with no outcome.
    pub fn open(&self) -> impl Iterator<Item = &(ProjectId, RuleId, String)> {
        self.open.iter()
    }

    /// How far rule evaluation got.
    pub fn cursor(&self) -> Seq {
        self.cursor
    }
}

/// Ready-made rules.
pub mod templates {
    use super::*;

    /// Follow up on an issue filed upstream: wake the coordinator once the
    /// issue's state or comment count differs from `seen` (as printed by
    /// the same command, such as `OPEN 2`). Polled hourly with the GitHub
    /// CLI.
    pub fn contribution_followup(repo: &str, issue: u64, seen: &str) -> Rule {
        let mut rule = Rule::new(
            format!("upstream-{}-{issue}", repo.replace('/', "-")),
            Condition::Command {
                argv: vec![
                    "gh".into(),
                    "issue".into(),
                    "view".into(),
                    issue.to_string(),
                    "--repo".into(),
                    repo.into(),
                    "--json".into(),
                    "state,comments".into(),
                    "--jq".into(),
                    r#""\(.state) \(.comments | length)""#.into(),
                ],
                interval_secs: 3600,
                stable: 1,
                timeout_secs: 60,
                expect: Some(Expect::Differs(seen.into())),
                error_budget: 24,
            },
            Action::Wake {
                note: format!(
                    "Upstream issue {repo}#{issue} changed since we last looked ({seen})."
                ),
            },
        );
        rule.description = format!("Follow up on {repo}#{issue}");
        rule
    }
}

#[cfg(test)]
mod tests {
    use quark_core::{HostId, NewEvent};

    use super::*;

    fn event(kind: &str, task: Option<&str>, payload: serde_json::Value) -> Event {
        NewEvent::new(
            HostId::from("h"),
            ProjectId::from("p"),
            task.map(TaskId::from),
            kind,
            payload,
        )
        .with_seq(Seq(1))
    }

    #[test]
    fn event_conditions_match_kind_task_and_fields() {
        let mut fields = BTreeMap::new();
        fields.insert("/verb".to_string(), serde_json::json!("done"));
        let c = Condition::Event {
            kind: "firstmate.*".into(),
            task: Some("t1".into()),
            fields,
        };
        let done = serde_json::json!({"verb": "done", "note": "PR"});
        assert!(event_matches(
            &c,
            &event("firstmate.status", Some("t1"), done.clone())
        ));
        assert!(!event_matches(
            &c,
            &event("firstmate.status", Some("t2"), done.clone())
        ));
        assert!(!event_matches(
            &c,
            &event("firstmatex.status", Some("t1"), done.clone())
        ));
        assert!(!event_matches(
            &c,
            &event(
                "firstmate.status",
                Some("t1"),
                serde_json::json!({"verb": "working"})
            )
        ));
        let exact = Condition::Event {
            kind: "channel.received".into(),
            task: None,
            fields: BTreeMap::new(),
        };
        assert!(event_matches(
            &exact,
            &event("channel.received", None, done.clone())
        ));
        assert!(!event_matches(&exact, &event("channel.acked", None, done)));
    }

    #[test]
    fn validation() {
        let wake = Action::Wake { note: "x".into() };
        Rule::new("ok-1", Condition::Every { secs: 5 }, wake.clone())
            .validate()
            .unwrap();
        assert!(
            Rule::new("bad id", Condition::Every { secs: 5 }, wake.clone())
                .validate()
                .is_err()
        );
        assert!(Rule::new("r", Condition::Every { secs: 0 }, wake.clone())
            .validate()
            .is_err());
        let loopy = Condition::Event {
            kind: "trigger.fired".into(),
            task: None,
            fields: BTreeMap::new(),
        };
        assert!(Rule::new("r", loopy, wake.clone()).validate().is_err());
        assert!(Rule::new(
            "r",
            Condition::Every { secs: 5 },
            Action::Command {
                argv: vec![],
                timeout_secs: 5
            }
        )
        .validate()
        .is_err());
        templates::contribution_followup("org/repo", 12, "OPEN 0")
            .validate()
            .unwrap();
    }

    #[test]
    fn rule_json_shape() {
        let r = templates::contribution_followup("org/repo", 12, "OPEN 0");
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["when"]["source"], "command");
        assert_eq!(v["then"]["do"], "wake");
        assert_eq!(v["when"]["expect"]["op"], "differs");
        let back: Rule = serde_json::from_value(v).unwrap();
        assert_eq!(back, r);
        assert!(r.fires_once());
    }
}
