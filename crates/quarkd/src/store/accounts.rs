//! Accounts, pools and quota readings (ADR-11), and which account each task
//! and coordinator runs under.
//!
//! Accounts are daemon configuration: the engine has no place for them. A
//! harness's default account has no row of its own; it is addressed as
//! `default-<harness>` in pools, quota readings and task records.

use std::collections::HashMap;

use quark_systems::{AccountQuota, AccountQuotaChanged, EventType, Task};
use rusqlite::{params, OptionalExtension, Transaction};

use super::{append_event, get_project, Result, Store, StoreError, TASK_SELECT};

const DEFAULT_PREFIX: &str = "default-";

/// The id of `harness`'s default account.
pub fn default_account_id(harness: &str) -> String {
    format!("{DEFAULT_PREFIX}{harness}")
}

/// The harness of a default account id.
pub fn default_harness(account_id: &str) -> Option<&str> {
    account_id.strip_prefix(DEFAULT_PREFIX)
}

/// An account added through the API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRow {
    pub id: String,
    pub harness: String,
    pub label: String,
    pub config_dir: String,
    pub created_at: String,
}

fn row_from(r: &rusqlite::Row) -> rusqlite::Result<AccountRow> {
    Ok(AccountRow {
        id: r.get(0)?,
        harness: r.get(1)?,
        label: r.get(2)?,
        config_dir: r.get(3)?,
        created_at: r.get(4)?,
    })
}

const ACCOUNT_SELECT: &str = "SELECT id, harness, label, config_dir, created_at FROM accounts";

/// Tasks in these states have finished with their worker.
const IDLE_TASK_STATES: &str = "('done', 'failed')";

impl Store {
    /// Every added account, oldest first.
    pub fn account_rows(&self) -> Result<Vec<AccountRow>> {
        self.read(|c| {
            let mut stmt = c.prepare(&format!("{ACCOUNT_SELECT} ORDER BY created_at, id"))?;
            let rows = stmt.query_map([], row_from)?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })
    }

    pub fn account_row(&self, id: &str) -> Result<AccountRow> {
        self.read(|c| {
            c.query_row(&format!("{ACCOUNT_SELECT} WHERE id = ?1"), [id], row_from)
                .optional()?
                .ok_or(StoreError::NotFound)
        })
    }

    /// Pools by account id, each sorted by name.
    pub fn account_pools(&self) -> Result<HashMap<String, Vec<String>>> {
        self.read(|c| {
            let mut stmt =
                c.prepare("SELECT account_id, pool FROM account_pools ORDER BY account_id, pool")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get(1)?)))?;
            let mut out: HashMap<String, Vec<String>> = HashMap::new();
            for row in rows {
                let (id, pool) = row?;
                out.entry(id).or_default().push(pool);
            }
            Ok(out)
        })
    }

    /// Records a new account. The same directory cannot be added twice for
    /// one harness.
    pub fn insert_account(&self, row: &AccountRow, pools: &[String]) -> Result<()> {
        self.write(|tx, _| {
            let taken: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM accounts WHERE harness = ?1 AND config_dir = ?2)",
                params![row.harness, row.config_dir],
                |r| r.get(0),
            )?;
            if taken {
                return Err(StoreError::Conflict(format!(
                    "{} is already an account for {}",
                    row.config_dir, row.harness
                )));
            }
            tx.execute(
                "INSERT INTO accounts (id, harness, label, config_dir, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    row.id,
                    row.harness,
                    row.label,
                    row.config_dir,
                    row.created_at
                ],
            )?;
            set_pools(tx, &row.id, pools)
        })
    }

    /// Renames an added account and/or replaces any account's pools.
    pub fn update_account(
        &self,
        id: &str,
        label: Option<&str>,
        pools: Option<&[String]>,
    ) -> Result<()> {
        self.write(|tx, _| {
            if let Some(label) = label {
                let n = tx.execute(
                    "UPDATE accounts SET label = ?2 WHERE id = ?1",
                    params![id, label],
                )?;
                if n == 0 {
                    return Err(StoreError::NotFound);
                }
            }
            if let Some(pools) = pools {
                set_pools(tx, id, pools)?;
            }
            Ok(())
        })
    }

    /// Removes an added account with its pools and quota reading. Refused
    /// while a task or coordinator runs under it; finished tasks keep the
    /// id as history.
    pub fn delete_account(&self, id: &str) -> Result<()> {
        self.write(|tx, _| {
            let in_use = active_leases(tx)?.get(id).copied().unwrap_or(0);
            if in_use > 0 {
                return Err(StoreError::Conflict(format!(
                    "the account is in use by {in_use} running task(s)"
                )));
            }
            if tx.execute("DELETE FROM accounts WHERE id = ?1", [id])? == 0 {
                return Err(StoreError::NotFound);
            }
            tx.execute("DELETE FROM account_pools WHERE account_id = ?1", [id])?;
            tx.execute("DELETE FROM account_quota WHERE account_id = ?1", [id])?;
            Ok(())
        })
    }

    /// Running tasks and coordinators per account id.
    pub fn active_leases(&self) -> Result<HashMap<String, u32>> {
        self.read(active_leases)
    }

    /// The latest quota reading per account id.
    pub fn account_quotas(&self) -> Result<HashMap<String, AccountQuota>> {
        self.read(|c| {
            let mut stmt = c.prepare("SELECT account_id, quota FROM account_quota")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut out = HashMap::new();
            for row in rows {
                let (id, json) = row?;
                out.insert(id, serde_json::from_str(&json)?);
            }
            Ok(out)
        })
    }

    /// Stores a quota reading. When it differs from the last one in more than
    /// its time, emits `account.quota_changed` and returns true.
    pub fn set_account_quota(&self, id: &str, harness: &str, quota: &AccountQuota) -> Result<bool> {
        self.write(|tx, events| {
            let old: Option<String> = tx
                .query_row(
                    "SELECT quota FROM account_quota WHERE account_id = ?1",
                    [id],
                    |r| r.get(0),
                )
                .optional()?;
            let old: Option<AccountQuota> = old.map(|j| serde_json::from_str(&j)).transpose()?;
            tx.execute(
                "INSERT INTO account_quota (account_id, quota) VALUES (?1, ?2)
                 ON CONFLICT (account_id) DO UPDATE SET quota = excluded.quota",
                params![id, serde_json::to_string(quota)?],
            )?;
            let same = old.is_some_and(|o| {
                AccountQuota {
                    checked_at: None,
                    ..o
                } == AccountQuota {
                    checked_at: None,
                    ..quota.clone()
                }
            });
            if same {
                return Ok(false);
            }
            let payload = AccountQuotaChanged {
                account_id: id.to_string(),
                harness: harness.to_string(),
                quota: quota.clone(),
            };
            append_event(
                tx,
                events,
                None,
                EventType::AccountQuotaChanged,
                serde_json::to_value(payload)?,
            )?;
            Ok(true)
        })
    }

    /// The account a task's worker was started under.
    pub fn task_account(&self, task_id: &str) -> Result<Option<String>> {
        self.read(|c| {
            c.query_row(
                "SELECT account_id FROM tasks WHERE id = ?1",
                [task_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(StoreError::NotFound)
        })
    }

    /// Records that a task's worker now runs under `account_id`, emitting
    /// `task.state_changed` when it moved.
    pub fn set_task_account(&self, task_id: &str, account_id: Option<&str>) -> Result<()> {
        self.write(|tx, events| {
            let old: Task = tx
                .query_row(
                    &format!("{TASK_SELECT} WHERE id = ?1"),
                    [task_id],
                    super::task_from_row,
                )
                .optional()?
                .ok_or(StoreError::NotFound)?;
            if old.account_id.as_deref() == account_id {
                return Ok(());
            }
            let task = Task {
                account_id: account_id.map(str::to_string),
                updated_at: crate::now_rfc3339(),
                ..old.clone()
            };
            tx.execute(
                "UPDATE tasks SET account_id = ?2, updated_at = ?3 WHERE id = ?1",
                params![task.id, task.account_id, task.updated_at],
            )?;
            append_event(
                tx,
                events,
                Some(&task.project_id),
                EventType::TaskStateChanged,
                serde_json::json!({ "task": task, "previous_state": old.state }),
            )
        })
    }

    /// The account a Project's coordinator was started under.
    pub fn coordinator_account(&self, project_id: &str) -> Result<Option<String>> {
        self.read(|c| {
            get_project(c, project_id)?;
            Ok(c.query_row(
                "SELECT coordinator_account_id FROM projects WHERE id = ?1",
                [project_id],
                |r| r.get(0),
            )?)
        })
    }

    pub fn set_coordinator_account(
        &self,
        project_id: &str,
        account_id: Option<&str>,
    ) -> Result<()> {
        self.write(|tx, _| {
            let n = tx.execute(
                "UPDATE projects SET coordinator_account_id = ?2 WHERE id = ?1",
                params![project_id, account_id],
            )?;
            if n == 0 {
                return Err(StoreError::NotFound);
            }
            Ok(())
        })
    }
}

fn set_pools(tx: &Transaction, id: &str, pools: &[String]) -> Result<()> {
    tx.execute("DELETE FROM account_pools WHERE account_id = ?1", [id])?;
    for pool in pools {
        tx.execute(
            "INSERT OR IGNORE INTO account_pools (account_id, pool) VALUES (?1, ?2)",
            params![id, pool],
        )?;
    }
    Ok(())
}

fn active_leases(c: &rusqlite::Connection) -> Result<HashMap<String, u32>> {
    let mut stmt = c.prepare(&format!(
        "SELECT account_id, COUNT(*) FROM (
             SELECT account_id FROM tasks
             WHERE account_id IS NOT NULL AND state NOT IN {IDLE_TASK_STATES}
             UNION ALL
             SELECT coordinator_account_id FROM projects
             WHERE coordinator_account_id IS NOT NULL AND status != 'failed'
         ) GROUP BY account_id"
    ))?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// The account a new worker started under. The engine carries the
/// coordinator's account onto workers of the same harness; a worker of
/// another harness runs under that harness's default account. `None` for a
/// harness without accounts.
pub(super) fn inherited_account(
    tx: &Transaction,
    project_id: &str,
    reported_harness: &str,
) -> Option<String> {
    let manifest = quark_harness::ManifestRegistry::builtin()
        .resolve(reported_harness)
        .cloned()?;
    manifest.account.as_ref()?.env.as_ref()?;
    let coordinator: Option<String> = tx
        .query_row(
            "SELECT coordinator_account_id FROM projects WHERE id = ?1",
            [project_id],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    if let Some(account) = coordinator {
        let harness = match default_harness(&account) {
            Some(h) => Some(h.to_string()),
            None => tx
                .query_row(
                    "SELECT harness FROM accounts WHERE id = ?1",
                    [&account],
                    |r| r.get::<_, String>(0),
                )
                .ok(),
        };
        if harness.as_deref() == Some(manifest.id.as_str()) {
            return Some(account);
        }
    }
    Some(default_account_id(&manifest.id))
}
