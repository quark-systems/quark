//! The PR center's projection: `pull_requests`, `checks` and `reviews`.
//!
//! Which pull requests exist comes from the tasks (the engine records the URL
//! each task opened); everything else is what the forge last reported. Each
//! change emits `pr.updated`, `check.updated` or `review.updated` in the same
//! transaction.

use std::collections::HashSet;

use base64::Engine as _;
use quark_systems::{
    Check, CheckStatus, CheckUpdated, ChecksState, CommentUpdated, DiffSide, EventType, Evidence,
    Mergeability, PrComment, PrCommentKind, PrConversation, PrConversationEntry, PrEntryKind,
    PrThread, PullRequest, PullRequestState, Review, ReviewDecision, ReviewState, ReviewUpdated,
    TranscriptEntry, TranscriptRole,
};
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};
use serde::de::DeserializeOwned;
use serde::Serialize;

use super::{
    append_event, finish_task, new_id, task_from_row, Result, Store, StoreError, TaskTarget,
    TASK_SELECT,
};
use crate::forge::{ForgePr, PrRef};
use crate::now_rfc3339;

/// A pull request the PR center reads from its forge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrSyncTarget {
    pub id: String,
    pub project_id: String,
    pub url: String,
    pub state: PullRequestState,
    /// Never read from the forge yet.
    pub never_synced: bool,
}

/// A pull request with the task that owns it, for comments and merges.
#[derive(Debug, Clone, PartialEq)]
pub struct PrOwner {
    pub pull_request: PullRequest,
    /// The owning task's engine coordinates; `None` when no task owns it.
    pub task: Option<TaskTarget>,
}

/// A merged pull request the Project's coordinator opened itself and has
/// not yet been told merged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinatorMerge {
    pub id: String,
    pub project_id: String,
    pub url: String,
    /// The Project's workspace root; `None` when it has none attached.
    pub workspace_path: Option<String>,
}

/// SQL: the pull request `pr` merged and its change reached the default
/// branch. A merge into another branch lands only when that branch's own
/// pull request merged into the default branch at or after it; merged
/// before, it never carries the change. A row whose default branch was
/// never read counts as landed.
pub(super) const LANDED: &str = "(pr.state = 'merged' AND (pr.default_branch IS NULL
    OR pr.base_ref IS NULL OR pr.base_ref = pr.default_branch
    OR EXISTS (SELECT 1 FROM pull_requests parent
        WHERE parent.project_id = pr.project_id AND parent.repo = pr.repo
          AND parent.head_ref = pr.base_ref AND parent.state = 'merged'
          AND parent.base_ref = parent.default_branch
          AND parent.merged_at >= pr.merged_at)))";

/// SQL: the pull request `pr` merged into another branch and its change
/// will not reach the default branch: no pull request of that branch is
/// still open to carry it there.
const STRANDED: &str = "(pr.state = 'merged' AND pr.default_branch IS NOT NULL
    AND pr.base_ref IS NOT NULL AND pr.base_ref <> pr.default_branch
    AND NOT EXISTS (SELECT 1 FROM pull_requests parent
        WHERE parent.project_id = pr.project_id AND parent.repo = pr.repo
          AND parent.head_ref = pr.base_ref
          AND (parent.state IN ('open', 'draft')
               OR (parent.state = 'merged' AND parent.base_ref = parent.default_branch
                   AND parent.merged_at >= pr.merged_at))))";

/// A merged pull request whose change missed the default branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrandedMerge {
    pub id: String,
    pub project_id: String,
    pub url: String,
    /// The branch it merged into.
    pub base: String,
    pub default_branch: String,
    /// The owning task's engine id, when a task owns it.
    pub task: Option<String>,
    pub workspace_path: Option<String>,
}

/// An open pull request whose base branch's own pull request already
/// merged, so it would merge into a branch nothing carries any further.
#[derive(Debug, Clone, PartialEq)]
pub struct Retarget {
    pub owner: PrOwner,
    pub base: String,
    pub default_branch: String,
}

const PR_COLUMNS: &str = "id, project_id, task_id, url, provider, repo, number, title, author, \
    state, head_ref, base_ref, head_sha, mergeable, checks_state, review_decision, additions, \
    deletions, changed_files, opened_at, updated_at, merged_at, closed_at, synced_at, sync_error, \
    evidence, body";

impl Store {
    /// Records a pull request row for every task that reports one, links it
    /// to its task, and returns the pull requests worth reading from the
    /// forge: every one that is not merged or closed, or was never read.
    pub fn ensure_pull_requests(&self) -> Result<Vec<PrSyncTarget>> {
        self.write(|tx, events| {
            let sources: Vec<(String, String, String)> = {
                let mut stmt = tx.prepare(
                    "SELECT id, project_id, pull_request_url FROM tasks
                     WHERE pull_request_url IS NOT NULL ORDER BY created_at, id",
                )?;
                let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                rows.collect::<std::result::Result<_, _>>()?
            };
            for (task_id, project_id, url) in sources {
                let Some(r) = PrRef::parse(&url) else {
                    continue;
                };
                let existing: Option<(String, Option<String>)> = tx
                    .query_row(
                        "SELECT id, task_id FROM pull_requests WHERE project_id = ?1 AND url = ?2",
                        params![project_id, url],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                match existing {
                    Some((_, Some(_))) => {}
                    Some((id, None)) => {
                        tx.execute(
                            "UPDATE pull_requests SET task_id = ?2 WHERE id = ?1",
                            params![id, task_id],
                        )?;
                        emit_pr(tx, events, &id)?;
                    }
                    None => {
                        let id = new_id("pr");
                        tx.execute(
                            "INSERT INTO pull_requests (id, project_id, task_id, url, provider,
                                 repo, number, state, mergeable, checks_state, review_decision,
                                 created_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'open', 'unknown', 'none',
                                 'none', ?8)",
                            params![
                                id,
                                project_id,
                                task_id,
                                url,
                                r.provider,
                                r.repo,
                                r.number as i64,
                                now_rfc3339()
                            ],
                        )?;
                        emit_pr(tx, events, &id)?;
                    }
                }
            }
            let mut stmt = tx.prepare(
                "SELECT id, project_id, url, state, synced_at IS NULL FROM pull_requests
                 WHERE state IN ('open', 'draft') OR synced_at IS NULL
                 ORDER BY created_at, id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(PrSyncTarget {
                    id: r.get(0)?,
                    project_id: r.get(1)?,
                    url: r.get(2)?,
                    state: PullRequestState::parse(&r.get::<_, String>(3)?)
                        .unwrap_or(PullRequestState::Open),
                    never_synced: r.get(4)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })
    }

    /// Records what the forge reported for a pull request. Emits
    /// `check.updated` and `review.updated` for each check and review that
    /// appeared or changed, then `pr.updated` when anything changed.
    pub fn apply_forge_pr(&self, id: &str, pr: &ForgePr) -> Result<PullRequest> {
        self.write(|tx, events| {
            let old = get_pr(tx, id)?;
            let checks_state = pr.checks_state();
            tx.execute(
                "UPDATE pull_requests SET title = ?2, author = ?3, state = ?4, head_ref = ?5,
                     base_ref = ?6, head_sha = ?7, mergeable = ?8, checks_state = ?9,
                     review_decision = ?10, additions = ?11, deletions = ?12,
                     changed_files = ?13, opened_at = ?14, updated_at = ?15, merged_at = ?16,
                     closed_at = ?17, synced_at = ?18, sync_error = NULL, body = ?19,
                     default_branch = COALESCE(?20, default_branch)
                 WHERE id = ?1",
                params![
                    id,
                    pr.title,
                    pr.author,
                    pr.state.as_str(),
                    pr.head_ref,
                    pr.base_ref,
                    pr.head_sha,
                    enum_str(&pr.mergeable)?,
                    enum_str(&checks_state)?,
                    enum_str(&pr.review_decision)?,
                    pr.additions.map(|n| n as i64),
                    pr.deletions.map(|n| n as i64),
                    pr.changed_files.map(|n| n as i64),
                    pr.opened_at,
                    pr.updated_at,
                    pr.merged_at,
                    pr.closed_at,
                    now_rfc3339(),
                    pr.body,
                    pr.default_branch,
                ],
            )?;

            // Checks belong to the current head: ones that no longer report
            // are dropped.
            let names: Vec<&str> = pr.checks.iter().map(|c| c.name.as_str()).collect();
            for gone in old
                .checks
                .iter()
                .filter(|c| !names.contains(&c.name.as_str()))
            {
                tx.execute(
                    "DELETE FROM checks WHERE pull_request_id = ?1 AND name = ?2",
                    params![id, gone.name],
                )?;
            }
            for check in &pr.checks {
                if old.checks.iter().any(|c| c == check) {
                    continue;
                }
                tx.execute(
                    "INSERT INTO checks (pull_request_id, name, status, conclusion, details_url,
                         started_at, completed_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                     ON CONFLICT (pull_request_id, name) DO UPDATE SET status = excluded.status,
                         conclusion = excluded.conclusion, details_url = excluded.details_url,
                         started_at = excluded.started_at, completed_at = excluded.completed_at",
                    params![
                        id,
                        check.name,
                        enum_str(&check.status)?,
                        check.conclusion,
                        check.details_url,
                        check.started_at,
                        check.completed_at
                    ],
                )?;
                append_event(
                    tx,
                    events,
                    Some(&old.project_id),
                    EventType::CheckUpdated,
                    serde_json::to_value(CheckUpdated {
                        pull_request_id: id.to_string(),
                        task_id: old.task_id.clone(),
                        head_sha: pr.head_sha.clone(),
                        check: check.clone(),
                    })?,
                )?;
            }

            let ids: Vec<&str> = pr.reviews.iter().map(|r| r.id.as_str()).collect();
            for gone in old.reviews.iter().filter(|r| !ids.contains(&r.id.as_str())) {
                tx.execute(
                    "DELETE FROM reviews WHERE pull_request_id = ?1 AND id = ?2",
                    params![id, gone.id],
                )?;
            }
            for review in &pr.reviews {
                if old.reviews.iter().any(|r| r == review) {
                    continue;
                }
                tx.execute(
                    "INSERT INTO reviews (pull_request_id, id, author, state, body, submitted_at,
                         commit_sha)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                     ON CONFLICT (pull_request_id, id) DO UPDATE SET author = excluded.author,
                         state = excluded.state, body = excluded.body,
                         submitted_at = excluded.submitted_at, commit_sha = excluded.commit_sha",
                    params![
                        id,
                        review.id,
                        review.author,
                        enum_str(&review.state)?,
                        review.body,
                        review.submitted_at,
                        review.commit
                    ],
                )?;
                append_event(
                    tx,
                    events,
                    Some(&old.project_id),
                    EventType::ReviewUpdated,
                    serde_json::to_value(ReviewUpdated {
                        pull_request_id: id.to_string(),
                        task_id: old.task_id.clone(),
                        review: review.clone(),
                    })?,
                )?;
            }

            if let Some(comments) = &pr.comments {
                apply_comments(tx, events, &old, comments)?;
            }

            let new = get_pr(tx, id)?;
            // A merge finishes the task that opened the pull request. The
            // snapshot projection does the same for merges it sees later.
            if new.state == PullRequestState::Merged && landed(tx, id)? {
                if let Some(task_id) = &new.task_id {
                    let task = tx
                        .query_row(
                            &format!("{TASK_SELECT} WHERE id = ?1"),
                            [task_id],
                            task_from_row,
                        )
                        .optional()?;
                    if let Some(task) =
                        task.filter(|t| t.pull_request_url.as_ref() == Some(&new.url))
                    {
                        finish_task(tx, events, &task)?;
                    }
                }
            }
            if !same_apart_from_sync_time(&old, &new) {
                append_event(
                    tx,
                    events,
                    Some(&new.project_id),
                    EventType::PrUpdated,
                    serde_json::to_value(&new)?,
                )?;
            }
            Ok(new)
        })
    }

    /// Records why reading a pull request failed. Emits `pr.updated` only
    /// when the reason changed.
    pub fn set_pr_sync_error(&self, id: &str, error: &str) -> Result<()> {
        self.write(|tx, events| {
            let changed = tx.execute(
                "UPDATE pull_requests SET sync_error = ?2 WHERE id = ?1 AND sync_error IS NOT ?2",
                params![id, error],
            )?;
            if changed > 0 {
                emit_pr(tx, events, id)?;
            }
            Ok(())
        })
    }

    /// Records a pull request's gate evidence as the engine reports it.
    /// Artifact ids and URLs are assigned here. Emits `pr.updated` only when
    /// the evidence changed.
    pub fn apply_evidence(&self, id: &str, evidence: Option<Evidence>) -> Result<()> {
        let evidence = evidence.map(|mut e| {
            e.stale = false;
            for a in e
                .gates
                .iter_mut()
                .flat_map(|g| g.cases.iter_mut())
                .flat_map(|c| c.artifacts.iter_mut())
            {
                a.id = artifact_id(&a.id);
                a.url = format!("/v1/pull-requests/{id}/evidence/artifacts/{}", a.id);
            }
            e
        });
        let json = evidence.as_ref().map(serde_json::to_string).transpose()?;
        self.write(|tx, events| {
            let changed = tx.execute(
                "UPDATE pull_requests SET evidence = ?2 WHERE id = ?1 AND evidence IS NOT ?2",
                params![id, json],
            )?;
            if changed > 0 {
                emit_pr(tx, events, id)?;
            }
            Ok(())
        })
    }

    /// Pull requests across all Projects, newest first.
    pub fn list_pull_requests(
        &self,
        state: Option<PullRequestState>,
        project_id: Option<&str>,
    ) -> Result<Vec<PullRequest>> {
        self.read(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {PR_COLUMNS} FROM pull_requests
                 WHERE (?1 IS NULL OR state = ?1) AND (?2 IS NULL OR project_id = ?2)
                 ORDER BY COALESCE(opened_at, created_at) DESC, id DESC"
            ))?;
            let rows = stmt.query_map(
                params![state.map(PullRequestState::as_str), project_id],
                pr_from_row,
            )?;
            let mut out: Vec<PullRequest> = rows.collect::<std::result::Result<_, _>>()?;
            for pr in &mut out {
                load_children(c, pr)?;
            }
            Ok(out)
        })
    }

    pub fn get_pull_request(&self, id: &str) -> Result<PullRequest> {
        self.read(|c| get_pr(c, id))
    }

    /// A pull request's conversation in order: its comments, and its
    /// reviews with the inline threads each started.
    pub fn pull_request_conversation(&self, id: &str) -> Result<PrConversation> {
        self.read(|c| {
            let pr = get_pr(c, id)?;
            let comments = load_comments(c, id)?;
            Ok(conversation(id, pr.reviews, comments))
        })
    }

    /// A pull request and where its owning task lives in the engine.
    pub fn pull_request_owner(&self, id: &str) -> Result<PrOwner> {
        self.read(|c| {
            let pull_request = get_pr(c, id)?;
            let task = match &pull_request.task_id {
                Some(t) => c
                    .query_row(
                        "SELECT t.project_id, t.engine_id, p.workspace_path
                         FROM tasks t JOIN projects p ON p.id = t.project_id
                         WHERE t.id = ?1",
                        [t],
                        |r| {
                            Ok(TaskTarget {
                                project_id: r.get(0)?,
                                engine_id: r.get(1)?,
                                workspace_path: r.get(2)?,
                            })
                        },
                    )
                    .optional()?,
                None => None,
            };
            Ok(PrOwner { pull_request, task })
        })
    }

    /// Merged pull requests that a task owns, for telling the engine of
    /// merges made outside it.
    pub fn merged_with_task(&self) -> Result<Vec<PrOwner>> {
        let ids: Vec<String> = self.read(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT pr.id FROM pull_requests pr
                     WHERE pr.task_id IS NOT NULL AND {LANDED}
                     ORDER BY pr.created_at, pr.id"
            ))?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })?;
        ids.iter().map(|id| self.pull_request_owner(id)).collect()
    }

    /// Open pull requests a task owns that cannot merge until its worker
    /// acts: conflicting with their base, or with a failed check.
    pub fn needing_worker(&self) -> Result<Vec<PrOwner>> {
        let ids: Vec<String> = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT id FROM pull_requests
                 WHERE state = 'open' AND task_id IS NOT NULL AND sync_error IS NULL
                   AND (mergeable = 'conflicting' OR checks_state = 'failing')
                 ORDER BY created_at, id",
            )?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })?;
        ids.iter().map(|id| self.pull_request_owner(id)).collect()
    }

    /// Open pull requests of Projects with standing approval that are ready
    /// to merge: not a draft, mergeable, every check passed and no review
    /// holding them.
    pub fn standing_approval_ready(&self) -> Result<Vec<PrOwner>> {
        let ids: Vec<String> = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT pr.id FROM pull_requests pr JOIN projects p ON p.id = pr.project_id
                 WHERE p.standing_approval = 1 AND pr.state = 'open'
                   AND pr.mergeable = 'mergeable' AND pr.checks_state = 'passing'
                   AND pr.review_decision IN ('approved', 'none')
                   AND pr.task_id IS NOT NULL AND pr.sync_error IS NULL
                   AND (pr.default_branch IS NULL OR pr.base_ref = pr.default_branch)
                 ORDER BY pr.created_at, pr.id",
            )?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })?;
        ids.iter().map(|id| self.pull_request_owner(id)).collect()
    }
    /// Merged pull requests the Project's coordinator opened itself and has
    /// not been told of. No task owns them, so the engine has no task to
    /// land: the coordinator is told directly, once (see
    /// [`Store::mark_merge_told`]).
    pub fn coordinator_merges_untold(&self) -> Result<Vec<CoordinatorMerge>> {
        self.read(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT pr.id, pr.project_id, pr.url, p.workspace_path
                 FROM pull_requests pr JOIN projects p ON p.id = pr.project_id
                 WHERE pr.task_id IS NULL AND pr.opened_by = 'coordinator'
                   AND pr.merge_told_at IS NULL AND {LANDED}
                 ORDER BY pr.created_at, pr.id"
            ))?;
            let rows = stmt.query_map([], |r| {
                Ok(CoordinatorMerge {
                    id: r.get(0)?,
                    project_id: r.get(1)?,
                    url: r.get(2)?,
                    workspace_path: r.get(3)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })
    }

    /// Tasks whose pull request landed but are not done yet: a pull request
    /// merged into another branch lands when that branch's own pull request
    /// merges later, which no forge read of the first one shows.
    pub fn finish_landed(&self) -> Result<()> {
        self.write(|tx, events| {
            let tasks: Vec<quark_systems::Task> = {
                let mut stmt = tx.prepare(&format!(
                    "{TASK_SELECT} WHERE state <> 'done' AND id IN (
                         SELECT pr.task_id FROM pull_requests pr
                         WHERE pr.task_id = tasks.id AND pr.url = tasks.pull_request_url
                           AND {LANDED})"
                ))?;
                let rows = stmt.query_map([], task_from_row)?;
                rows.collect::<std::result::Result<_, _>>()?
            };
            for task in &tasks {
                finish_task(tx, events, task)?;
            }
            Ok(())
        })
    }

    /// Merged pull requests whose change missed the default branch, that
    /// the coordinator has not been told of (see
    /// [`Store::mark_stranded_told`]).
    pub fn stranded_untold(&self) -> Result<Vec<StrandedMerge>> {
        self.read(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT pr.id, pr.project_id, pr.url, pr.base_ref, pr.default_branch,
                        t.engine_id, p.workspace_path
                 FROM pull_requests pr JOIN projects p ON p.id = pr.project_id
                 LEFT JOIN tasks t ON t.id = pr.task_id
                 WHERE pr.stranded_told_at IS NULL AND {STRANDED}
                 ORDER BY pr.created_at, pr.id"
            ))?;
            let rows = stmt.query_map([], |r| {
                Ok(StrandedMerge {
                    id: r.get(0)?,
                    project_id: r.get(1)?,
                    url: r.get(2)?,
                    base: r.get(3)?,
                    default_branch: r.get(4)?,
                    task: r.get(5)?,
                    workspace_path: r.get(6)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })
    }

    /// Records that the coordinator was told the pull request's merge
    /// missed the default branch.
    pub fn mark_stranded_told(&self, id: &str) -> Result<()> {
        self.write(|tx, _| {
            tx.execute(
                "UPDATE pull_requests SET stranded_told_at = ?2
                 WHERE id = ?1 AND stranded_told_at IS NULL",
                params![id, now_rfc3339()],
            )?;
            Ok(())
        })
    }

    /// Open pull requests a task owns whose base is not the default branch
    /// and whose base branch's own pull request already merged: merged as
    /// they are, their change would never reach the default branch.
    pub fn needing_retarget(&self) -> Result<Vec<Retarget>> {
        let rows: Vec<(String, String, String)> = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT pr.id, pr.base_ref, pr.default_branch FROM pull_requests pr
                 WHERE pr.state IN ('open', 'draft') AND pr.task_id IS NOT NULL
                   AND pr.default_branch IS NOT NULL AND pr.base_ref <> pr.default_branch
                   AND EXISTS (SELECT 1 FROM pull_requests parent
                       WHERE parent.project_id = pr.project_id AND parent.repo = pr.repo
                         AND parent.head_ref = pr.base_ref
                         AND parent.state IN ('merged', 'closed'))
                 ORDER BY pr.created_at, pr.id",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })?;
        rows.into_iter()
            .map(|(id, base, default_branch)| {
                Ok(Retarget {
                    owner: self.pull_request_owner(&id)?,
                    base,
                    default_branch,
                })
            })
            .collect()
    }

    /// The branch the pull request merges into and the repository's default
    /// branch, when merged now its change would miss the default branch:
    /// they differ, and no open pull request of that branch would carry it
    /// on.
    pub fn off_default(&self, id: &str) -> Result<Option<(String, String)>> {
        self.read(|c| {
            Ok(c.query_row(
                "SELECT pr.base_ref, pr.default_branch FROM pull_requests pr
                 WHERE pr.id = ?1 AND pr.base_ref IS NOT NULL
                   AND pr.default_branch IS NOT NULL AND pr.base_ref <> pr.default_branch
                   AND NOT EXISTS (SELECT 1 FROM pull_requests parent
                       WHERE parent.project_id = pr.project_id AND parent.repo = pr.repo
                         AND parent.head_ref = pr.base_ref
                         AND parent.state IN ('open', 'draft'))",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
        })
    }

    /// Records that the coordinator was told the pull request merged.
    pub fn mark_merge_told(&self, id: &str) -> Result<()> {
        self.write(|tx, _| {
            tx.execute(
                "UPDATE pull_requests SET merge_told_at = ?2
                 WHERE id = ?1 AND merge_told_at IS NULL",
                params![id, now_rfc3339()],
            )?;
            Ok(())
        })
    }
}

/// Watches a coordinator's transcript for a pull request it opened from its
/// own shell (`gh pr create`, `glab mr create`) rather than through a task,
/// and records it, so the PR center tracks it and its merge wakes the
/// coordinator. `calls` holds the pull-request-opening calls seen earlier in
/// the same batch, by call id; a call from an earlier batch is looked up in
/// the recorded transcript.
pub(super) fn note_coordinator_entry(
    tx: &Transaction,
    events: &mut Vec<quark_systems::Event>,
    project_id: &str,
    entry: &TranscriptEntry,
    calls: &mut HashSet<String>,
) -> Result<()> {
    let Some(call_id) = entry.tool_call_id.as_deref() else {
        return Ok(());
    };
    match entry.role {
        TranscriptRole::ToolCall => {
            if opens_pull_request(call_command(entry)) {
                calls.insert(call_id.to_string());
            }
        }
        TranscriptRole::ToolResult if !entry.is_error => {
            let Some(url) = opened_url(&entry.text) else {
                return Ok(());
            };
            if calls.contains(call_id) || recorded_call_opens(tx, project_id, call_id)? {
                record_coordinator_pr(tx, events, project_id, &url)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// The shell command a tool call ran, or its raw input when the harness
/// records no command.
fn call_command(entry: &TranscriptEntry) -> &str {
    entry
        .tool
        .as_ref()
        .and_then(|t| t.command.as_deref())
        .unwrap_or(&entry.text)
}

/// Whether a shell command opens a pull request.
pub fn opens_pull_request(command: &str) -> bool {
    let words: Vec<&str> = command.split_whitespace().collect();
    words.windows(3).any(|w| {
        matches!(
            (w[0].rsplit('/').next().unwrap_or(w[0]), w[1], w[2]),
            ("gh", "pr", "create") | ("glab", "mr", "create")
        )
    })
}

/// The pull request URL a `gh pr create` or `glab mr create` printed: the
/// last word of its output that names one.
pub fn opened_url(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .rev()
        .map(|w| w.trim_matches(|c: char| matches!(c, '"' | '\'' | '<' | '>' | '(' | ')' | ',')))
        .find(|w| PrRef::parse(w).is_some())
        .map(str::to_string)
}

fn recorded_call_opens(tx: &Transaction, project_id: &str, call_id: &str) -> Result<bool> {
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload FROM events
             WHERE type = ?1 AND project_id = ?2
               AND json_extract(payload, '$.entry.role') = 'tool_call'
               AND json_extract(payload, '$.entry.tool_call_id') = ?3
             ORDER BY seq DESC LIMIT 1",
            params![EventType::CoordinatorMessage.as_str(), project_id, call_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(payload) = payload else {
        return Ok(false);
    };
    let value: serde_json::Value = serde_json::from_str(&payload)?;
    let entry: TranscriptEntry = serde_json::from_value(value["entry"].clone())?;
    Ok(opens_pull_request(call_command(&entry)))
}

/// Records the pull request at `url` as opened by `project_id`'s coordinator,
/// unless the Project already tracks it.
fn record_coordinator_pr(
    tx: &Transaction,
    events: &mut Vec<quark_systems::Event>,
    project_id: &str,
    url: &str,
) -> Result<()> {
    let Some(r) = PrRef::parse(url) else {
        return Ok(());
    };
    let known: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM pull_requests WHERE project_id = ?1 AND url = ?2)",
        params![project_id, url],
        |r| r.get(0),
    )?;
    if known {
        return Ok(());
    }
    let id = new_id("pr");
    tx.execute(
        "INSERT INTO pull_requests (id, project_id, task_id, url, provider, repo, number,
             state, mergeable, checks_state, review_decision, created_at, opened_by)
         VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, 'open', 'unknown', 'none', 'none', ?7,
             'coordinator')",
        params![
            id,
            project_id,
            url,
            r.provider,
            r.repo,
            r.number as i64,
            now_rfc3339()
        ],
    )?;
    emit_pr(tx, events, &id)
}

/// The public id of a gate artifact: its relative path, base64url-encoded.
pub fn artifact_id(path: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(path)
}

/// The relative path an artifact id names, if it is one.
pub fn artifact_path(id: &str) -> Option<String> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(id)
        .ok()?;
    String::from_utf8(bytes).ok()
}

/// Whether the pull request `id` merged and its change reached the default
/// branch (see [`LANDED`]).
fn landed(tx: &Transaction, id: &str) -> Result<bool> {
    Ok(tx.query_row(
        &format!("SELECT EXISTS (SELECT 1 FROM pull_requests pr WHERE pr.id = ?1 AND {LANDED})"),
        [id],
        |r| r.get(0),
    )?)
}

fn emit_pr(tx: &Transaction, events: &mut Vec<quark_systems::Event>, id: &str) -> Result<()> {
    let pr = get_pr(tx, id)?;
    append_event(
        tx,
        events,
        Some(&pr.project_id),
        EventType::PrUpdated,
        serde_json::to_value(&pr)?,
    )
}

fn same_apart_from_sync_time(a: &PullRequest, b: &PullRequest) -> bool {
    PullRequest {
        synced_at: None,
        ..a.clone()
    } == PullRequest {
        synced_at: None,
        ..b.clone()
    }
}

fn get_pr(c: &Connection, id: &str) -> Result<PullRequest> {
    let mut pr = c
        .query_row(
            &format!("SELECT {PR_COLUMNS} FROM pull_requests WHERE id = ?1"),
            [id],
            pr_from_row,
        )
        .optional()?
        .ok_or(StoreError::NotFound)?;
    load_children(c, &mut pr)?;
    Ok(pr)
}

fn load_children(c: &Connection, pr: &mut PullRequest) -> Result<()> {
    let mut stmt = c.prepare(
        "SELECT name, status, conclusion, details_url, started_at, completed_at FROM checks
         WHERE pull_request_id = ?1 ORDER BY name",
    )?;
    let rows = stmt.query_map([&pr.id], |r| {
        Ok(Check {
            name: r.get(0)?,
            status: parse_enum(&r.get::<_, String>(1)?).unwrap_or(CheckStatus::Pending),
            conclusion: r.get(2)?,
            details_url: r.get(3)?,
            started_at: r.get(4)?,
            completed_at: r.get(5)?,
        })
    })?;
    pr.checks = rows.collect::<std::result::Result<_, _>>()?;
    let mut stmt = c.prepare(
        "SELECT id, author, state, body, submitted_at, commit_sha FROM reviews
         WHERE pull_request_id = ?1 ORDER BY submitted_at, id",
    )?;
    let rows = stmt.query_map([&pr.id], |r| {
        Ok(Review {
            id: r.get(0)?,
            author: r.get(1)?,
            state: parse_enum(&r.get::<_, String>(2)?).unwrap_or(ReviewState::Commented),
            body: r.get(3)?,
            submitted_at: r.get(4)?,
            commit: r.get(5)?,
        })
    })?;
    pr.reviews = rows.collect::<std::result::Result<_, _>>()?;
    Ok(())
}

/// Records the forge's comments: new and edited ones are written and emit
/// `comment.updated`; ones gone from the forge are deleted and emit it with
/// `deleted`.
fn apply_comments(
    tx: &Transaction,
    events: &mut Vec<quark_systems::Event>,
    pr: &PullRequest,
    comments: &[PrComment],
) -> Result<()> {
    let old = load_comments(tx, &pr.id)?;
    let emit = |events: &mut Vec<quark_systems::Event>, comment: &PrComment, deleted: bool| {
        append_event(
            tx,
            events,
            Some(&pr.project_id),
            EventType::CommentUpdated,
            serde_json::to_value(CommentUpdated {
                pull_request_id: pr.id.clone(),
                task_id: pr.task_id.clone(),
                comment: comment.clone(),
                deleted,
            })?,
        )
    };
    for gone in old
        .iter()
        .filter(|o| !comments.iter().any(|c| c.id == o.id))
    {
        tx.execute(
            "DELETE FROM pr_comments WHERE pull_request_id = ?1 AND id = ?2",
            params![pr.id, gone.id],
        )?;
        emit(events, gone, true)?;
    }
    for c in comments {
        if old.iter().any(|o| o == c) {
            continue;
        }
        tx.execute(
            "INSERT INTO pr_comments (pull_request_id, id, kind, author, body, created_at,
                 updated_at, url, review_id, path, line, original_line, side, commit_sha,
                 in_reply_to)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
             ON CONFLICT (pull_request_id, id) DO UPDATE SET kind = excluded.kind,
                 author = excluded.author, body = excluded.body,
                 created_at = excluded.created_at, updated_at = excluded.updated_at,
                 url = excluded.url, review_id = excluded.review_id, path = excluded.path,
                 line = excluded.line, original_line = excluded.original_line,
                 side = excluded.side, commit_sha = excluded.commit_sha,
                 in_reply_to = excluded.in_reply_to",
            params![
                pr.id,
                c.id,
                enum_str(&c.kind)?,
                c.author,
                c.body,
                c.created_at,
                c.updated_at,
                c.url,
                c.review_id,
                c.path,
                c.line.map(|n| n as i64),
                c.original_line.map(|n| n as i64),
                c.side.as_ref().map(enum_str).transpose()?,
                c.commit,
                c.in_reply_to,
            ],
        )?;
        emit(events, c, false)?;
    }
    Ok(())
}

fn load_comments(c: &Connection, pr: &str) -> Result<Vec<PrComment>> {
    let mut stmt = c.prepare(
        "SELECT id, kind, author, body, created_at, updated_at, url, review_id, path, line,
             original_line, side, commit_sha, in_reply_to
         FROM pr_comments WHERE pull_request_id = ?1 ORDER BY created_at, id",
    )?;
    let rows = stmt.query_map([pr], |r| {
        Ok(PrComment {
            id: r.get(0)?,
            kind: parse_enum(&r.get::<_, String>(1)?).unwrap_or(PrCommentKind::Conversation),
            author: r.get(2)?,
            body: r.get(3)?,
            created_at: r.get(4)?,
            updated_at: r.get(5)?,
            url: r.get(6)?,
            review_id: r.get(7)?,
            path: r.get(8)?,
            line: r.get::<_, Option<i64>>(9)?.map(|n| n as u64),
            original_line: r.get::<_, Option<i64>>(10)?.map(|n| n as u64),
            side: r.get::<_, Option<String>>(11)?.and_then(|s| parse_enum(&s)),
            commit: r.get(12)?,
            in_reply_to: r.get(13)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Orders a pull request's reviews and comments into its conversation.
/// Inline comments are grouped into threads by the comment they reply to,
/// and each thread goes with the review its first comment was left in.
pub fn conversation(id: &str, reviews: Vec<Review>, comments: Vec<PrComment>) -> PrConversation {
    let (inline, plain): (Vec<_>, Vec<_>) = comments
        .into_iter()
        .partition(|c| c.kind == PrCommentKind::Inline);
    let known: std::collections::HashSet<&str> = inline.iter().map(|c| c.id.as_str()).collect();
    // A reply's thread is the first comment up its chain the forge still has.
    let root_of = |c: &PrComment| -> String {
        let mut at = c;
        let mut seen = 0;
        while let Some(parent) = at.in_reply_to.as_deref().filter(|p| known.contains(p)) {
            at = inline.iter().find(|x| x.id == parent).expect("known");
            seen += 1;
            if seen > inline.len() {
                break;
            }
        }
        at.id.clone()
    };
    let mut threads: Vec<PrThread> = Vec::new();
    for c in &inline {
        let root = root_of(c);
        match threads.iter_mut().find(|t| t.id == root) {
            Some(t) => t.comments.push(c.clone()),
            None => {
                let first = inline.iter().find(|x| x.id == root).unwrap_or(c);
                threads.push(PrThread {
                    id: root.clone(),
                    path: first.path.clone().unwrap_or_default(),
                    line: first.line,
                    original_line: first.original_line,
                    side: first.side.unwrap_or(DiffSide::New),
                    commit: first.commit.clone(),
                    outdated: first.line.is_none(),
                    comments: vec![c.clone()],
                });
            }
        }
    }
    for t in &mut threads {
        t.comments
            .sort_by(|a, b| (&a.created_at, &a.id).cmp(&(&b.created_at, &b.id)));
    }

    let mut entries: Vec<PrConversationEntry> = Vec::new();
    for c in plain {
        entries.push(PrConversationEntry {
            kind: PrEntryKind::Comment,
            at: c.created_at.clone(),
            comment: Some(c),
            review: None,
            threads: vec![],
        });
    }
    let mut placed: std::collections::HashSet<String> = Default::default();
    for r in reviews {
        let started: Vec<PrThread> = threads
            .iter()
            .filter(|t| t.comments.first().and_then(|c| c.review_id.as_deref()) == Some(&r.id))
            .cloned()
            .collect();
        // A review that only holds replies to other threads says nothing
        // of its own.
        let replies_only = r.body.trim().is_empty()
            && started.is_empty()
            && r.state == ReviewState::Commented
            && threads
                .iter()
                .flat_map(|t| &t.comments)
                .any(|c| c.review_id.as_deref() == Some(&r.id));
        placed.extend(started.iter().map(|t| t.id.clone()));
        if replies_only {
            continue;
        }
        entries.push(PrConversationEntry {
            kind: PrEntryKind::Review,
            at: r.submitted_at.clone(),
            comment: None,
            review: Some(r),
            threads: started,
        });
    }
    for t in threads.into_iter().filter(|t| !placed.contains(&t.id)) {
        entries.push(PrConversationEntry {
            kind: PrEntryKind::Thread,
            at: t.comments.first().and_then(|c| c.created_at.clone()),
            comment: None,
            review: None,
            threads: vec![t],
        });
    }
    // Undated entries (a pending review) go last.
    entries.sort_by(|a, b| match (&a.at, &b.at) {
        (Some(x), Some(y)) => x.cmp(y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    PrConversation {
        pull_request_id: id.to_string(),
        entries,
    }
}

fn pr_from_row(r: &Row) -> rusqlite::Result<PullRequest> {
    let count = |i: usize| -> rusqlite::Result<Option<u64>> {
        Ok(r.get::<_, Option<i64>>(i)?.map(|n| n as u64))
    };
    Ok(PullRequest {
        id: r.get(0)?,
        project_id: r.get(1)?,
        task_id: r.get(2)?,
        url: r.get(3)?,
        provider: r.get(4)?,
        repo: r.get(5)?,
        number: r.get::<_, i64>(6)? as u64,
        title: r.get(7)?,
        body: r.get(26)?,
        author: r.get(8)?,
        state: PullRequestState::parse(&r.get::<_, String>(9)?).unwrap_or(PullRequestState::Open),
        head_ref: r.get(10)?,
        base_ref: r.get(11)?,
        head_sha: r.get(12)?,
        mergeable: parse_enum(&r.get::<_, String>(13)?).unwrap_or(Mergeability::Unknown),
        checks_state: parse_enum(&r.get::<_, String>(14)?).unwrap_or(ChecksState::None),
        review_decision: parse_enum(&r.get::<_, String>(15)?).unwrap_or(ReviewDecision::None),
        additions: count(16)?,
        deletions: count(17)?,
        changed_files: count(18)?,
        checks: Vec::new(),
        reviews: Vec::new(),
        evidence: None,
        opened_at: r.get(19)?,
        updated_at: r.get(20)?,
        merged_at: r.get(21)?,
        closed_at: r.get(22)?,
        synced_at: r.get(23)?,
        sync_error: r.get(24)?,
    })
    .map(|mut pr| {
        pr.evidence = r
            .get::<_, Option<String>>(25)
            .ok()
            .flatten()
            .and_then(|j| serde_json::from_str::<Evidence>(&j).ok())
            .map(|mut e| {
                e.stale = matches!((&e.head_sha, &pr.head_sha), (Some(a), Some(b)) if a != b);
                e
            });
        pr
    })
}

/// The snake_case name serde gives a unit enum variant.
fn enum_str<T: Serialize>(v: &T) -> Result<String> {
    match serde_json::to_value(v)? {
        serde_json::Value::String(s) => Ok(s),
        other => Err(StoreError::Invalid(format!("not a unit enum: {other}"))),
    }
}

fn parse_enum<T: DeserializeOwned>(s: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{EngineTask, FleetSnapshot};
    use quark_systems::{CreateProject, TaskState};

    const URL: &str = "https://github.com/quark-systems/quark/pull/16";

    fn store_with_pr() -> (Store, String) {
        let store = Store::open_in_memory().unwrap();
        let p = store
            .create_project(CreateProject {
                name: "Quark".into(),
                ..Default::default()
            })
            .unwrap();
        store
            .apply_snapshot(
                &p.id,
                &FleetSnapshot {
                    tasks: vec![EngineTask {
                        id: "ship-pr-center".into(),
                        title: "PR center".into(),
                        kind: None,
                        state: TaskState::InReview,
                        state_note: None,
                        state_source: None,
                        harness: None,
                        pull_request_url: Some(URL.into()),
                        terminal: None,
                        worktree: None,
                        worktree_at: None,
                        unhealthy: None,
                        unanswered_since: None,
                    }],
                },
            )
            .unwrap();
        (store, p.id)
    }

    fn inline(id: &str, review: &str, reply_to: Option<&str>, at: &str) -> PrComment {
        PrComment {
            id: id.into(),
            kind: PrCommentKind::Inline,
            author: Some("rev".into()),
            body: format!("comment {id}"),
            created_at: Some(at.into()),
            updated_at: Some(at.into()),
            url: None,
            review_id: Some(review.into()),
            path: Some("src/a.rs".into()),
            line: Some(7),
            original_line: Some(7),
            side: Some(DiffSide::New),
            commit: Some("abc".into()),
            in_reply_to: reply_to.map(str::to_string),
        }
    }

    fn review(id: &str, state: ReviewState, body: &str, at: &str) -> Review {
        Review {
            id: id.into(),
            author: Some("rev".into()),
            state,
            body: body.into(),
            submitted_at: Some(at.into()),
            commit: Some("abc".into()),
        }
    }

    #[test]
    fn the_conversation_groups_inline_comments_into_threads_under_their_review() {
        let plain = PrComment {
            kind: PrCommentKind::Conversation,
            review_id: None,
            path: None,
            line: None,
            original_line: None,
            side: None,
            commit: None,
            ..inline("c1", "", None, "2026-10-02T09:00:00Z")
        };
        let mut lost = inline("t9", "r_gone", None, "2026-10-02T12:00:00Z");
        lost.line = None;
        let conv = conversation(
            "pr_1",
            vec![
                review(
                    "r1",
                    ReviewState::ChangesRequested,
                    "Two things.",
                    "2026-10-02T10:00:00Z",
                ),
                // Only a reply to r1's thread: not an entry of its own.
                review("r2", ReviewState::Commented, "", "2026-10-02T11:00:00Z"),
                review("r3", ReviewState::Approved, "", "2026-10-02T13:00:00Z"),
            ],
            vec![
                plain,
                inline("t1", "r1", None, "2026-10-02T10:00:00Z"),
                inline("t2", "r1", None, "2026-10-02T10:00:01Z"),
                inline("t1b", "r2", Some("t1"), "2026-10-02T11:00:00Z"),
                lost,
            ],
        );
        let kinds: Vec<_> = conv.entries.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            [
                PrEntryKind::Comment,
                PrEntryKind::Review,
                PrEntryKind::Thread,
                PrEntryKind::Review
            ]
        );
        let r1 = &conv.entries[1];
        assert_eq!(r1.review.as_ref().unwrap().id, "r1");
        assert_eq!(r1.threads.len(), 2);
        let ids: Vec<_> = r1.threads[0]
            .comments
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(ids, ["t1", "t1b"]);
        assert_eq!(
            (r1.threads[0].path.as_str(), r1.threads[0].line),
            ("src/a.rs", Some(7))
        );
        assert!(conv.entries[2].threads[0].outdated);
        assert_eq!(
            conv.entries[3].review.as_ref().unwrap().state,
            ReviewState::Approved
        );
    }

    fn forge_pr(check: CheckStatus) -> ForgePr {
        ForgePr {
            title: "PR center".into(),
            body: String::new(),
            comments: None,
            default_branch: None,
            author: Some("claude".into()),
            state: PullRequestState::Open,
            head_ref: Some("f".into()),
            base_ref: Some("main".into()),
            head_sha: Some("abc".into()),
            mergeable: Mergeability::Mergeable,
            review_decision: ReviewDecision::None,
            additions: Some(1),
            deletions: Some(0),
            changed_files: Some(1),
            opened_at: Some("2026-10-02T00:00:00Z".into()),
            updated_at: None,
            merged_at: None,
            closed_at: None,
            checks: vec![Check {
                name: "CI / test".into(),
                status: check,
                conclusion: None,
                details_url: None,
                started_at: None,
                completed_at: None,
            }],
            reviews: vec![],
        }
    }

    fn types(store: &Store, after: i64) -> Vec<&'static str> {
        store
            .events_after(after, 100)
            .unwrap()
            .into_iter()
            .map(|e| e.event_type.as_str())
            .collect()
    }

    #[test]
    fn projects_task_pull_requests_and_forge_changes() {
        let (store, project_id) = store_with_pr();
        let before = store.last_seq().unwrap();
        let targets = store.ensure_pull_requests().unwrap();
        assert_eq!(targets.len(), 1);
        assert!(targets[0].never_synced);
        assert_eq!(types(&store, before), ["pr.updated"]);
        // Idempotent.
        let seq = store.last_seq().unwrap();
        store.ensure_pull_requests().unwrap();
        assert_eq!(store.last_seq().unwrap(), seq);

        let id = targets[0].id.clone();
        let pr = store
            .apply_forge_pr(&id, &forge_pr(CheckStatus::Pending))
            .unwrap();
        assert_eq!(pr.checks_state, ChecksState::Pending);
        assert_eq!(pr.repo, "quark-systems/quark");
        assert_eq!(pr.number, 16);
        assert_eq!(types(&store, seq), ["check.updated", "pr.updated"]);

        // An unchanged read emits nothing.
        let seq = store.last_seq().unwrap();
        store
            .apply_forge_pr(&id, &forge_pr(CheckStatus::Pending))
            .unwrap();
        assert_eq!(store.last_seq().unwrap(), seq);

        let mut green = forge_pr(CheckStatus::Success);
        green.reviews.push(Review {
            id: "R1".into(),
            author: Some("mattsanchez".into()),
            state: ReviewState::Approved,
            body: String::new(),
            submitted_at: None,
            commit: Some("abc".into()),
        });
        store.apply_forge_pr(&id, &green).unwrap();
        assert_eq!(
            types(&store, seq),
            ["check.updated", "review.updated", "pr.updated"]
        );
        let listed = store
            .list_pull_requests(Some(PullRequestState::Open), Some(&project_id))
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].checks_state, ChecksState::Passing);
        assert_eq!(listed[0].reviews.len(), 1);
        assert!(store
            .list_pull_requests(Some(PullRequestState::Merged), None)
            .unwrap()
            .is_empty());

        // A failed read is recorded once.
        let seq = store.last_seq().unwrap();
        store.set_pr_sync_error(&id, "gh: offline").unwrap();
        store.set_pr_sync_error(&id, "gh: offline").unwrap();
        assert_eq!(types(&store, seq), ["pr.updated"]);
        assert_eq!(
            store.get_pull_request(&id).unwrap().sync_error.as_deref(),
            Some("gh: offline")
        );

        // Merged pull requests are no longer read.
        let mut merged = green.clone();
        merged.state = PullRequestState::Merged;
        store.apply_forge_pr(&id, &merged).unwrap();
        assert!(store.get_pull_request(&id).unwrap().sync_error.is_none());
        assert!(store.ensure_pull_requests().unwrap().is_empty());
    }

    fn merged_pr() -> ForgePr {
        ForgePr {
            state: PullRequestState::Merged,
            merged_at: Some("2026-10-03T00:00:00Z".into()),
            closed_at: Some("2026-10-03T00:00:00Z".into()),
            ..forge_pr(CheckStatus::Success)
        }
    }

    fn pr_task(store: &Store, project_id: &str) -> quark_systems::Task {
        let tasks = store.list_tasks(project_id).unwrap();
        assert_eq!(tasks.len(), 1);
        tasks.into_iter().next().unwrap()
    }

    fn in_review_snapshot() -> FleetSnapshot {
        let et = EngineTask {
            id: "ship-pr-center".into(),
            title: "PR center".into(),
            kind: None,
            state: TaskState::InReview,
            state_note: Some(format!("PR {URL}")),
            state_source: None,
            harness: None,
            pull_request_url: Some(URL.into()),
            terminal: None,
            worktree: None,
            worktree_at: None,
            unhealthy: None,
            unanswered_since: None,
        };
        FleetSnapshot { tasks: vec![et] }
    }

    fn state_events(store: &Store, after: i64) -> Vec<quark_systems::Event> {
        store
            .events_after(after, 100)
            .unwrap()
            .into_iter()
            .filter(|e| e.event_type == EventType::TaskStateChanged)
            .collect()
    }

    #[test]
    fn a_merge_noticed_while_the_engine_task_exists_finishes_the_task() {
        let (store, project_id) = store_with_pr();
        let id = store.ensure_pull_requests().unwrap()[0].id.clone();
        let before = store.last_seq().unwrap();
        store.apply_forge_pr(&id, &merged_pr()).unwrap();
        let task = pr_task(&store, &project_id);
        assert_eq!(task.state, TaskState::Done);
        let events = state_events(&store, before);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload["task"]["state"], "done");
        assert_eq!(events[0].payload["previous_state"], "in_review");

        // The engine still reports the finished task as in review until it
        // cleans the task up; that must not move the task back or repeat the
        // event on every refresh.
        let after = store.last_seq().unwrap();
        store
            .apply_snapshot(&project_id, &in_review_snapshot())
            .unwrap();
        store
            .apply_snapshot(&project_id, &in_review_snapshot())
            .unwrap();
        assert_eq!(pr_task(&store, &project_id).state, TaskState::Done);
        assert!(state_events(&store, after).is_empty());
    }

    #[test]
    fn an_engine_task_that_disappears_after_its_merge_is_done() {
        let (store, project_id) = store_with_pr();
        let id = store.ensure_pull_requests().unwrap()[0].id.clone();
        store.apply_forge_pr(&id, &merged_pr()).unwrap();
        // The engine cleaned the task up: it is gone from the snapshot.
        store
            .apply_snapshot(&project_id, &FleetSnapshot { tasks: vec![] })
            .unwrap();
        assert_eq!(pr_task(&store, &project_id).state, TaskState::Done);
    }

    #[test]
    fn records_left_in_review_by_a_merge_correct_on_the_next_snapshot() {
        let (store, project_id) = store_with_pr();
        let id = store.ensure_pull_requests().unwrap()[0].id.clone();
        store.apply_forge_pr(&id, &merged_pr()).unwrap();
        // The state the app showed before this fix: merged PR, task still in
        // review with the worker's last status line, and never revisited.
        store
            .write(|tx, _| {
                tx.execute(
                    "UPDATE tasks SET state = 'in_review', state_note = ?1",
                    [format!("PR {URL}")],
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(pr_task(&store, &project_id).state, TaskState::InReview);
        let before = store.last_seq().unwrap();
        store
            .apply_snapshot(&project_id, &FleetSnapshot { tasks: vec![] })
            .unwrap();
        assert_eq!(pr_task(&store, &project_id).state, TaskState::Done);
        let events = state_events(&store, before);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload["previous_state"], "in_review");
    }

    #[test]
    fn a_task_that_disappears_without_a_merged_pr_keeps_its_state() {
        let (store, project_id) = store_with_pr();
        let id = store.ensure_pull_requests().unwrap()[0].id.clone();
        store
            .apply_forge_pr(&id, &forge_pr(CheckStatus::Success))
            .unwrap();
        store
            .apply_snapshot(&project_id, &FleetSnapshot { tasks: vec![] })
            .unwrap();
        assert_eq!(pr_task(&store, &project_id).state, TaskState::InReview);
    }

    #[test]
    fn standing_approval_selects_green_pull_requests() {
        let (store, project_id) = store_with_pr();
        let id = store.ensure_pull_requests().unwrap()[0].id.clone();
        store
            .apply_forge_pr(&id, &forge_pr(CheckStatus::Success))
            .unwrap();
        assert!(store.standing_approval_ready().unwrap().is_empty());
        store
            .update_project(
                &project_id,
                quark_systems::UpdateProject {
                    standing_approval: Some(true),
                    ..Default::default()
                },
            )
            .unwrap();
        let ready = store.standing_approval_ready().unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].task.as_ref().unwrap().engine_id, "ship-pr-center");

        store
            .apply_forge_pr(&id, &forge_pr(CheckStatus::Failure))
            .unwrap();
        assert!(store.standing_approval_ready().unwrap().is_empty());
    }
}
