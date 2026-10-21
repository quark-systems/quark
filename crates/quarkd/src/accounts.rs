//! Accounts and pools per harness, with per-account quota (ADR-11).
//!
//! An account is a harness config directory (`CLAUDE_CONFIG_DIR`,
//! `CODEX_HOME` and equivalents). Every installed harness has a default
//! one, its usual login; harnesses with an account variable take more,
//! added through the API, and any account can be grouped into named pools. A dispatch profile (an [`AgentConfig`]) naming a pool
//! runs under one of the pool's accounts: the same one for the life of a
//! task (sticky), and the least busy one when a task or coordinator starts
//! (balanced).
//!
//! The daemon applies a choice where it starts an agent itself, a Project's
//! coordinator or a relaunched task, by setting the harness's account
//! variable on the engine call. Workers the coordinator starts inherit its
//! account through the engine.
//!
//! A harness whose logins cannot be moved to another directory but that
//! signs in with an API key (IBM Bob) takes key accounts instead: the key
//! is kept in the Keychain ([`crate::secrets`]) and reaches only the
//! worker's launch, as a reference the supervisor resolves.
//!
//! Quota comes from `quota-axi --provider claude|codex --json`, one call per
//! account with the account variable naming its directory (unset for the
//! default account), and changes stream as `account.quota_changed`. Not
//! `--profile-only`: it reads only a credential file, and Claude Code on
//! macOS keeps its credentials in the Keychain. Providers quota-axi does
//! not report, IBM Bob and Kiro CLI, are read by quarkd itself
//! ([`quark_dispatch::local`]).

use std::cmp::Reverse;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use quark_dispatch::local::LocalReading;
use quark_systems::{
    Account, AccountQuota, AgentConfig, AuthState, CreateAccount, QuotaState, QuotaWindow,
    UpdateAccount,
};
use serde::Deserialize;

use crate::harness::{self, pool_name_ok, Harness, HarnessRegistry};
use crate::now_rfc3339;
use crate::secrets::{Keychain, SecretStore};
use crate::store::{default_account_id, AccountRow, Store, StoreError, KEYCHAIN_PREFIX};

/// Longest account label.
const MAX_LABEL_CHARS: usize = 100;

/// Longest a single quota read may run.
const QUOTA_TIMEOUT: Duration = Duration::from_secs(60);

/// Longest quota failure detail kept, in characters.
const QUOTA_DETAIL_CHARS: usize = 300;

/// Longest API key accepted.
const MAX_KEY_CHARS: usize = 1024;

/// The Keychain service an account's API key is kept under.
pub fn key_service(account_id: &str) -> String {
    format!("quark/accounts/{account_id}")
}

#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    #[error("{0}")]
    Invalid(String),
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Conflict(String),
    #[error(transparent)]
    Store(StoreError),
}

impl From<StoreError> for AccountError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound => AccountError::NotFound,
            StoreError::Conflict(m) => AccountError::Conflict(m),
            StoreError::Invalid(m) => AccountError::Invalid(m),
            e => AccountError::Store(e),
        }
    }
}

pub type Result<T> = std::result::Result<T, AccountError>;

/// What runs under a leased account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holder {
    /// A task's worker, by API task id.
    Task(String),
    /// A Project's coordinator, by Project id.
    Coordinator(String),
}

/// The account chosen for an agent start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub account_id: String,
    /// The harness's account variable and the account's directory; empty for
    /// the default account, which is the daemon's own environment.
    pub env: Vec<(String, String)>,
}

/// Reads one account's quota.
#[async_trait]
pub trait QuotaReader: Send + Sync {
    /// Runs `quota-axi --provider <provider> --json` with `env_var` set to
    /// `config_dir` (left as the daemon's own for the `default` account,
    /// since setting it changes which Keychain item Claude Code uses) and
    /// returns its report.
    async fn read(
        &self,
        provider: &str,
        env_var: &str,
        config_dir: &Path,
        default: bool,
    ) -> std::result::Result<String, String>;

    /// The report for an account that signs in with `key`.
    async fn read_key(&self, provider: &str, _key: &str) -> std::result::Result<String, String> {
        Err(format!("no per-key quota reading for {provider}"))
    }
}

/// The `quota-axi` command, and quarkd's own readings for the providers it
/// does not report.
#[derive(Clone)]
pub struct QuotaAxi {
    pub bin: PathBuf,
    /// Read instead of quota-axi for their provider.
    pub local: Vec<Arc<dyn LocalReading>>,
}

impl std::fmt::Debug for QuotaAxi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let local: Vec<&str> = self.local.iter().map(|r| r.provider()).collect();
        f.debug_struct("QuotaAxi")
            .field("bin", &self.bin)
            .field("local", &local)
            .finish()
    }
}

impl Default for QuotaAxi {
    fn default() -> Self {
        Self {
            bin: PathBuf::from("quota-axi"),
            local: Vec::new(),
        }
    }
}

#[async_trait]
impl QuotaReader for QuotaAxi {
    async fn read(
        &self,
        provider: &str,
        env_var: &str,
        config_dir: &Path,
        default: bool,
    ) -> std::result::Result<String, String> {
        if let Some(r) = self.local.iter().find(|r| r.provider() == provider) {
            return Ok(r.read().await.to_string());
        }
        let mut cmd = tokio::process::Command::new(&self.bin);
        cmd.args(["--provider", provider, "--json"]);
        if !default {
            cmd.env(env_var, config_dir);
        }
        let run = cmd
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output();
        let out = tokio::time::timeout(QUOTA_TIMEOUT, run)
            .await
            .map_err(|_| format!("quota-axi timed out after {}s", QUOTA_TIMEOUT.as_secs()))?
            .map_err(|e| format!("could not run {}: {e}", self.bin.display()))?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        // A failed provider still prints its report, with the reason.
        if out.status.success() || serde_json::from_str::<QuotaReport>(&stdout).is_ok() {
            return Ok(stdout);
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let text = if stdout.trim().is_empty() {
            stderr.as_ref()
        } else {
            stdout.as_str()
        };
        Err(failure_detail(text))
    }

    async fn read_key(&self, provider: &str, key: &str) -> std::result::Result<String, String> {
        let r = self.local.iter().find(|r| r.provider() == provider);
        match r {
            Some(r) => match r.read_with_key(key).await {
                Some(v) => Ok(v.to_string()),
                None => Err(format!("no per-key quota reading for {provider}")),
            },
            None => Err(format!("no per-key quota reading for {provider}")),
        }
    }
}

/// A quota reader for tests: canned output per config directory, and a log
/// of every read.
#[derive(Debug, Default)]
pub struct StubQuota {
    readings:
        std::sync::Mutex<std::collections::HashMap<PathBuf, std::result::Result<String, String>>>,
    calls: std::sync::Mutex<Vec<(String, String, PathBuf, bool)>>,
}

impl StubQuota {
    pub fn new() -> Self {
        Self::default()
    }

    /// What a read of `config_dir` returns: quota-axi's stdout, or a failure.
    pub fn set(&self, config_dir: impl Into<PathBuf>, out: std::result::Result<String, String>) {
        self.readings.lock().unwrap().insert(config_dir.into(), out);
    }

    /// Reads so far, as `(provider, variable, config_dir, default)`.
    pub fn calls(&self) -> Vec<(String, String, PathBuf, bool)> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl QuotaReader for StubQuota {
    async fn read(
        &self,
        provider: &str,
        env_var: &str,
        config_dir: &Path,
        default: bool,
    ) -> std::result::Result<String, String> {
        self.calls.lock().unwrap().push((
            provider.into(),
            env_var.into(),
            config_dir.to_path_buf(),
            default,
        ));
        self.readings
            .lock()
            .unwrap()
            .get(config_dir)
            .cloned()
            .unwrap_or_else(|| Err("no reading".into()))
    }

    /// Set with `config_dir` `key:<key>`.
    async fn read_key(&self, provider: &str, key: &str) -> std::result::Result<String, String> {
        let dir = PathBuf::from(format!("key:{key}"));
        self.read(provider, "", &dir, false).await
    }
}

/// The first meaningful line of a failed read.
fn failure_detail(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("quota-axi failed")
        .trim_start_matches("error:")
        .trim()
        .trim_matches('"');
    line.chars().take(QUOTA_DETAIL_CHARS).collect()
}

/// Accounts, pools, leases and quota.
pub struct Accounts {
    store: Arc<Store>,
    harnesses: Arc<HarnessRegistry>,
    quota: Arc<dyn QuotaReader>,
    /// Where key accounts' keys are kept.
    secrets: Arc<dyn SecretStore>,
    /// Account variables the engine carries onto a launch.
    forwarded: &'static [&'static str],
    /// Serializes choices so two starts never pick the same idle account
    /// from stale counts.
    lease_lock: tokio::sync::Mutex<()>,
}

impl Accounts {
    pub fn new(
        store: Arc<Store>,
        harnesses: Arc<HarnessRegistry>,
        quota: Arc<dyn QuotaReader>,
        forwarded: &'static [&'static str],
    ) -> Self {
        Self {
            store,
            harnesses,
            quota,
            secrets: Arc::new(Keychain::default()),
            forwarded,
            lease_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Keep key accounts' keys in `secrets` instead of the Keychain.
    pub fn with_secrets(mut self, secrets: Arc<dyn SecretStore>) -> Self {
        self.secrets = secrets;
        self
    }

    /// Every account: per harness, its default account (when the harness is
    /// installed or has added accounts) and then the added ones, oldest
    /// first. A harness without an account variable has only its default.
    pub async fn list(&self) -> Result<Vec<Account>> {
        let infos = self.harnesses.list(false).await;
        let store = self.store.clone();
        let (rows, pools, quotas, leases) = tokio::task::spawn_blocking(move || {
            Ok::<_, StoreError>((
                store.account_rows()?,
                store.account_pools()?,
                store.account_quotas()?,
                store.active_leases()?,
            ))
        })
        .await
        .map_err(|e| AccountError::Store(StoreError::Invalid(e.to_string())))??;

        let env = self.harnesses.env();
        let mut out = Vec::new();
        for h in self.harnesses.all() {
            let var = h.account_env();
            let installed = infos.iter().any(|i| i.id == h.id() && i.install.installed);
            let added: Vec<&AccountRow> = rows.iter().filter(|r| r.harness == h.id()).collect();
            if !installed && added.is_empty() {
                continue;
            }
            let quota_for = |id: &str| {
                quotas
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| match h.quota_provider() {
                        Some(_) => AccountQuota::empty(QuotaState::Pending, None),
                        None => AccountQuota::empty(
                            QuotaState::Unsupported,
                            Some(format!("Quark has no quota reading for {}", h.name())),
                        ),
                    })
            };
            let default_id = default_account_id(h.id());
            out.push(Account {
                health: h.auth_status(env, &harness::Account::default()),
                quota: quota_for(&default_id),
                pools: pools.get(&default_id).cloned().unwrap_or_default(),
                active_tasks: leases.get(&default_id).copied().unwrap_or(0),
                id: default_id,
                harness: h.id().into(),
                label: "Default".into(),
                config_dir: h.default_config_dir(env).map(|d| d.display().to_string()),
                api_key: false,
                team_id: None,
                default: true,
                launchable: true,
                created_at: None,
            });
            for r in added {
                let keyed = r.key_service().is_some();
                let (config_dir, health, launchable) = if keyed {
                    (
                        None,
                        quark_systems::HarnessAuth {
                            state: AuthState::Configured,
                            detail: "API key kept in the Keychain".into(),
                        },
                        account_env(h.as_ref(), &r.id, None, true, r.team_id.as_deref())
                            .iter()
                            .all(|(v, _)| self.forwarded.contains(&v.as_str())),
                    )
                } else {
                    let account = harness::Account {
                        config_dir: Some(PathBuf::from(&r.config_dir)),
                    };
                    (
                        Some(r.config_dir.clone()),
                        h.auth_status(env, &account),
                        var.is_some_and(|v| self.forwarded.contains(&v)),
                    )
                };
                out.push(Account {
                    id: r.id.clone(),
                    harness: r.harness.clone(),
                    label: r.label.clone(),
                    config_dir,
                    api_key: keyed,
                    team_id: r.team_id.clone(),
                    default: false,
                    pools: pools.get(&r.id).cloned().unwrap_or_default(),
                    health,
                    quota: quota_for(&r.id),
                    active_tasks: leases.get(&r.id).copied().unwrap_or(0),
                    launchable,
                    created_at: Some(r.created_at.clone()),
                });
            }
        }
        Ok(out)
    }

    pub async fn get(&self, id: &str) -> Result<Account> {
        self.list()
            .await?
            .into_iter()
            .find(|a| a.id == id)
            .ok_or(AccountError::NotFound)
    }

    /// Adds an account for a harness that supports more than one.
    pub async fn create(&self, input: CreateAccount) -> Result<Account> {
        let h = self
            .harnesses
            .get(input.harness.trim())
            .ok_or_else(|| AccountError::Invalid(format!("unknown harness `{}`", input.harness)))?
            .clone();
        if let Some(key) = input.api_key.as_deref() {
            return self.create_key(h.as_ref(), key, &input).await;
        }
        if h.account_env().is_none() {
            return Err(AccountError::Invalid(match h.key_env() {
                Some(_) => format!(
                    "{} accounts beyond the default are API keys: give api_key, not config_dir",
                    h.name()
                ),
                None => format!("{} supports only its default account", h.name()),
            }));
        }
        let dir = config_dir(&input.config_dir)?;
        if h.default_config_dir(self.harnesses.env()).as_deref() == Some(Path::new(&dir)) {
            return Err(AccountError::Conflict(format!(
                "{dir} is {}'s default account",
                h.name()
            )));
        }
        let label = match input
            .label
            .as_deref()
            .map(str::trim)
            .filter(|l| !l.is_empty())
        {
            Some(l) => label(l)?,
            None => Path::new(&dir)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| dir.clone()),
        };
        let pools = pools(&input.pools)?;
        let row = AccountRow {
            id: format!("acc_{}", uuid::Uuid::now_v7().simple()),
            harness: h.id().into(),
            label,
            config_dir: dir,
            created_at: now_rfc3339(),
            team_id: None,
        };
        let (store, r) = (self.store.clone(), row.clone());
        blocking(move || store.insert_account(&r, &pools)).await?;
        self.get(&row.id).await
    }

    /// Adds an account that signs in with `key`, kept in the Keychain.
    async fn create_key(
        &self,
        h: &dyn Harness,
        key: &str,
        input: &CreateAccount,
    ) -> Result<Account> {
        if h.key_env().is_none() {
            return Err(AccountError::Invalid(format!(
                "{} accounts are config directories, not API keys",
                h.name()
            )));
        }
        if !input.config_dir.trim().is_empty() {
            return Err(AccountError::Invalid(
                "give either api_key or config_dir, not both".into(),
            ));
        }
        let key = api_key(key)?;
        let team_id = match input.team_id.as_deref().map(str::trim) {
            Some(t) if !t.is_empty() => {
                if h.team_env().is_none() {
                    return Err(AccountError::Invalid(format!(
                        "{} keys have no team",
                        h.name()
                    )));
                }
                Some(team(t)?)
            }
            _ => None,
        };
        let id = format!("acc_{}", uuid::Uuid::now_v7().simple());
        let label = match input
            .label
            .as_deref()
            .map(str::trim)
            .filter(|l| !l.is_empty())
        {
            Some(l) => label(l)?,
            None => {
                let tail: String = key
                    .chars()
                    .rev()
                    .take(4)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                format!("API key …{tail}")
            }
        };
        let pools = pools(&input.pools)?;
        let service = key_service(&id);
        self.secrets.set(&service, &key).await.map_err(|e| {
            AccountError::Invalid(format!("could not keep the key in the Keychain: {e}"))
        })?;
        let row = AccountRow {
            id: id.clone(),
            harness: h.id().into(),
            label,
            config_dir: format!("{KEYCHAIN_PREFIX}{service}"),
            created_at: now_rfc3339(),
            team_id,
        };
        let (store, r) = (self.store.clone(), row.clone());
        if let Err(e) = blocking(move || store.insert_account(&r, &pools)).await {
            let _ = self.secrets.delete(&service).await;
            return Err(e);
        }
        self.get(&id).await
    }

    /// Renames an added account or replaces any account's pools.
    pub async fn update(&self, id: &str, input: UpdateAccount) -> Result<Account> {
        let account = self.get(id).await?;
        let label = match input.label.as_deref() {
            Some(_) if account.default => {
                return Err(AccountError::Invalid(
                    "a default account's label cannot change".into(),
                ))
            }
            Some(l) => Some(label(l.trim())?),
            None => None,
        };
        let pools = input.pools.as_deref().map(pools).transpose()?;
        let (store, id2) = (self.store.clone(), id.to_string());
        blocking(move || store.update_account(&id2, label.as_deref(), pools.as_deref())).await?;
        self.get(id).await
    }

    /// Removes an added account. Default accounts stay.
    pub async fn delete(&self, id: &str) -> Result<()> {
        let account = self.get(id).await?;
        if account.default {
            return Err(AccountError::Conflict(
                "a harness's default account cannot be removed".into(),
            ));
        }
        let (store, id2) = (self.store.clone(), id.to_string());
        blocking(move || store.delete_account(&id2)).await?;
        if account.api_key {
            if let Err(e) = self.secrets.delete(&key_service(id)).await {
                tracing::warn!(account = %id, error = %e, "could not remove the account's key from the Keychain");
            }
        }
        Ok(())
    }

    /// Chooses the account `holder` starts under with `config` and records
    /// it. Sticky: the holder keeps its account while it is still a launchable
    /// account of the harness and, when `config` names a pool, in that pool.
    /// Balanced: otherwise the pool's ready account with the fewest running
    /// tasks wins, then the one with the most quota left, then the oldest.
    /// Without a pool, the default account. `None` for a harness without
    /// accounts.
    pub async fn lease(&self, holder: &Holder, config: &AgentConfig) -> Result<Option<Lease>> {
        let _guard = self.lease_lock.lock().await;
        let Some(h) = self.harnesses.resolve(&config.harness).cloned() else {
            return Ok(None);
        };
        let accounts: Vec<Account> = self
            .list()
            .await?
            .into_iter()
            .filter(|a| a.harness == h.id())
            .collect();
        let pool = config.pool.as_deref();
        let in_pool = |a: &Account| pool.is_none_or(|p| a.pools.iter().any(|x| x == p));

        let (store, who) = (self.store.clone(), holder.clone());
        let current = blocking(move || match &who {
            Holder::Task(id) => store.task_account(id),
            Holder::Coordinator(id) => store.coordinator_account(id),
        })
        .await?;
        let sticky = current.and_then(|c| {
            accounts
                .iter()
                .find(|a| a.id == c && a.launchable && in_pool(a))
                .cloned()
        });

        let chosen = match (sticky, pool) {
            (Some(a), _) => a.id.clone(),
            (None, None) => default_account_id(h.id()),
            (None, Some(pool)) => {
                let members: Vec<&Account> = accounts.iter().filter(|a| in_pool(a)).collect();
                if members.is_empty() {
                    return Err(AccountError::Invalid(format!(
                        "pool `{pool}` has no {} accounts",
                        h.name()
                    )));
                }
                let ready = least_busy(members.iter().copied().filter(|a| ready(a)));
                let Some(a) = ready else {
                    return Err(AccountError::Conflict(format!(
                        "no account in pool `{pool}` is ready: each is logged out, out of quota, or one the engine cannot launch {} under yet",
                        h.name()
                    )));
                };
                a.id.clone()
            }
        };

        let env = accounts
            .iter()
            .find(|a| a.id == chosen)
            .map(|a| env_of(h.as_ref(), a))
            .unwrap_or_default();
        let (store, who, id) = (self.store.clone(), holder.clone(), chosen.clone());
        blocking(move || match &who {
            Holder::Task(t) => store.set_task_account(t, Some(&id)),
            Holder::Coordinator(p) => store.set_coordinator_account(p, Some(&id)),
        })
        .await?;
        Ok(Some(Lease {
            account_id: chosen,
            env,
        }))
    }

    /// The accounts of `harness` (its id or engine name) as native dispatch
    /// chooses among them, default first. Empty for a harness without
    /// accounts listed. Nothing is leased.
    pub async fn pool_accounts(&self, harness: &str) -> Result<Vec<quark_dispatch::PoolAccount>> {
        let Some(h) = self.harnesses.resolve(harness).cloned() else {
            return Ok(Vec::new());
        };
        Ok(self
            .list()
            .await?
            .into_iter()
            .filter(|a| a.harness == h.id())
            .map(|a| quark_dispatch::PoolAccount {
                ready: ready(&a),
                env: env_of(h.as_ref(), &a).into_iter().collect(),
                id: a.id,
                default: a.default,
                pools: a.pools,
                launchable: a.launchable,
                remaining_percent: a.quota.remaining_percent,
                active_tasks: a.active_tasks,
            })
            .collect())
    }

    /// The account a task's worker moves to after its own reported a rate
    /// limit: the least busy ready account of `harness` in `pool`, or, with
    /// no pool named, in any pool the task's account is in. The task's own
    /// account and those in `exclude` are passed over. `None` when no such
    /// account is left. Nothing is recorded; see
    /// [`Store::record_failover`].
    pub async fn next_account(
        &self,
        task_id: &str,
        harness: &str,
        pool: Option<&str>,
        exclude: &[String],
    ) -> Result<Option<Lease>> {
        let _guard = self.lease_lock.lock().await;
        let Some(h) = self.harnesses.resolve(harness).cloned() else {
            return Ok(None);
        };
        if !h.multi_account() {
            return Ok(None);
        }
        let accounts: Vec<Account> = self
            .list()
            .await?
            .into_iter()
            .filter(|a| a.harness == h.id())
            .collect();
        let (store, id) = (self.store.clone(), task_id.to_string());
        let current = blocking(move || store.task_account(&id))
            .await?
            .unwrap_or_else(|| default_account_id(h.id()));
        let pools: Vec<String> = match pool {
            Some(p) => vec![p.to_string()],
            None => accounts
                .iter()
                .find(|a| a.id == current)
                .map(|a| a.pools.clone())
                .unwrap_or_default(),
        };
        let next = least_busy(accounts.iter().filter(|a| {
            a.id != current
                && !exclude.contains(&a.id)
                && a.pools.iter().any(|p| pools.contains(p))
                && ready(a)
        }));
        Ok(next.map(|a| Lease {
            account_id: a.id.clone(),
            env: env_of(h.as_ref(), a),
        }))
    }

    /// Reads quota for every account of a harness with a quota provider, or
    /// only `only`, one read per account, and stores each reading. Changed readings stream as `account.quota_changed`.
    pub async fn refresh_quota(&self, only: Option<&str>) -> Result<()> {
        for account in self.list().await? {
            if only.is_some_and(|id| id != account.id) {
                continue;
            }
            let Some(h) = self.harnesses.get(&account.harness).cloned() else {
                continue;
            };
            let Some(provider) = h.quota_provider() else {
                continue;
            };
            let var = h.account_env().unwrap_or_default();
            // A single-account harness's login is wherever the harness
            // keeps it, with or without a directory Quark knows.
            let dir = match (&account.config_dir, var.is_empty()) {
                (None, true) => Some(String::new()),
                (dir, _) => dir.clone(),
            };
            let quota = if account.api_key {
                match self.secrets.get(&key_service(&account.id)).await {
                    Ok(key) => match self.quota.read_key(provider, &key).await {
                        Ok(json) => parse_quota(provider, &json),
                        Err(detail) => AccountQuota::empty(QuotaState::Error, Some(detail)),
                    },
                    Err(e) => AccountQuota::empty(QuotaState::Error, Some(e)),
                }
            } else {
                match &dir {
                    None => AccountQuota::empty(
                        QuotaState::Error,
                        Some("the account has no config directory".into()),
                    ),
                    Some(dir) => match self
                        .quota
                        .read(provider, var, Path::new(dir), account.default)
                        .await
                    {
                        Ok(json) => parse_quota(provider, &json),
                        Err(detail) => AccountQuota::empty(QuotaState::Error, Some(detail)),
                    },
                }
            };
            let quota = AccountQuota {
                checked_at: Some(now_rfc3339()),
                ..quota
            };
            let (store, id, harness) = (self.store.clone(), account.id.clone(), account.harness);
            blocking(move || store.set_account_quota(&id, &harness, &quota)).await?;
        }
        Ok(())
    }

    /// Reads quota on start and every `interval` until the task is dropped.
    pub async fn run(self: Arc<Self>, interval: Duration) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if let Err(e) = self.refresh_quota(None).await {
                tracing::warn!(error = %e, "quota refresh failed");
            }
        }
    }
}

/// The environment that starts `h` under account `a`: nothing for the
/// default account, the account variable naming its directory, or a key
/// account's key reference and team.
fn env_of(h: &dyn Harness, a: &Account) -> Vec<(String, String)> {
    account_env(
        h,
        &a.id,
        a.config_dir.as_deref(),
        a.api_key,
        a.team_id.as_deref(),
    )
    .into_iter()
    .filter(|_| !a.default)
    .collect()
}

fn account_env(
    h: &dyn Harness,
    id: &str,
    config_dir: Option<&str>,
    keyed: bool,
    team_id: Option<&str>,
) -> Vec<(String, String)> {
    if keyed {
        let Some(key_env) = h.key_env() else {
            return Vec::new();
        };
        let mut env = vec![quark_supervisor::secrets::reference(
            key_env,
            &key_service(id),
        )];
        if let (Some(var), Some(team)) = (h.team_env(), team_id) {
            env.push((var.into(), team.into()));
        }
        return env;
    }
    match (h.account_env(), config_dir) {
        (Some(var), Some(dir)) => vec![(var.into(), dir.into())],
        _ => Vec::new(),
    }
}

/// An API key as given: trimmed, with no spaces, quotes or backslashes.
fn api_key(raw: &str) -> Result<String> {
    let key = raw.trim();
    if key.is_empty()
        || key.chars().count() > MAX_KEY_CHARS
        || key
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '"' || c == '\\')
    {
        return Err(AccountError::Invalid(
            "api_key must be one word: no spaces, quotes or backslashes".into(),
        ));
    }
    Ok(key.to_string())
}

/// A team id: letters, digits, `-`, `_` and `.`.
fn team(raw: &str) -> Result<String> {
    if raw.len() > 128
        || !raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(AccountError::Invalid(
            "team_id must be letters, digits, dashes, underscores or dots".into(),
        ));
    }
    Ok(raw.to_string())
}

/// Whether an agent can start under the account now: the engine can launch
/// it, it is logged in, and its last quota reading has quota left.
fn ready(a: &Account) -> bool {
    a.launchable && a.health.state != AuthState::NotConfigured && !a.quota.exhausted()
}

/// The account with the fewest running tasks, then the most quota left, then
/// the first listed.
fn least_busy<'a>(accounts: impl Iterator<Item = &'a Account>) -> Option<&'a Account> {
    accounts
        .enumerate()
        .min_by_key(|(i, a)| {
            let left = a.quota.remaining_percent.map_or(-1, |p| (p * 100.0) as i64);
            (a.active_tasks, Reverse(left), *i)
        })
        .map(|(_, a)| a)
}

async fn blocking<T, F>(f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> std::result::Result<T, StoreError> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| AccountError::Store(StoreError::Invalid(e.to_string())))?
        .map_err(AccountError::from)
}

/// An absolute directory path with no trailing slash.
fn config_dir(raw: &str) -> Result<String> {
    let raw = raw.trim();
    let dir = if raw.len() > 1 {
        raw.trim_end_matches('/')
    } else {
        raw
    };
    if dir.is_empty() || !Path::new(dir).is_absolute() || dir.chars().any(char::is_control) {
        return Err(AccountError::Invalid(format!(
            "config_dir must be an absolute directory path, got {raw:?}"
        )));
    }
    if Path::new(dir)
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(AccountError::Invalid(
            "config_dir must not contain `..`".into(),
        ));
    }
    Ok(dir.to_string())
}

fn label(l: &str) -> Result<String> {
    if l.is_empty() || l.chars().count() > MAX_LABEL_CHARS || l.chars().any(char::is_control) {
        return Err(AccountError::Invalid(format!(
            "label must be one line of 1 to {MAX_LABEL_CHARS} characters"
        )));
    }
    Ok(l.to_string())
}

fn pools(raw: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for p in raw {
        let p = p.trim();
        if !pool_name_ok(p) {
            return Err(AccountError::Invalid(format!(
                "pool `{p}` must be lowercase letters, digits and dashes"
            )));
        }
        if !out.iter().any(|x| x == p) {
            out.push(p.to_string());
        }
    }
    out.sort();
    Ok(out)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuotaReport {
    #[serde(default)]
    providers: Vec<ProviderReport>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderReport {
    provider: String,
    plan: Option<String>,
    #[serde(default)]
    windows: Vec<WindowReport>,
    state: Option<StateReport>,
    quota_semantics: Option<Semantics>,
    /// The rest, where a quarkd reading keeps its provider's own detail
    /// ([`quark_dispatch::local`]).
    #[serde(flatten)]
    rest: serde_json::Map<String, serde_json::Value>,
}

/// What a quarkd reading says beside its row: the plan, the period's
/// reset, and why it is not known when it is not.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocalDetail {
    plan: Option<String>,
    allowance_coins: Option<f64>,
    resets_at: Option<String>,
    period_end: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WindowReport {
    id: String,
    label: Option<String>,
    kind: Option<String>,
    resets_at: Option<String>,
    percent_remaining: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StateReport {
    status: Option<String>,
    error: Option<String>,
    /// The command that fixes it, such as a one-time Keychain grant.
    remedy_command: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Semantics {
    #[serde(default)]
    effective_availability: Vec<Availability>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Availability {
    scope: Option<String>,
    status: Option<String>,
    effective_percent_remaining: Option<f64>,
}

/// One account's reading from quota-axi's `--json` report. What limits the
/// account is the effective availability across all models, else the
/// tightest session or weekly window.
pub fn parse_quota(provider: &str, json: &str) -> AccountQuota {
    let report: QuotaReport = match serde_json::from_str(json) {
        Ok(r) => r,
        Err(e) => {
            return AccountQuota::empty(
                QuotaState::Error,
                Some(format!("could not read quota-axi output: {e}")),
            )
        }
    };
    let Some(p) = report
        .providers
        .into_iter()
        .find(|p| p.provider == provider)
    else {
        return AccountQuota::empty(
            QuotaState::Unavailable,
            Some(format!("quota-axi reported nothing for {provider}")),
        );
    };
    let local: LocalDetail = p
        .rest
        .get(provider)
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let effective = p
        .quota_semantics
        .iter()
        .flat_map(|s| &s.effective_availability)
        .find(|a| a.scope.as_deref() == Some("all_models") && a.status.as_deref() == Some("known"))
        .and_then(|a| a.effective_percent_remaining);
    let tightest = |pred: &dyn Fn(&WindowReport) -> bool| {
        p.windows
            .iter()
            .filter(|w| pred(w))
            .filter_map(|w| w.percent_remaining)
            .reduce(f64::min)
    };
    let remaining = effective
        .or_else(|| tightest(&|w| matches!(w.kind.as_deref(), Some("session" | "weekly"))))
        .or_else(|| tightest(&|_| true));
    let mut windows: Vec<QuotaWindow> = p
        .windows
        .iter()
        .map(|w| QuotaWindow {
            id: w.id.clone(),
            label: w.label.clone().unwrap_or_else(|| w.id.clone()),
            percent_remaining: w.percent_remaining,
            resets_at: w.resets_at.clone(),
        })
        .collect();
    // A quarkd reading is one monthly allowance.
    let resets_at = local.resets_at.or(local.period_end);
    if windows.is_empty() && remaining.is_some() && resets_at.is_some() {
        windows.push(QuotaWindow {
            id: "monthly".into(),
            label: "month".into(),
            percent_remaining: remaining,
            resets_at,
        });
    }
    let plan = p.plan.or(local.plan).or_else(|| {
        local
            .allowance_coins
            .map(|c| format!("{} Bobcoins a month", c.round()))
    });
    let error = p.state.as_ref().and_then(|s| {
        let error = s.error.clone()?;
        Some(match &s.remedy_command {
            Some(fix) => format!("{error}; run `{fix}` once"),
            None => error,
        })
    });
    let detail = match remaining {
        Some(_) => error,
        None => error
            .or(local.reason)
            .or_else(|| {
                let status = p.state.as_ref()?.status.clone()?;
                (status != "fresh").then_some(status)
            })
            .or_else(|| Some("quota-axi reported no quota windows".into())),
    };
    AccountQuota {
        state: if remaining.is_some() {
            QuotaState::Known
        } else {
            QuotaState::Unavailable
        },
        remaining_percent: remaining,
        plan,
        windows,
        detail,
        checked_at: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODEX: &str = r#"{"schemaVersion":5,"providers":[{"provider":"codex","plan":"plus",
        "windows":[{"id":"five_hour","label":"session","kind":"session","resetsAt":"2026-10-03T02:22:10.000Z","percentRemaining":80},
                   {"id":"weekly","label":"week","kind":"weekly","resetsAt":"2026-10-09T21:22:10.000Z","percentRemaining":64.5}],
        "state":{"status":"fresh","stale":false},
        "quotaSemantics":{"status":"known","effectiveAvailability":[{"scope":"all_models","status":"known","effectivePercentRemaining":64.5}]}}]}"#;

    #[test]
    fn parses_a_reading() {
        let q = parse_quota("codex", CODEX);
        assert_eq!(q.state, QuotaState::Known);
        assert_eq!(q.remaining_percent, Some(64.5));
        assert_eq!(q.plan.as_deref(), Some("plus"));
        assert_eq!(q.windows.len(), 2);
        assert_eq!(q.windows[0].label, "session");
        assert_eq!(q.detail, None);
    }

    #[test]
    fn falls_back_to_the_tightest_window() {
        let json = CODEX.replace("\"all_models\"", "\"other\"");
        assert_eq!(parse_quota("codex", &json).remaining_percent, Some(64.5));
    }

    #[test]
    fn no_windows_is_unavailable_with_the_reason() {
        let json = r#"{"providers":[{"provider":"claude","windows":[],
            "state":{"status":"auth_required","error":"Claude sign-in required"}}]}"#;
        let q = parse_quota("claude", json);
        assert_eq!(q.state, QuotaState::Unavailable);
        assert_eq!(q.detail.as_deref(), Some("Claude sign-in required"));
        assert_eq!(parse_quota("claude", "not json").state, QuotaState::Error);
    }

    #[test]
    fn failure_detail_is_the_error_line() {
        assert_eq!(
            failure_detail("error: \"unknown argument: --profile-only\"\ncode: X\n"),
            "unknown argument: --profile-only"
        );
    }

    #[test]
    fn a_failed_reading_says_why_and_how_to_fix_it() {
        let q = parse_quota(
            "claude",
            r#"{"schemaVersion":5,"providers":[{"provider":"claude","windows":[],
                "state":{"status":"unavailable","stale":false,"error":"Keychain access required",
                  "reason":"keychain_access_required","remedyCommand":"quota-axi --allow-keychain-prompt"}}]}"#,
        );
        assert_eq!(q.state, QuotaState::Unavailable);
        assert_eq!(
            q.detail.as_deref(),
            Some("Keychain access required; run `quota-axi --allow-keychain-prompt` once")
        );
    }

    #[test]
    fn config_dirs_and_pools_are_checked() {
        assert_eq!(config_dir(" /a/b/ ").unwrap(), "/a/b");
        assert!(config_dir("rel/dir").is_err());
        assert!(config_dir("/a/../b").is_err());
        assert_eq!(
            pools(&["b".into(), "a".into(), "b".into()]).unwrap(),
            ["a", "b"]
        );
        assert!(pools(&["Bad Pool".into()]).is_err());
    }
}
