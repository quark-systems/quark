//! `GET /v1/tasks/{id}/brief`: the issue a task was dispatched for, the
//! brief its worker received, and the delivery contract recorded at spawn.
//!
//! The issue is not recorded with the task. The coordinator is told to put
//! an issue's id in the title and instructions of a task it starts for it,
//! so the first id the Project's Beads database knows, in the title, then
//! the engine's task id, then the brief, is the task's issue.

use std::collections::HashSet;

use quark_systems::{DispatchBrief, Issue, TaskBrief};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::api::{db, ApiError, AppState};
use crate::engine::{EngineBrief, WorkspaceRef};

/// Headings of the section restating what the person asked for; briefs
/// written before the native engine use the second.
const INTENT: &[&str] = &["intent", "captain's intent"];
/// Headings of the section where the coordinator specifies the work.
const SPEC: &[&str] = &["spec", "firstmate spec"];

pub async fn read(state: &AppState, id: String) -> Result<TaskBrief, ApiError> {
    let task = {
        let id = id.clone();
        db(state, move |s| s.get_task(&id)).await?
    };
    let target = {
        let id = id.clone();
        db(state, move |s| s.task_target(&id)).await?
    };
    let dispatched = {
        let id = id.clone();
        db(state, move |s| s.list_dispatch(&id)).await?
    };

    let engine = match &target.workspace_path {
        Some(root) => {
            let ws = WorkspaceRef {
                project_id: target.project_id.clone(),
                root: root.into(),
            };
            match state.engine.brief(&ws, &target.engine_id).await {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(task = %id, error = %e, "reading a task's brief failed");
                    None
                }
            }
        }
        None => None,
    }
    .unwrap_or_default();

    let EngineBrief {
        text,
        launch,
        written_at,
        delivery,
        merges_without_asking,
    } = engine;

    let issue = match state
        .beads
        .issues(&state.layout.home, &target.project_id)
        .await
    {
        Ok(issues) => {
            let sources = [
                Some(task.title.as_str()),
                Some(target.engine_id.as_str()),
                text.as_deref(),
            ];
            issue_for(issues, sources.into_iter().flatten())
        }
        Err(e) => {
            tracing::debug!(task = %id, error = %e, "no issues to match a task's brief to");
            None
        }
    };

    let dispatched_at = dispatched
        .iter()
        .map(|d| d.recorded_at.clone())
        .min()
        .or_else(|| written_at.and_then(rfc3339));
    let has_dispatch = dispatched_at.is_some() || text.is_some();

    Ok(TaskBrief {
        task_id: id,
        issue,
        brief: text.map(|markdown| {
            let (intent, spec) = sections(&markdown);
            DispatchBrief {
                markdown,
                intent,
                spec,
                launch,
            }
        }),
        dispatched_at,
        dispatched_by: has_dispatch.then(|| "coordinator".to_string()),
        delivery,
        merges_without_asking,
        pull_request_url: task.pull_request_url,
    })
}

fn rfc3339(secs: i64) -> Option<String> {
    OffsetDateTime::from_unix_timestamp(secs)
        .ok()?
        .format(&Rfc3339)
        .ok()
}

/// The first issue whose id appears as a word in `sources`, searched in
/// order.
pub fn issue_for<'a>(issues: Vec<Issue>, sources: impl Iterator<Item = &'a str>) -> Option<Issue> {
    let ids: HashSet<&str> = issues.iter().map(|i| i.id.as_str()).collect();
    let found = sources
        .flat_map(words)
        .find(|w| ids.contains(w))?
        .to_string();
    issues.into_iter().find(|i| i.id == found)
}

/// Runs of id characters, without the trailing dots of a sentence.
fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .map(|w| w.trim_end_matches('.'))
        .filter(|w| !w.is_empty())
}

/// The intent and spec sections of a brief: each heading's body, up to the
/// next heading at its level or above.
pub fn sections(markdown: &str) -> (Option<String>, Option<String>) {
    (section(markdown, INTENT), section(markdown, SPEC))
}

fn section(markdown: &str, titles: &[&str]) -> Option<String> {
    let mut lines = markdown.lines();
    let level = lines.by_ref().find_map(|l| {
        let (level, text) = heading(l)?;
        titles.contains(&normalize(text).as_str()).then_some(level)
    })?;
    let body: Vec<&str> = lines
        .take_while(|l| heading(l).is_none_or(|(n, _)| n > level))
        .collect();
    let body = body.join("\n").trim().to_string();
    (!body.is_empty()).then_some(body)
}

/// An ATX heading's level and text.
fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|&b| b == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &line[level..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    Some((level, rest.trim().trim_end_matches('#').trim()))
}

fn normalize(s: &str) -> String {
    s.replace('\u{2019}', "'").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BRIEF: &str = "You are a crewmate.\n\n# Task\n## Captain's intent\nFix the pty bug.\n\nAs its own PR.\n\n## Firstmate spec\n- Confirm first.\n### Gates\nRun them.\n\n# Report\nSay done.\n";

    fn issue(id: &str) -> Issue {
        Issue {
            id: id.into(),
            title: id.into(),
            description: String::new(),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            labels: vec![],
            assignee: None,
            owner: None,
            created_by: None,
            created_at: String::new(),
            updated_at: String::new(),
            closed_at: None,
            external_ref: None,
            blocked_by: vec![],
            ready: true,
            blocked: false,
        }
    }

    #[test]
    fn finds_the_intent_and_spec_sections() {
        let (intent, spec) = sections(BRIEF);
        assert_eq!(
            intent.as_deref(),
            Some("Fix the pty bug.\n\nAs its own PR.")
        );
        assert_eq!(
            spec.as_deref(),
            Some("- Confirm first.\n### Gates\nRun them."),
            "a deeper heading stays in the section"
        );
    }

    #[test]
    fn a_brief_without_the_sections_has_none() {
        assert_eq!(sections("Fix the parser.\n## Notes\nx"), (None, None));
        assert_eq!(
            sections("## Intent\nDo it.\n## Spec\nLike so.\n"),
            (Some("Do it.".into()), Some("Like so.".into()))
        );
        assert_eq!(
            sections("## Captain’s Intent\n\n## Firstmate spec\n").0,
            None
        );
        assert_eq!(
            sections("## Captain’s Intent ##\nDo it.").0.as_deref(),
            Some("Do it.")
        );
    }

    #[test]
    fn the_issue_is_the_first_known_id_in_order() {
        let issues = vec![issue("quark-ctx"), issue("quark-ouo"), issue("quark-a1.2")];
        let found =
            |sources: &[&str]| issue_for(issues.clone(), sources.iter().copied()).map(|i| i.id);
        assert_eq!(
            found(&["Brief tab (quark-ouo)", "see quark-ctx"]).as_deref(),
            Some("quark-ouo")
        );
        assert_eq!(
            found(&["Brief tab", "Work quark-ctx."]).as_deref(),
            Some("quark-ctx")
        );
        assert_eq!(
            found(&["child quark-a1.2, then"]).as_deref(),
            Some("quark-a1.2")
        );
        assert_eq!(found(&["quark-ctxx and xquark-ctx"]), None);
        assert_eq!(found(&[]), None);
    }
}
