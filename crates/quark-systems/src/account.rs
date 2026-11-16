//! Accounts, quota readings and account failover.

use crate::HarnessAuth;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// How a rate limit on a task's account was handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FailoverOutcome {
    /// The worker was relaunched from its branch, in the same worktree,
    /// under `to_account_id`.
    Relaunched,
    /// No other account in the pool was healthy; a decision was opened.
    NoHealthyAccount,
    /// The engine could not relaunch the worker; a decision was opened.
    RelaunchFailed,
}

/// One rate limit a task's worker hit, and where the worker went (ADR-11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AccountFailover {
    /// The account that reported the rate limit (an `Account.id`).
    pub from_account_id: String,
    /// The account the worker was relaunched under; absent unless `outcome`
    /// is `relaunched`.
    pub to_account_id: Option<String>,
    /// The pool the next account was chosen from, when one was named.
    pub pool: Option<String>,
    pub outcome: FailoverOutcome,
    /// The harness log line that reported the limit, e.g.
    /// `claude: assistant isApiErrorMessage error=rate_limit`.
    pub signal: String,
    /// What the harness said about the limit, or why the relaunch failed.
    pub detail: Option<String>,
    /// When the daemon handled it (RFC 3339 UTC).
    pub at: String,
}

/// An account a harness runs under: its own config directory, holding what
/// the harness writes after its own login, or for a harness that signs in
/// with keys (IBM Bob), an API key kept in the Keychain. Every installed
/// harness has a default account (`default: true`), its usual login, which
/// is listed but cannot be removed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Account {
    /// `acc_...`, or `default-<harness>` for a harness's default account.
    pub id: String,
    /// Harness id, e.g. `claude-code`.
    pub harness: String,
    pub label: String,
    /// The config directory the harness is pointed at (`CLAUDE_CONFIG_DIR`,
    /// `CODEX_HOME` and equivalents). Absent for a key account.
    pub config_dir: Option<String>,
    /// True for an account that signs in with an API key kept in the
    /// Keychain. The key is never returned.
    #[serde(default)]
    pub api_key: bool,
    /// The team a key account's key belongs to, when the harness needs one.
    #[serde(default)]
    pub team_id: Option<String>,
    pub default: bool,
    /// Pools the account belongs to. A dispatch profile naming a pool runs
    /// each task under one of the pool's accounts for its harness.
    pub pools: Vec<String>,
    /// Credential health from the harness adapter.
    pub health: HarnessAuth,
    pub quota: AccountQuota,
    /// Tasks and coordinators currently running under the account.
    pub active_tasks: u32,
    /// False when the engine cannot start this harness under another account
    /// yet, so only its default account can be chosen from a pool.
    pub launchable: bool,
    pub created_at: Option<String>,
    /// False once the person disabled the account for Quark: no new task or
    /// coordinator starts under it, in any Project. Every account starts
    /// enabled.
    #[serde(default = "enabled")]
    pub enabled: bool,
    /// Where the account stands: `enabled`, `draining` (disabled, with work
    /// started before still running under it) or `disabled`.
    #[serde(default)]
    pub availability: AccountAvailability,
    /// The account is removed once the work running under it finishes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub removing: bool,
}

fn enabled() -> bool {
    true
}

/// Whether Quark starts work under an account.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccountAvailability {
    /// New tasks and coordinators may start under it.
    #[default]
    Enabled,
    /// Disabled, but work that started before still runs under it; it is
    /// `disabled` once that work finishes.
    Draining,
    /// Nothing starts or runs under it.
    Disabled,
}

/// What the person is about to do to an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccountChangeAction {
    Disable,
    Remove,
}

/// Request body for `POST /v1/accounts/{id}/plan`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PlanAccountChange {
    pub action: AccountChangeAction,
}

/// What disabling or removing an account touches, and what Quark proposes
/// for each dispatch profile left without an enabled account. Nothing is
/// changed until the person confirms with `POST /v1/accounts/{id}/disable`
/// or `/remove`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AccountChangePlan {
    pub account_id: String,
    pub action: AccountChangeAction,
    /// Tasks and coordinators running under the account now. They keep
    /// running until they finish; the account is draining meanwhile.
    pub running: Vec<AccountWork>,
    /// Every dispatch profile and coordinator agent, in any Project, that
    /// can run under the account.
    pub references: Vec<AccountReference>,
    /// Why the change cannot be made at all, e.g. removing a default account.
    pub refused: Option<String>,
}

/// One task or coordinator running under an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AccountWork {
    pub project_id: String,
    pub project_name: String,
    /// Absent for the Project's coordinator.
    pub task_id: Option<String>,
    /// The task's title, or `Coordinator`.
    pub title: String,
}

/// Where in a Project an agent profile is configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccountPlace {
    /// A candidate of one of the dispatch rules.
    Rule,
    /// A candidate of the dispatch rules' default.
    Default,
    /// The Project coordinator's own agent.
    Coordinator,
}

/// A dispatch profile or coordinator agent that can run under the account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AccountReference {
    /// Names the reference in a confirmation, e.g. `prj_1/rules/0/1`.
    pub id: String,
    pub project_id: String,
    pub project_name: String,
    pub place: AccountPlace,
    /// The rule's name, or `rule N` (1-based) for one without a name.
    pub rule: Option<String>,
    pub harness: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// The pool it names; absent runs under the harness's default account.
    pub pool: Option<String>,
    /// Labels of the enabled accounts it keeps after the change.
    pub remaining: Vec<String>,
    /// No enabled account is left for it: one of `options` must be chosen.
    pub stranded: bool,
    /// What Quark can do with it, the one it proposes first. Empty when it
    /// is not stranded.
    pub options: Vec<AccountDisposition>,
}

/// What to do with a stranded profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AccountDisposition {
    /// Run it on another pool of the same harness.
    UsePool {
        pool: String,
        /// Labels of the pool's enabled accounts.
        #[serde(default)]
        accounts: Vec<String>,
    },
    /// Run it on the harness's default account (drop its pool).
    UseDefault,
    /// Take the candidate out of its rule; the rule keeps its others.
    Drop,
    /// Leave it as it is: work it would get goes to the coordinator to place.
    Keep,
}

impl AccountDisposition {
    /// Same choice, ignoring the labels shown with it.
    pub fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::UsePool { pool: a, .. }, Self::UsePool { pool: b, .. }) => a == b,
            _ => std::mem::discriminant(self) == std::mem::discriminant(other),
        }
    }
}

/// Request body for `POST /v1/accounts/{id}/disable` and `/remove`: the
/// person's choice for each stranded reference of the plan, by its id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ApplyAccountChange {
    #[serde(default)]
    pub dispositions: std::collections::BTreeMap<String, AccountDisposition>,
}

/// What a confirmed disable or remove did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AccountChangeOutcome {
    /// The account as it is now; absent once removed.
    pub account: Option<Account>,
    /// The account is gone.
    pub removed: bool,
    /// Projects whose dispatch rules or coordinator agent changed.
    pub changed: Vec<AccountChangeApplied>,
}

/// One Project's part of a confirmed change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AccountChangeApplied {
    pub project_id: String,
    pub project_name: String,
    /// The Project repo commit with the new dispatch rules, when they changed.
    pub commit: Option<String>,
    /// The coordinator's agent changed (a switch was started when it runs).
    pub coordinator: bool,
}

/// Add an account for a harness.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CreateAccount {
    pub harness: String,
    /// Shown in the app; defaults to the directory name.
    pub label: Option<String>,
    /// Absolute path of the account's config directory. Log in once with the
    /// harness pointed at it, e.g. `CLAUDE_CONFIG_DIR=<dir> claude`.
    /// Leave empty for a key account.
    #[serde(default)]
    pub config_dir: String,
    /// The API key, for a harness whose extra accounts are keys
    /// (`HarnessInfo.key_env`). Stored in the Keychain, never returned.
    #[serde(default)]
    pub api_key: Option<String>,
    /// The key's team, e.g. for an IBM Bob key of type "general".
    #[serde(default)]
    pub team_id: Option<String>,
    #[serde(default)]
    pub pools: Vec<String>,
}

/// An API key for a default account to sign in with, kept in the Keychain
/// (`PUT /v1/accounts/{id}/key`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SetAccountKey {
    /// Never returned or logged.
    pub api_key: String,
}

/// Partial update; absent fields are left unchanged.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct UpdateAccount {
    /// Not accepted for a default account.
    pub label: Option<String>,
    /// Replaces the account's pools.
    pub pools: Option<Vec<String>>,
}

/// Whether an account's quota could be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuotaState {
    /// Not read yet.
    Pending,
    /// `remaining_percent` and `windows` hold the latest reading.
    Known,
    /// The read ran but had no numbers, e.g. the account needs a login.
    Unavailable,
    /// The quota reader failed or is not installed.
    Error,
    /// Quota is read per account for Claude Code and Codex only.
    Unsupported,
}

/// One rate-limit window of an account's plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct QuotaWindow {
    /// e.g. `five_hour`, `weekly`.
    pub id: String,
    /// e.g. `session`, `week`.
    pub label: String,
    pub percent_remaining: Option<f64>,
    /// RFC 3339 UTC.
    pub resets_at: Option<String>,
    /// How much of the allowance is spent, in [`Self::unit`], when the
    /// provider counts it (Bob, Kiro).
    #[serde(default)]
    pub used: Option<f64>,
    /// The window's allowance, in [`Self::unit`].
    #[serde(default)]
    pub limit: Option<f64>,
    /// What `used` and `limit` count, e.g. `Bobcoins`, `credits`.
    #[serde(default)]
    pub unit: Option<String>,
    /// The part of `used` past `limit`, billed as overage (Kiro).
    #[serde(default)]
    pub overage: Option<f64>,
}

/// An account's latest quota reading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AccountQuota {
    pub state: QuotaState,
    /// What limits the account now, 0 to 100.
    pub remaining_percent: Option<f64>,
    /// The subscription plan, when reported.
    pub plan: Option<String>,
    pub windows: Vec<QuotaWindow>,
    /// Why there are no numbers.
    pub detail: Option<String>,
    /// When the reading was taken (RFC 3339 UTC).
    pub checked_at: Option<String>,
    /// The reading needs a one-time macOS Keychain grant for this account's
    /// login, given with `POST /v1/accounts/{id}/keychain-grant`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keychain_grant: bool,
    /// For a harness that signs in to many providers (Pi, OpenCode), one
    /// reading per provider login; the fields above then sum them up.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<ProviderQuota>,
}

/// One provider login's reading within a multi-provider account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ProviderQuota {
    /// The quota provider (`claude`, `openrouter`, ...).
    pub provider: String,
    /// Its display name.
    pub label: String,
    pub state: QuotaState,
    pub remaining_percent: Option<f64>,
    pub plan: Option<String>,
    pub windows: Vec<QuotaWindow>,
    pub detail: Option<String>,
}

impl AccountQuota {
    /// A quota with no numbers in `state`.
    pub fn empty(state: QuotaState, detail: Option<String>) -> Self {
        Self {
            state,
            remaining_percent: None,
            plan: None,
            windows: Vec::new(),
            detail,
            checked_at: None,
            keychain_grant: false,
            providers: Vec::new(),
        }
    }

    /// True when a reading shows nothing left.
    pub fn exhausted(&self) -> bool {
        self.state == QuotaState::Known && self.remaining_percent.is_some_and(|p| p <= 0.0)
    }
}

/// Payload of `account.quota_changed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AccountQuotaChanged {
    pub account_id: String,
    pub harness: String,
    pub quota: AccountQuota,
}
