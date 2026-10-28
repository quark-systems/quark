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
