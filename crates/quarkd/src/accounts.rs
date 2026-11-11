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
//! Quota comes from quarkd's own readers ([`quark_dispatch::local`]: Claude
//! Code, Codex, IBM Bob, Kiro CLI), one read per account with its config
//! directory (the daemon's own login for the default account), and changes
//! stream as `account.quota_changed`. Claude Code on macOS keeps each
//! login in its own Keychain item; should one want a prompt, background
//! reads give up on it and the account offers a grant
//! ([`Accounts::grant_keychain`]), a read that waits for the person.

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
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

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
    /// The provider's schema-5 report for the account whose config
    /// directory is `config_dir` (the daemon's own login for the `default`
    /// account, whose variable is left as it is, since setting it changes
    /// which Keychain item Claude Code uses).
    async fn read(
        &self,
        provider: &str,
        env_var: &str,
        config_dir: &Path,
        default: bool,
    ) -> std::result::Result<String, String>;

    /// [`Self::read`], waiting on a macOS Keychain prompt for the login's
    /// item instead of giving up on it. Once allowed ("Always Allow"),
    /// later reads of that item need no prompt.
    async fn grant(
        &self,
        provider: &str,
        env_var: &str,
        config_dir: &Path,
        default: bool,
    ) -> std::result::Result<String, String> {
        self.read(provider, env_var, config_dir, default).await
    }

    /// The report for an account that signs in with `key`.
    async fn read_key(&self, provider: &str, _key: &str) -> std::result::Result<String, String> {
        Err(format!("no per-key quota reading for {provider}"))
    }
}

/// quarkd's own quota readers, one per provider.
#[derive(Clone, Default)]
pub struct LocalReaders {
    pub readers: Vec<Arc<dyn LocalReading>>,
}

impl std::fmt::Debug for LocalReaders {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let readers: Vec<&str> = self.readers.iter().map(|r| r.provider()).collect();
        f.debug_struct("LocalReaders")
            .field("readers", &readers)
            .finish()
    }
}

impl LocalReaders {
    fn reader(&self, provider: &str) -> std::result::Result<&Arc<dyn LocalReading>, String> {
        self.readers
            .iter()
            .find(|r| r.provider() == provider)
            .ok_or_else(|| format!("Quark has no quota reader for {provider}"))
    }

    async fn run(
        &self,
        provider: &str,
        config_dir: &Path,
        default: bool,
        grant: bool,
    ) -> std::result::Result<String, String> {
        let r = self.reader(provider)?;
        let dir = (!default && !config_dir.as_os_str().is_empty()).then_some(config_dir);
        let read = tokio::time::timeout(QUOTA_TIMEOUT, r.read_account(dir, grant))
            .await
            .map_err(|_| {
                format!(
                    "the {provider} quota read timed out after {}s",
                    QUOTA_TIMEOUT.as_secs()
                )
            })?;
        read.map(|v| v.to_string())
            .ok_or_else(|| format!("{provider} has no quota reading for added accounts"))
    }
}

#[async_trait]
impl QuotaReader for LocalReaders {
    async fn read(
        &self,
        provider: &str,
        _env_var: &str,
        config_dir: &Path,
        default: bool,
    ) -> std::result::Result<String, String> {
        self.run(provider, config_dir, default, false).await
    }

    async fn grant(
        &self,
        provider: &str,
        _env_var: &str,
        config_dir: &Path,
        default: bool,
    ) -> std::result::Result<String, String> {
        self.run(provider, config_dir, default, true).await
    }

    async fn read_key(&self, provider: &str, key: &str) -> std::result::Result<String, String> {
        match self.reader(provider)?.read_with_key(key).await {
            Some(v) => Ok(v.to_string()),
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

    /// What a read of `config_dir` returns: a schema-5 report, or a failure.
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
        let (rows, pools, quotas, leases, kept) = tokio::task::spawn_blocking(move || {
            Ok::<_, StoreError>((
                store.account_rows()?,
                store.account_pools()?,
                store.account_quotas()?,
                store.active_leases()?,
                store.default_account_keys()?,
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
            // A kept key signs the default login in wherever quarkd runs.
            let keyed = h.key_env().is_some() && kept.contains(&default_id);
            let (health, launchable) = if keyed {
                (
                    kept_key_health(),
                    account_env(h.as_ref(), &default_id, None, true, None)
                        .iter()
                        .all(|(v, _)| self.forwarded.contains(&v.as_str())),
                )
            } else {
                (h.auth_status(env, &harness::Account::default()), true)
            };
            out.push(Account {
                health,
                quota: quota_for(&default_id),
                pools: pools.get(&default_id).cloned().unwrap_or_default(),
                active_tasks: leases.get(&default_id).copied().unwrap_or(0),
                id: default_id,
                harness: h.id().into(),
                label: "Default".into(),
                config_dir: h.default_config_dir(env).map(|d| d.display().to_string()),
                api_key: keyed,
                team_id: None,
                default: true,
                launchable,
                created_at: None,
            });
            for r in added {
                let keyed = r.key_service().is_some();
                let (config_dir, health, launchable) = if keyed {
                    (
                        None,
                        kept_key_health(),
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

    /// Keeps `key` in the Keychain as default account `id`'s API key, so
    /// its launches and quota reads sign in with it rather than with
    /// whatever quarkd's own environment has (a quarkd launchd starts has
    /// no shell profile). Replaces an earlier kept key.
    pub async fn set_default_key(&self, id: &str, key: &str) -> Result<Account> {
        let account = self.get(id).await?;
        let h = self.default_key_harness(&account)?;
        let key = api_key(key)?;
        self.secrets
            .set(&key_service(id), &key)
            .await
            .map_err(|e| {
                AccountError::Invalid(format!("could not keep the key in the Keychain: {e}"))
            })?;
        let (store, id2, at) = (self.store.clone(), id.to_string(), now_rfc3339());
        blocking(move || store.set_default_account_key(&id2, true, &at)).await?;
        tracing::info!(account = %id, harness = %h, "kept an API key for the default account");
        self.get(id).await
    }

    /// Forgets default account `id`'s kept key: it signs in from quarkd's
    /// environment again.
    pub async fn clear_default_key(&self, id: &str) -> Result<Account> {
        let account = self.get(id).await?;
        self.default_key_harness(&account)?;
        let (store, id2, at) = (self.store.clone(), id.to_string(), now_rfc3339());
        blocking(move || store.set_default_account_key(&id2, false, &at)).await?;
        if let Err(e) = self.secrets.delete(&key_service(id)).await {
            tracing::warn!(account = %id, error = %e, "could not remove the account's key from the Keychain");
        }
        self.get(id).await
    }

    /// The harness of a default account that can keep a key.
    fn default_key_harness(&self, account: &Account) -> Result<String> {
        if !account.default {
            return Err(AccountError::Invalid(
                "only a default account takes a kept key; an added key account is its key".into(),
            ));
        }
        let h = self
            .harnesses
            .get(&account.harness)
            .ok_or(AccountError::NotFound)?;
        if h.key_env().is_none() {
            return Err(AccountError::Invalid(format!(
                "{} does not sign in with an API key",
                h.name()
            )));
        }
        Ok(h.id().to_string())
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
        self.read_quota(only, false).await
    }

    /// Reads `id`'s quota waiting on a Keychain prompt for its login, so
    /// later reads can use it, and returns the account with that reading.
    pub async fn grant_keychain(&self, id: &str) -> Result<Account> {
        let account = self.get(id).await?;
        if account.api_key {
            return Err(AccountError::Invalid(
                "a key account's quota needs no Keychain grant".into(),
            ));
        }
        self.read_quota(Some(id), true).await?;
        self.get(id).await
    }

    async fn read_quota(&self, only: Option<&str>, grant: bool) -> Result<()> {
        // Accounts are read side by side: one slow reader (Kiro's /usage
        // with an API key takes tens of seconds) must not hold up the rest,
        // nor an API call waiting on all of them.
        let reads = self
            .list()
            .await?
            .into_iter()
            .filter(|a| only.is_none_or(|id| id == a.id))
            .map(|a| self.read_account_quota(a, grant));
        for r in futures_util::future::join_all(reads).await {
            r?;
        }
        Ok(())
    }

    async fn read_account_quota(&self, account: Account, grant: bool) -> Result<()> {
        let Some(h) = self.harnesses.get(&account.harness).cloned() else {
            return Ok(());
        };
        let Some(provider) = h.quota_provider() else {
            return Ok(());
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
                Some(dir) => {
                    let path = Path::new(dir);
                    let read = if grant {
                        self.quota.grant(provider, var, path, account.default).await
                    } else {
                        self.quota.read(provider, var, path, account.default).await
                    };
                    match read {
                        Ok(json) => parse_quota(provider, &json),
                        Err(detail) => AccountQuota::empty(QuotaState::Error, Some(detail)),
                    }
                }
            }
        };
        let quota = AccountQuota {
            checked_at: Some(now_rfc3339()),
            ..quota
        };
        let quota = keep_last_reading(&account.quota, quota, OffsetDateTime::now_utc());
        let (store, id, harness) = (self.store.clone(), account.id.clone(), account.harness);
        blocking(move || store.set_account_quota(&id, &harness, &quota)).await?;
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
    .filter(|_| !a.default || a.api_key)
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

/// How long a reading stands in for reads that did not finish.
const KEEP_READING_FOR: time::Duration = time::Duration::hours(6);

/// `new`, or when its read did not finish (a reader that timed out or could
/// not run) the last reading with numbers, if under [`KEEP_READING_FOR`]
/// old, noting the failed read. A read that answered, such as a lapsed
/// login, always replaces it.
fn keep_last_reading(last: &AccountQuota, new: AccountQuota, now: OffsetDateTime) -> AccountQuota {
    let unfinished = new.state == QuotaState::Error
        || new
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("did not answer within") || d.contains("timed out"));
    let fresh = last
        .checked_at
        .as_deref()
        .and_then(|t| OffsetDateTime::parse(t, &Rfc3339).ok())
        .is_some_and(|t| now - t < KEEP_READING_FOR);
    if new.state == QuotaState::Known || !unfinished || last.state != QuotaState::Known || !fresh {
        return new;
    }
    let failed = new.detail.as_deref().unwrap_or("the read failed");
    // Never stack one note on another.
    let note = format!("Showing the last reading; the latest read failed: {failed}");
    AccountQuota {
        detail: Some(note.chars().take(QUOTA_DETAIL_CHARS).collect()),
        ..last.clone()
    }
}

/// The health of an account that signs in with a key Quark keeps.
fn kept_key_health() -> quark_systems::HarnessAuth {
    quark_systems::HarnessAuth {
        state: AuthState::Configured,
        detail: "API key kept in the Keychain".into(),
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
    /// Bob's coins spent this month.
    estimated_used_coins: Option<f64>,
    /// Kiro's credits spent and covered this month.
    used_credits: Option<f64>,
    plan_credits: Option<f64>,
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
    /// Why, e.g. `keychain_access_required`.
    reason: Option<String>,
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

/// One account's reading from a schema-5 report. What limits the account
/// is the effective availability across all models, else the tightest
/// session or weekly window.
pub fn parse_quota(provider: &str, json: &str) -> AccountQuota {
    let report: QuotaReport = match serde_json::from_str(json) {
        Ok(r) => r,
        Err(e) => {
            return AccountQuota::empty(
                QuotaState::Error,
                Some(format!("could not read the quota report: {e}")),
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
            Some(format!("the reader reported nothing for {provider}")),
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
            used: None,
            limit: None,
            unit: None,
            overage: None,
        })
        .collect();
    // A quarkd reading is one monthly allowance.
    let resets_at = local.resets_at.or(local.period_end);
    if windows.is_empty() && remaining.is_some() && resets_at.is_some() {
        let (used, limit, unit) = match (local.used_credits, local.plan_credits) {
            (Some(u), Some(l)) => (Some(u), Some(l), Some("credits")),
            _ => match (local.estimated_used_coins, local.allowance_coins) {
                (Some(u), Some(l)) => (Some(u), Some(l), Some("Bobcoins")),
                _ => (None, None, None),
            },
        };
        windows.push(QuotaWindow {
            id: "monthly".into(),
            label: "month".into(),
            percent_remaining: remaining,
            resets_at,
            used,
            limit,
            unit: unit.map(Into::into),
            overage: used.zip(limit).map(|(u, l)| u - l).filter(|o| *o > 0.0),
        });
    }
    let plan = p.plan.or(local.plan).or_else(|| {
        local
            .allowance_coins
            .map(|c| format!("{} Bobcoins a month", c.round()))
    });
    let keychain_grant = p
        .state
        .as_ref()
        .is_some_and(|s| s.reason.as_deref() == Some("keychain_access_required"));
    let error = p.state.as_ref().and_then(|s| {
        let error = s.error.clone()?;
        Some(match s.remedy_command.as_deref() {
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
            .or_else(|| Some("no quota windows reported".into())),
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
        keychain_grant: keychain_grant && remaining.is_none(),
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
    fn bob_and_kiro_months_say_how_much_is_used() {
        let kiro = r#"{"schemaVersion":5,"providers":[{"provider":"kiro","state":{"status":"fresh"},
            "quotaSemantics":{"status":"known","effectiveAvailability":[{"scope":"all_models","status":"known","effectivePercentRemaining":99.63}]},
            "kiro":{"usedCredits":3.69,"planCredits":1000,"plan":"KIRO PRO","resetsAt":"2026-11-01T00:00:00Z"}}]}"#;
        let w = &parse_quota("kiro", kiro).windows[0];
        assert_eq!((w.used, w.limit), (Some(3.69), Some(1000.0)));
        assert_eq!(w.unit.as_deref(), Some("credits"));
        assert_eq!(w.overage, None);
        let over = kiro.replace("3.69", "1100");
        assert_eq!(parse_quota("kiro", &over).windows[0].overage, Some(100.0));

        let bob = r#"{"schemaVersion":5,"providers":[{"provider":"bob","state":{"status":"fresh"},
            "quotaSemantics":{"status":"known","effectiveAvailability":[{"scope":"all_models","status":"known","effectivePercentRemaining":75}]},
            "bob":{"estimatedUsedCoins":375,"allowanceCoins":1500,"periodEnd":"2026-11-01T00:00:00Z"}}]}"#;
        let q = parse_quota("bob", bob);
        let w = &q.windows[0];
        assert_eq!((w.used, w.limit), (Some(375.0), Some(1500.0)));
        assert_eq!(w.unit.as_deref(), Some("Bobcoins"));
        assert_eq!(q.plan.as_deref(), Some("1500 Bobcoins a month"));

        assert_eq!(parse_quota("codex", CODEX).windows[0].used, None);
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
    fn a_withheld_keychain_offers_a_grant() {
        let q = parse_quota(
            "claude",
            r#"{"schemaVersion":5,"providers":[{"provider":"claude","windows":[],
                "state":{"status":"unavailable","stale":false,"error":"Keychain access needed",
                  "reason":"keychain_access_required"}}]}"#,
        );
        assert_eq!(q.state, QuotaState::Unavailable);
        assert_eq!(q.detail.as_deref(), Some("Keychain access needed"));
        assert!(q.keychain_grant);
    }

    struct Seen(std::sync::Mutex<Vec<(Option<PathBuf>, bool)>>);

    #[async_trait]
    impl LocalReading for Seen {
        fn provider(&self) -> &str {
            "claude"
        }
        async fn read(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn read_account(
            &self,
            config_dir: Option<&Path>,
            grant: bool,
        ) -> Option<serde_json::Value> {
            self.0
                .lock()
                .unwrap()
                .push((config_dir.map(Path::to_path_buf), grant));
            Some(serde_json::json!({"ok": true}))
        }
    }

    #[tokio::test]
    async fn each_account_is_read_from_its_own_login() {
        let seen = Arc::new(Seen(Default::default()));
        let q = LocalReaders {
            readers: vec![seen.clone()],
        };
        let work = Path::new("/Users/m/.claude-work");
        q.read("claude", "CLAUDE_CONFIG_DIR", work, false)
            .await
            .unwrap();
        q.grant("claude", "CLAUDE_CONFIG_DIR", work, false)
            .await
            .unwrap();
        q.read("claude", "CLAUDE_CONFIG_DIR", Path::new("/ignored"), true)
            .await
            .unwrap();
        assert!(q.read("grok", "", work, false).await.is_err());
        assert_eq!(
            *seen.0.lock().unwrap(),
            [
                (Some(work.to_path_buf()), false),
                (Some(work.to_path_buf()), true),
                (None, false)
            ]
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
