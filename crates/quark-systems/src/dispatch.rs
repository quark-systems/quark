//! Dispatch records, rules, profiles and dry-run tests.

use crate::AccountFailover;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Why a task got its agent (ADR-11): one record per spawn of the task's
/// worker. Records are history: they are kept after the task ends, for later
/// scoring, and never change once recorded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DispatchRecord {
    pub id: String,
    pub task_id: String,
    pub project_id: String,
    /// What started this worker.
    pub trigger: DispatchTrigger,
    /// Who chose the agent.
    pub decided_by: DispatchDecider,
    /// One sentence on why this agent, for people.
    pub summary: String,
    /// The dispatch rule the classifier matched; absent when none matched or
    /// no classifier was consulted.
    pub rule: Option<DispatchRule>,
    /// What the engine's dispatch resolution reported.
    pub resolution: DispatchResolution,
    /// Every profile the resolution weighed, in the rule's order, each with
    /// whether it passed and why.
    pub candidates: Vec<DispatchCandidate>,
    /// The agent the worker was actually started with.
    pub chosen: DispatchChoice,
    pub classifier: DispatchClassifier,
    /// The rate limit this relaunch answered, when the daemon relaunched the
    /// worker to move it to another account; also in `Task.failovers`.
    #[serde(default)]
    pub failover: Option<AccountFailover>,
    /// When the daemon recorded the dispatch (RFC 3339 UTC).
    pub recorded_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DispatchTrigger {
    /// The task's first worker.
    Spawn,
    /// A replacement worker in the same worktree.
    Relaunch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DispatchDecider {
    /// The classifier matched a rule confidently and the worker was started
    /// with the profile its resolution selected.
    Classifier,
    /// The coordinator picked: no classifier, a resolution that was not
    /// clear, or a selected profile the coordinator overrode.
    Coordinator,
    /// The classifier's answer was not used (below its floor, a timeout or
    /// a failure), `on_failure: default` resolved the default rule instead,
    /// and the worker was started with the profile that selected.
    DefaultRule,
    /// The worker was relaunched in its worktree; dispatch rules were not
    /// consulted again.
    Relaunch,
}

/// A dispatch rule, as the Project's dispatch rules name it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DispatchRule {
    /// The rule's id in the resolution, e.g. `rule_2`, or `default`.
    pub id: String,
    /// The rule's `when` condition.
    pub when: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DispatchStatus {
    /// A profile was selected.
    Clear,
    /// The classifier's confidence was below its floor.
    Ambiguous,
    /// The rule needs approval, no candidate qualified, there was a tie, or
    /// there were no rules to match.
    Escalate,
    /// The classifier or the quota read failed.
    Error,
    /// No classifier is configured (`provider: none`).
    Off,
    /// The resolution was not run for this worker: a relaunch, or a worker
    /// that started before the daemon saw it.
    NotConsulted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DispatchResolution {
    pub status: DispatchStatus,
    /// Why the status is not `clear`.
    pub reason: Option<String>,
    /// Further notes from the resolution, such as an unranked candidate.
    pub notes: Vec<String>,
    /// The resolution's output as the engine printed it, kept for scoring.
    pub output: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DispatchCandidate {
    pub harness: String,
    pub model: Option<String>,
    /// Whether the candidate was eligible.
    pub passed: bool,
    /// Why it passed or failed, e.g. `eligible` or `profile floor
    /// all_models below 15%`.
    pub reason: String,
    /// The quota evidence it was judged on, e.g. `provider=claude
    /// scope=all_models remaining=79%`.
    pub evidence: Option<String>,
}

/// The agent a worker was started with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DispatchChoice {
    pub harness: String,
    /// Absent for the harness's default model.
    pub model: Option<String>,
    /// Absent for the harness's default effort.
    pub effort: Option<String>,
    /// The account the worker was started under (an `Account.id`, as on the
    /// task when it was recorded); absent when the harness has no accounts or
    /// the daemon does not know it.
    pub account: Option<String>,
}

/// The classifier behind the System-1 API, as consulted for this dispatch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DispatchClassifier {
    /// `none` when no classifier is configured. A configured classifier that
    /// did not answer has a provider and no model or confidence.
    pub provider: String,
    /// The model that answered, e.g. `jev-1.13.0`.
    pub model: Option<String>,
    /// The classifier's confidence in the matched rule, from 0 to 1.
    pub confidence: Option<f64>,
}

/// How a list of candidate profiles is resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DispatchSelect {
    /// The first candidate that can run.
    Ordered,
    /// The candidate with the most quota to spend.
    QuotaBalanced,
}

/// A quota floor; the engine applies it only under typed resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DispatchFloor {
    pub scope: String,
    #[schema(value_type = f64)]
    pub min_percent: serde_json::Number,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

/// One candidate of a dispatch rule or of the default: a harness with its
/// model, effort and account pool.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DispatchProfile {
    /// Harness id, e.g. `claude-code`.
    pub harness: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    /// Account pool to run under (see `Account.pools`); absent runs under
    /// the harness's default account.
    #[serde(default)]
    pub pool: Option<String>,
    /// The engine's typed-resolution fields, kept as `dispatch.yaml` has them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub floor: Option<DispatchFloor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<String>,
}

/// One rule of a Project's `dispatch.yaml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DispatchRuleSpec {
    /// Quark's name for the rule, unique in the file.
    #[serde(default)]
    pub name: Option<String>,
    /// The condition a task must meet, in plain words.
    pub when: String,
    /// The rule's `use` list, in order; at least one.
    pub candidates: Vec<DispatchProfile>,
    /// How `candidates` are resolved; absent uses `default_select`.
    #[serde(default)]
    pub select: Option<DispatchSelect>,
    /// The engine's typed-resolution fields, kept as `dispatch.yaml` has them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub floor: Option<DispatchFloor>,
}

/// The editable part of a Project's `dispatch.yaml`: its rules, in order,
/// and the default. The `classifier` block is not part of it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DispatchRulesDraft {
    /// How candidate lists are resolved where a rule does not say.
    #[serde(default)]
    pub default_select: Option<DispatchSelect>,
    #[serde(default)]
    pub rules: Vec<DispatchRuleSpec>,
    /// The candidates for a task no rule matches, in order.
    #[serde(default)]
    pub default: Vec<DispatchProfile>,
}

/// A Project's dispatch rules: `dispatch.yaml` on the Project repo's `main`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DispatchRules {
    pub project_id: String,
    /// Names this version of the file (its git blob id); absent when `main`
    /// has no `dispatch.yaml`. Send it back when saving.
    pub revision: Option<String>,
    /// The commit on `main` that last changed the file.
    pub commit: Option<String>,
    /// The classifier in effect: the file's `classifier` block over the
    /// user-level default, with `provider: none` when neither names one. Its
    /// `credential` is a keychain reference, never a key. Saving keeps the
    /// file's own block.
    #[schema(value_type = Option<Object>)]
    pub classifier: Option<serde_json::Value>,
    pub default_select: Option<DispatchSelect>,
    pub rules: Vec<DispatchRuleSpec>,
    pub default: Vec<DispatchProfile>,
}

/// Request body for `PUT /v1/projects/{id}/dispatch`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PutDispatchRules {
    /// The `revision` the edit started from. When `main` has another by
    /// now, nothing is saved. Absent saves over whatever is there.
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(flatten)]
    pub rules: DispatchRulesDraft,
}

/// A task description to test against a Project's dispatch rules.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TestDispatch {
    /// The task as it would be briefed to a worker.
    pub description: String,
    /// Rules to test instead of the saved ones. They are compiled and
    /// never written. The engine's dispatch resolution reads the saved
    /// rules, so it is not run for a draft that differs from them.
    #[serde(default)]
    pub draft: Option<DispatchRulesDraft>,
}

/// What a Project's dispatch rules would do with a task description
/// (ADR-11). Nothing is dispatched or recorded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DispatchTest {
    pub project_id: String,
    /// Who would choose the agent: `classifier` when the resolution selects
    /// a matched rule's profile, `default_rule` when it falls back to the
    /// default rule (`on_failure: default`), else `coordinator`.
    pub decided_by: DispatchDecider,
    /// One sentence on the outcome, for people.
    pub summary: String,
    /// The dispatch rule the classifier matched; absent when none matched or
    /// no classifier is configured.
    pub rule: Option<DispatchRule>,
    /// What the engine's dispatch resolution reported for the description.
    pub resolution: DispatchResolution,
    /// `provider` is `none` when no classifier is configured and the
    /// coordinator would pick.
    pub classifier: DispatchClassifier,
    /// The profile the resolution selected; absent when the coordinator
    /// would pick.
    pub chosen: Option<DispatchChoice>,
    /// The matched rule's profiles when the resolution weighed them, else
    /// every profile of every rule and the default, in the order the rules
    /// list them.
    pub candidates: Vec<DispatchTestCandidate>,
}

/// One profile of the Project's dispatch rules, with whether a worker could
/// be started with it now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DispatchTestCandidate {
    /// The rule that lists the profile; its id is `default` for a default
    /// profile.
    pub rule: DispatchRule,
    /// The rule's `name` in `dispatch.yaml`, when it has one.
    pub rule_name: Option<String>,
    /// Harness id, e.g. `claude-code`.
    pub harness: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// The account pool the profile runs under, when it names one.
    pub pool: Option<String>,
    /// True when every check passed.
    pub passed: bool,
    /// Why it passed or failed: the first failed check, else the
    /// resolution's verdict or `eligible`.
    pub reason: String,
    /// The quota evidence it was judged on.
    pub evidence: Option<String>,
    /// One entry per [`DispatchCheckKind`], in that order.
    pub checks: Vec<DispatchCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DispatchCheck {
    pub check: DispatchCheckKind,
    pub passed: bool,
    /// What was found, e.g. `Claude Code 2.1.0 at /usr/local/bin/claude`.
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DispatchCheckKind {
    /// The harness's executable is on this machine.
    HarnessInstalled,
    /// The harness accepts the profile's model and effort for a worker.
    ModelAccepted,
    /// An account the profile can run under has a credential.
    AccountHealth,
    /// Such an account has quota left, by the resolution's quota evidence
    /// and each account's own reading.
    QuotaHeadroom,
}
