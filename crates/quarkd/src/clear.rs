//! Clearing finished workers.
//!
//! A done or failed task's worker leaves things behind: its session (tmux
//! or PTY) when the agent never exited, and its worktree. Clearing stops the
//! session, gives the worktree back through the engine's worktree provider
//! and takes the task off the board. The task's row, activity log and events
//! are kept, so its history still reads. A worktree holding work that has
//! not landed is never discarded: the task stays on the board and the
//! caller is told why.
//!
//! The API (`POST /v1/tasks/{id}:clear`, `POST /v1/projects/{id}/tasks:clear`)
//! and the coordinator's `clear` tool both come here.

use std::sync::Arc;

use quark_systems::{Task, TaskState};

use crate::engine::{EngineAdapter, EngineError, TaskControl, WorkspaceRef, WorktreeRelease};
use crate::store::{Store, StoreError};

/// How clearing one task went.
#[derive(Debug, Clone, PartialEq)]
pub enum Cleared {
    /// Off the board; its session is stopped and its worktree given back.
    Cleared(Task),
    /// Still on the board: its worktree holds work that has not landed.
    Kept {
        task: Task,
        uncommitted: bool,
        unpushed_commits: u32,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ClearError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error("{0}")]
    Join(String),
}

/// Whether a task in `state` can be cleared.
pub fn clearable(state: TaskState) -> bool {
    matches!(state, TaskState::Done | TaskState::Failed)
}

async fn blocking<T: Send + 'static>(
    store: &Arc<Store>,
    f: impl FnOnce(&Store) -> crate::store::Result<T> + Send + 'static,
) -> Result<T, ClearError> {
    let store = store.clone();
    tokio::task::spawn_blocking(move || f(&store))
        .await
        .map_err(|e| ClearError::Join(e.to_string()))?
        .map_err(ClearError::from)
}

/// Clear the task with API id `id`. Clearing a cleared task again is a
/// no-op; a task that is still working is refused as a conflict.
pub async fn clear(
    store: &Arc<Store>,
    engine: &Arc<dyn EngineAdapter>,
    id: &str,
) -> Result<Cleared, ClearError> {
    let tid = id.to_string();
    let (task, target) = blocking(store, move |s| {
        Ok((s.get_task(&tid)?, s.task_target(&tid)?))
    })
    .await?;
    if task.cleared_at.is_some() {
        return Ok(Cleared::Cleared(task));
    }
    if !clearable(task.state) {
        return Err(StoreError::Conflict(format!(
            "the worker is {}; cancel it or let it finish first",
            task.state.as_str()
        ))
        .into());
    }
    // Without a workspace the engine holds nothing for it.
    if let Some(root) = target.workspace_path {
        let ws = WorkspaceRef {
            project_id: target.project_id,
            root: root.into(),
        };
        // A task the daemon counts as done (its PR merged) may still be in
        // review in the engine, which gives back only finished worktrees.
        // Cancelling a finished task changes nothing but stopping a session
        // its agent left running.
        match engine
            .control(&ws, &target.engine_id, &TaskControl::Cancel)
            .await
        {
            Ok(()) | Err(EngineError::TaskNotFound(_)) => {}
            Err(e) => return Err(e.into()),
        }
        match engine.release_worktree(&ws, &target.engine_id).await {
            Ok(WorktreeRelease::Returned) | Err(EngineError::TaskNotFound(_)) => {}
            Ok(WorktreeRelease::Kept {
                uncommitted,
                unpushed_commits,
            }) => {
                return Ok(Cleared::Kept {
                    task,
                    uncommitted,
                    unpushed_commits,
                })
            }
            Err(e) => return Err(e.into()),
        }
    }
    let tid = id.to_string();
    Ok(Cleared::Cleared(
        blocking(store, move |s| s.clear_task(&tid)).await?,
    ))
}

/// What a kept worktree holds, in words.
pub fn unlanded(uncommitted: bool, unpushed: u32) -> String {
    let commits = match unpushed {
        0 => None,
        1 => Some("1 unpushed commit".to_string()),
        n => Some(format!("{n} unpushed commits")),
    };
    match (uncommitted, commits) {
        (true, Some(c)) => format!("uncommitted changes and {c}"),
        (true, None) => "uncommitted changes".into(),
        (false, Some(c)) => c,
        (false, None) => "work not landed".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_what_has_not_landed() {
        assert_eq!(unlanded(true, 0), "uncommitted changes");
        assert_eq!(unlanded(false, 1), "1 unpushed commit");
        assert_eq!(
            unlanded(true, 3),
            "uncommitted changes and 3 unpushed commits"
        );
    }
}
