//! Account pools: which of a harness's accounts a worker runs under.
//!
//! A profile naming a pool runs under one of the pool's accounts for its
//! harness; without a pool, under the harness's default account. The rules
//! are quarkd's (`accounts.rs`), as pure functions the native dispatcher
//! calls:
//!
//! - **Sticky:** a task keeps its account while that account is still
//!   launchable and, when a pool is named, in the pool.
//! - **Balanced:** otherwise the pool's ready account with the fewest
//!   running tasks wins, then the one with the most quota left, then the
//!   first listed.
//! - **Failover:** after a rate limit, the least busy ready account in the
//!   same pool other than the current one and any already tried.

use std::cmp::Reverse;
use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// One account as pool selection sees it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PoolAccount {
    pub id: String,
    pub default: bool,
    pub pools: Vec<String>,
    /// The engine can start the harness under it.
    pub launchable: bool,
    /// Launchable, logged in, and its last quota reading has quota left.
    pub ready: bool,
    pub remaining_percent: Option<f64>,
    pub active_tasks: u32,
    /// What a worker under it is started with, such as its config
    /// directory variable.
    pub env: BTreeMap<String, String>,
}

impl PoolAccount {
    fn in_pool(&self, pool: Option<&str>) -> bool {
        pool.is_none_or(|p| self.pools.iter().any(|x| x == p))
    }
}

/// Why no account could be chosen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PoolError {
    /// The pool has no accounts of the harness.
    Empty { pool: String },
    /// Every account in the pool is logged out, out of quota, or not
    /// launchable.
    NoneReady { pool: String },
}

impl std::fmt::Display for PoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PoolError::Empty { pool } => write!(f, "pool `{pool}` has no accounts for this harness"),
            PoolError::NoneReady { pool } => write!(
                f,
                "no account in pool `{pool}` is ready: each is logged out, out of quota, or not launchable"
            ),
        }
    }
}

/// The account a task starts under. `accounts` are the harness's; `current`
/// is the task's account from an earlier generation. `Ok(None)` when no
/// pool is named and the harness has no default account listed.
pub fn choose<'a>(
    accounts: &'a [PoolAccount],
    pool: Option<&str>,
    current: Option<&str>,
) -> Result<Option<&'a PoolAccount>, PoolError> {
    if let Some(a) = current.and_then(|c| {
        accounts
            .iter()
            .find(|a| a.id == c && a.launchable && a.in_pool(pool))
    }) {
        return Ok(Some(a));
    }
    let Some(pool) = pool else {
        return Ok(accounts.iter().find(|a| a.default));
    };
    let members: Vec<&PoolAccount> = accounts.iter().filter(|a| a.in_pool(Some(pool))).collect();
    if members.is_empty() {
        return Err(PoolError::Empty { pool: pool.into() });
    }
    least_busy(members.into_iter().filter(|a| a.ready))
        .map(Some)
        .ok_or_else(|| PoolError::NoneReady { pool: pool.into() })
}

/// The account a worker moves to after its own hit a rate limit: in `pool`,
/// or with none named in any pool `current` belongs to.
pub fn next<'a>(
    accounts: &'a [PoolAccount],
    pool: Option<&str>,
    current: &str,
    exclude: &[String],
) -> Option<&'a PoolAccount> {
    let pools: Vec<String> = match pool {
        Some(p) => vec![p.to_string()],
        None => accounts
            .iter()
            .find(|a| a.id == current)
            .map(|a| a.pools.clone())
            .unwrap_or_default(),
    };
    least_busy(accounts.iter().filter(|a| {
        a.id != current
            && !exclude.contains(&a.id)
            && a.pools.iter().any(|p| pools.contains(p))
            && a.ready
    }))
}

fn least_busy<'a>(accounts: impl Iterator<Item = &'a PoolAccount>) -> Option<&'a PoolAccount> {
    accounts
        .enumerate()
        .min_by_key(|(i, a)| {
            let left = a.remaining_percent.map_or(-1, |p| (p * 100.0) as i64);
            (a.active_tasks, Reverse(left), *i)
        })
        .map(|(_, a)| a)
}

/// Where the dispatcher reads a harness's accounts.
#[async_trait]
pub trait Accounts: Send + Sync {
    /// The harness's accounts, default first. Empty for a harness without
    /// accounts.
    async fn accounts(&self, harness: &str) -> Result<Vec<PoolAccount>, String>;
}

/// No accounts anywhere: every worker runs under its harness's own
/// configuration.
#[derive(Debug, Default, Clone)]
pub struct NoAccounts;

#[async_trait]
impl Accounts for NoAccounts {
    async fn accounts(&self, _: &str) -> Result<Vec<PoolAccount>, String> {
        Ok(Vec::new())
    }
}

/// A fixed list per harness.
#[derive(Debug, Default, Clone)]
pub struct StaticAccounts(pub BTreeMap<String, Vec<PoolAccount>>);

#[async_trait]
impl Accounts for StaticAccounts {
    async fn accounts(&self, harness: &str) -> Result<Vec<PoolAccount>, String> {
        Ok(self.0.get(harness).cloned().unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acc(id: &str, pools: &[&str], ready: bool, active: u32, left: Option<f64>) -> PoolAccount {
        PoolAccount {
            id: id.into(),
            default: id == "default",
            pools: pools.iter().map(|p| p.to_string()).collect(),
            launchable: true,
            ready,
            remaining_percent: left,
            active_tasks: active,
            env: BTreeMap::new(),
        }
    }

    #[test]
    fn balanced_then_sticky() {
        let a = vec![
            acc("default", &[], true, 0, None),
            acc("a", &["team"], true, 2, Some(90.0)),
            acc("b", &["team"], true, 1, Some(10.0)),
            acc("c", &["team"], true, 1, Some(50.0)),
            acc("d", &["team"], false, 0, Some(99.0)),
        ];
        assert_eq!(choose(&a, Some("team"), None).unwrap().unwrap().id, "c");
        assert_eq!(
            choose(&a, Some("team"), Some("a")).unwrap().unwrap().id,
            "a"
        );
        // Sticky only inside the pool.
        assert_eq!(
            choose(&a, Some("team"), Some("default"))
                .unwrap()
                .unwrap()
                .id,
            "c"
        );
        assert_eq!(choose(&a, None, None).unwrap().unwrap().id, "default");
        assert_eq!(
            choose(&a, Some("other"), None).unwrap_err(),
            PoolError::Empty {
                pool: "other".into()
            }
        );
        let tired = vec![acc("d", &["team"], false, 0, None)];
        assert!(matches!(
            choose(&tired, Some("team"), None),
            Err(PoolError::NoneReady { .. })
        ));
    }

    #[test]
    fn failover_stays_in_the_pool() {
        let a = vec![
            acc("a", &["team"], true, 0, Some(90.0)),
            acc("b", &["team"], true, 0, Some(20.0)),
            acc("x", &["other"], true, 0, Some(99.0)),
        ];
        assert_eq!(next(&a, None, "a", &[]).unwrap().id, "b");
        assert!(next(&a, None, "a", &["b".into()]).is_none());
        assert_eq!(next(&a, Some("other"), "a", &[]).unwrap().id, "x");
    }
}
