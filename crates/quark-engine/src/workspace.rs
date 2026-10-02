use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// One firstmate home on this machine, plus the engine checkout whose scripts
/// operate on it. The spec calls a firstmate home a workspace; a later cloud
/// runtime becomes another location behind the same type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    /// The `FM_HOME` holding `data/`, `state/`, `config/` and `projects/`.
    pub home: PathBuf,
    /// The pinned firstmate checkout whose `bin/` holds the engine scripts.
    pub engine_root: PathBuf,
    /// Extra environment for every script run against this workspace, such
    /// as the `TMUX` socket the engine should open its windows on.
    pub env: Vec<(String, String)>,
}

impl Workspace {
    pub fn new(home: impl Into<PathBuf>, engine_root: impl Into<PathBuf>) -> Self {
        Self {
            home: home.into(),
            engine_root: engine_root.into(),
            env: Vec::new(),
        }
    }

    /// Adds `key=value` to every script run against this workspace.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn state_dir(&self) -> PathBuf {
        self.home.join("state")
    }

    pub fn data_dir(&self) -> PathBuf {
        self.home.join("data")
    }

    pub fn engine_bin(&self) -> PathBuf {
        self.engine_root.join("bin")
    }

    pub fn home_summary_path(&self) -> PathBuf {
        self.state_dir().join("home-summary.json")
    }

    pub fn status_log_path(&self, task_id: &str) -> Result<PathBuf> {
        self.task_file(task_id, "status")
    }

    pub fn pr_poll_path(&self, task_id: &str) -> Result<PathBuf> {
        self.task_file(task_id, "pr-poll")
    }

    pub fn merge_notified_path(&self, task_id: &str) -> Result<PathBuf> {
        self.task_file(task_id, "pr-poll-merge-notified")
    }

    /// The task's metadata record, rewritten on every spawn and relaunch.
    pub fn meta_path(&self, task_id: &str) -> Result<PathBuf> {
        self.task_file(task_id, "meta")
    }

    /// The brief a task's worker was started from.
    pub fn brief_path(&self, task_id: &str) -> Result<PathBuf> {
        validate_task_id(task_id)?;
        Ok(self.data_dir().join(task_id).join("brief.md"))
    }

    /// The gate runner's manifest for a task (`quark.gates.v1`).
    pub fn gates_manifest_path(&self, task_id: &str) -> Result<PathBuf> {
        self.task_file(task_id, "gates.json")
    }

    /// Where a task's gate artifacts live; manifest paths are relative to it.
    pub fn gates_dir(&self, task_id: &str) -> Result<PathBuf> {
        self.task_file(task_id, "gates")
    }

    fn task_file(&self, task_id: &str, suffix: &str) -> Result<PathBuf> {
        validate_task_id(task_id)?;
        Ok(self.state_dir().join(format!("{task_id}.{suffix}")))
    }
}

/// Same rule as the engine's `fm_task_id_path_safe`: nonempty, no leading dot,
/// and only `A-Za-z0-9._-`, so an id can never escape `state/`.
pub fn validate_task_id(id: &str) -> Result<()> {
    let ok = !id.is_empty()
        && !id.starts_with('.')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(Error::InvalidTaskId(id.to_string()))
    }
}

pub(crate) fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_ids_follow_engine_rule() {
        for ok in ["ship-task", "a.b_c-1", "X"] {
            assert!(validate_task_id(ok).is_ok(), "{ok}");
        }
        for bad in ["", ".hidden", "..", "a/b", "../x", "a b", "é"] {
            assert!(validate_task_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn task_paths_stay_in_state() {
        let ws = Workspace::new("/h", "/e");
        assert_eq!(
            ws.status_log_path("t1").unwrap(),
            PathBuf::from("/h/state/t1.status")
        );
        assert!(ws.pr_poll_path("../t1").is_err());
    }
}
