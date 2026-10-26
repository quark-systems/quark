//! Folds Projects, workers, decisions and pull requests into the status
//! summary (`GET /v1/status`): what waits on you, oldest first, and each
//! Project's workers by tone. The desktop app, the menubar and widgets all
//! read this one fold, so "needs you" means the same thing everywhere.

use std::collections::{HashMap, HashSet};

use quark_systems::{
    AttentionItem, AttentionKind, ChecksState, Decision, DecisionState, Project, ProjectSummary,
    PullRequest, PullRequestState, StatusView, Task, TaskState, Tone, ToneCounts, WorkerSummary,
};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

/// How long a failed worker stays listed for you to look at.
pub const FAILED_SHOWN: Duration = Duration::days(3);

/// Whether a worker is listed: unfinished, or failed within [`FAILED_SHOWN`].
/// Cleared workers never are.
fn listed(t: &Task, now: OffsetDateTime) -> bool {
    if t.cleared_at.is_some() {
        return false;
    }
    match t.state {
        TaskState::Done => false,
        TaskState::Failed => {
            OffsetDateTime::parse(&t.updated_at, &Rfc3339).is_ok_and(|at| now - at < FAILED_SHOWN)
        }
        _ => true,
    }
}

/// The status of every unarchived Project in `projects`, given all their
/// tasks, open decisions and pull requests.
pub fn fold(
    projects: &[Project],
    tasks: &[Task],
    decisions: &[Decision],
    prs: &[PullRequest],
    now: OffsetDateTime,
) -> StatusView {
    let names: HashMap<&str, &str> = projects
        .iter()
        .filter(|p| p.archived_at.is_none())
        .map(|p| (p.id.as_str(), p.name.as_str()))
        .collect();
    let item =
        |key: String, kind, target_id: &str, title: String, project_id: &str, since: &str| {
            names.get(project_id).map(|name| AttentionItem {
                key,
                kind,
                target_id: target_id.to_string(),
                title,
                project_id: project_id.to_string(),
                project_name: name.to_string(),
                since: since.to_string(),
            })
        };

    let mut attention = Vec::new();
    // A worker waiting on a decision is reached through that decision.
    let mut asked = HashSet::new();
    for d in decisions.iter().filter(|d| d.state == DecisionState::Open) {
        if let Some(t) = &d.task_id {
            asked.insert(t.as_str());
        }
        attention.extend(item(
            format!("decision:{}", d.id),
            AttentionKind::Decision,
            &d.id,
            d.question.clone(),
            &d.project_id,
            &d.opened_at,
        ));
    }
    let open =
        |p: &PullRequest| matches!(p.state, PullRequestState::Open | PullRequestState::Draft);
    for p in prs
        .iter()
        .filter(|p| open(p) && p.checks_state == ChecksState::Failing)
    {
        let since = p
            .updated_at
            .as_deref()
            .or(p.opened_at.as_deref())
            .unwrap_or("");
        attention.extend(item(
            format!("pr:{}", p.id),
            AttentionKind::PullRequest,
            &p.id,
            p.title
                .clone()
                .unwrap_or_else(|| format!("{}#{}", p.repo, p.number)),
            &p.project_id,
            since,
        ));
    }
    for t in tasks {
        let stuck = matches!(t.state, TaskState::Failed | TaskState::Blocked);
        if !stuck || asked.contains(t.id.as_str()) || t.cleared_at.is_some() {
            continue;
        }
        attention.extend(item(
            format!("worker:{}", t.id),
            AttentionKind::Worker,
            &t.id,
            t.title.clone(),
            &t.project_id,
            &t.updated_at,
        ));
    }
    attention.sort_by(|a, b| a.since.cmp(&b.since).then_with(|| a.key.cmp(&b.key)));

    let mut counts = ToneCounts::default();
    let mut summaries: Vec<ProjectSummary> = projects
        .iter()
        .filter(|p| p.archived_at.is_none())
        .map(|p| {
            let mut last = p.updated_at.clone();
            let mut bump = |ts: &str| {
                if ts > last.as_str() {
                    last = ts.to_string();
                }
            };
            let mut workers: Vec<&Task> = tasks
                .iter()
                .filter(|t| t.project_id == p.id && listed(t, now))
                .collect();
            workers.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            let mut c = ToneCounts::default();
            let workers: Vec<WorkerSummary> = workers
                .into_iter()
                .map(|t| {
                    let tone = Tone::of(t.state);
                    c.add(tone);
                    bump(&t.updated_at);
                    WorkerSummary {
                        id: t.id.clone(),
                        title: t.title.clone(),
                        state: t.state,
                        tone,
                        note: t.state_note.clone(),
                        harness: t.harness.clone(),
                        pull_request_url: t.pull_request_url.clone(),
                        updated_at: t.updated_at.clone(),
                    }
                })
                .collect();
            let mut open_decisions = 0;
            for d in decisions.iter().filter(|d| d.project_id == p.id) {
                bump(&d.opened_at);
                if let Some(at) = &d.answered_at {
                    bump(at);
                }
                if d.state == DecisionState::Open {
                    open_decisions += 1;
                }
            }
            let (mut open_prs, mut failing) = (0, 0);
            for pr in prs.iter().filter(|pr| pr.project_id == p.id) {
                if let Some(at) = pr.updated_at.as_deref().or(pr.opened_at.as_deref()) {
                    bump(at);
                }
                if open(pr) {
                    open_prs += 1;
                    if pr.checks_state == ChecksState::Failing {
                        failing += 1;
                    }
                }
            }
            counts.merge(c);
            ProjectSummary {
                id: p.id.clone(),
                name: p.name.clone(),
                status: p.status,
                status_detail: p.status_detail.clone(),
                attention: attention.iter().filter(|i| i.project_id == p.id).count() as u32,
                open_decisions,
                open_pull_requests: open_prs,
                failing_pull_requests: failing,
                counts: c,
                workers,
                last_activity: last,
            }
        })
        .collect();
    summaries.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));

    StatusView {
        attention,
        counts,
        projects: summaries,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quark_systems::{DecisionBrief, ProjectStatus};

    fn now() -> OffsetDateTime {
        OffsetDateTime::parse("2026-10-08T12:00:00Z", &Rfc3339).unwrap()
    }

    fn project(id: &str, name: &str, archived: bool) -> Project {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": name, "goal": null, "workspace_path": null, "status": "ready",
            "status_detail": null, "repos": [], "agent_config": null, "standing_approval": false,
            "archived_at": if archived { Some("2026-10-07T00:00:00Z") } else { None },
            "created_at": "2026-10-01T00:00:00Z", "updated_at": "2026-10-01T00:00:00Z",
        }))
        .unwrap()
    }

    fn task(id: &str, project: &str, state: TaskState, updated_at: &str) -> Task {
        serde_json::from_value(serde_json::json!({
            "id": id, "project_id": project, "title": format!("T {id}"), "kind": null,
            "state": state, "state_note": null, "harness": "claude-code", "pull_request_url": null,
            "created_at": "2026-10-01T00:00:00Z", "updated_at": updated_at,
        }))
        .unwrap()
    }

    fn decision(
        id: &str,
        project: &str,
        task: Option<&str>,
        state: DecisionState,
        opened_at: &str,
    ) -> Decision {
        let mut d: Decision = serde_json::from_value(serde_json::json!({
            "id": id, "number": 1, "project_id": project, "task_id": task,
            "question": format!("Q {id}"), "state": state, "brief": DecisionBrief::default(),
            "answer": null, "answered_by": null, "opened_at": opened_at, "answered_at": null,
        }))
        .unwrap();
        d.state = state;
        d
    }

    fn pr(
        id: &str,
        project: &str,
        checks: ChecksState,
        state: PullRequestState,
        updated_at: &str,
    ) -> PullRequest {
        let mut p: PullRequest = serde_json::from_value(serde_json::json!({
            "id": id, "project_id": project, "task_id": null, "url": "", "provider": "github",
            "repo": "o/r", "number": 7, "title": format!("PR {id}"), "body": null, "author": null,
            "state": "open", "head_ref": null, "base_ref": null, "head_sha": null,
            "mergeable": "unknown", "checks_state": "none",
            "review_decision": "none", "additions": null, "deletions": null,
            "changed_files": null, "checks": [], "reviews": [], "evidence": null,
            "opened_at": null, "updated_at": updated_at, "merged_at": null, "closed_at": null,
            "synced_at": null, "sync_error": null,
        }))
        .unwrap();
        p.checks_state = checks;
        p.state = state;
        p
    }

    #[test]
    fn attention_holds_open_decisions_red_prs_and_stuck_workers_oldest_first() {
        // The cases of app/src/shell/attention.test.ts, which this fold replaces.
        let v = fold(
            &[project("p", "Quark", false)],
            &[
                task("t1", "p", TaskState::Failed, "2026-10-08T04:00:00Z"),
                task("t2", "p", TaskState::Blocked, "2026-10-08T00:30:00Z"),
                task("t3", "p", TaskState::Running, "2026-10-08T00:00:00Z"),
            ],
            &[
                decision(
                    "d1",
                    "p",
                    Some("t2"),
                    DecisionState::Open,
                    "2026-10-08T03:00:00Z",
                ),
                decision(
                    "d2",
                    "p",
                    None,
                    DecisionState::Answered,
                    "2026-10-08T01:00:00Z",
                ),
            ],
            &[
                pr(
                    "p1",
                    "p",
                    ChecksState::Failing,
                    PullRequestState::Open,
                    "2026-10-08T02:00:00Z",
                ),
                pr(
                    "p2",
                    "p",
                    ChecksState::Passing,
                    PullRequestState::Open,
                    "2026-10-08T00:00:00Z",
                ),
                pr(
                    "p3",
                    "p",
                    ChecksState::Failing,
                    PullRequestState::Merged,
                    "2026-10-08T00:00:00Z",
                ),
            ],
            now(),
        );
        let keys: Vec<&str> = v.attention.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(keys, ["pr:p1", "decision:d1", "worker:t1"]);
        assert_eq!(v.attention[1].kind, AttentionKind::Decision);
        assert_eq!(v.attention[1].target_id, "d1");
        assert_eq!(v.attention[1].project_name, "Quark");

        let p = &v.projects[0];
        assert_eq!(p.attention, 3);
        assert_eq!(p.open_decisions, 1);
        assert_eq!(p.open_pull_requests, 2);
        assert_eq!(p.failing_pull_requests, 1);
        assert_eq!(
            p.counts,
            ToneCounts {
                busy: 1,
                needs_you: 1,
                failed: 1,
                ..Default::default()
            }
        );
        let ids: Vec<&str> = p.workers.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(ids, ["t1", "t2", "t3"], "most recent activity first");
        assert_eq!(p.last_activity, "2026-10-08T04:00:00Z");
        assert_eq!(v.counts, p.counts);
    }

    #[test]
    fn leaves_out_archived_projects_done_cleared_and_old_failed_workers() {
        let mut cleared = task("t4", "p", TaskState::Failed, "2026-10-08T11:00:00Z");
        cleared.cleared_at = Some("2026-10-08T11:30:00Z".into());
        let v = fold(
            &[
                project("p", "B", false),
                project("a", "Old", true),
                project("q", "A", false),
            ],
            &[
                task("t1", "p", TaskState::Done, "2026-10-08T11:00:00Z"),
                task("t2", "p", TaskState::Failed, "2026-10-01T00:00:00Z"),
                task("t3", "p", TaskState::InReview, "2026-10-08T10:00:00Z"),
                cleared,
                task("t5", "a", TaskState::Running, "2026-10-08T10:00:00Z"),
            ],
            &[decision(
                "d1",
                "a",
                None,
                DecisionState::Open,
                "2026-10-08T03:00:00Z",
            )],
            &[],
            now(),
        );
        assert_eq!(
            v.projects
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            ["A", "B"]
        );
        let b = &v.projects[1];
        assert_eq!(
            b.workers.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(),
            ["t3"]
        );
        assert_eq!(b.workers[0].tone, Tone::Ready);
        // An old failed worker still waits on you until it is cleared.
        let keys: Vec<&str> = v.attention.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(keys, ["worker:t2"]);
        assert_eq!(v.counts.ready, 1);
        assert_eq!(v.projects[0].status, ProjectStatus::Ready);
    }
}
