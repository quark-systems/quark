//! The PR center's projection: `pull_requests`, `checks` and `reviews`.
//!
//! Which pull requests exist comes from the tasks (the engine records the URL
//! each task opened); everything else is what the forge last reported. Each
//! change emits `pr.updated`, `check.updated` or `review.updated` in the same
//! transaction.

use base64::Engine as _;
use quark_systems::{
    Check, CheckStatus, CheckUpdated, ChecksState, EventType, Evidence, Mergeability, PullRequest,
    PullRequestState, Review, ReviewDecision, ReviewState, ReviewUpdated,
};
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};
use serde::de::DeserializeOwned;
use serde::Serialize;

use super::{append_event, new_id, Result, Store, StoreError, TaskTarget};
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

const PR_COLUMNS: &str = "id, project_id, task_id, url, provider, repo, number, title, author, \
    state, head_ref, base_ref, head_sha, mergeable, checks_state, review_decision, additions, \
    deletions, changed_files, opened_at, updated_at, merged_at, closed_at, synced_at, sync_error, \
    evidence";

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
                     closed_at = ?17, synced_at = ?18, sync_error = NULL
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

            let new = get_pr(tx, id)?;
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
                 ORDER BY pr.created_at, pr.id",
            )?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })?;
        ids.iter().map(|id| self.pull_request_owner(id)).collect()
    }
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
                    }],
                },
            )
            .unwrap();
        (store, p.id)
    }

    fn forge_pr(check: CheckStatus) -> ForgePr {
        ForgePr {
            title: "PR center".into(),
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
