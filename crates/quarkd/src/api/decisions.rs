//! The decision log's reads and standing rules: one decision by id, the
//! rules answers made, revoking a rule, and logging a decision an agent made
//! under one.
//!
//! A Project's standing approval to merge green pull requests is listed as
//! one more rule (`merge-approval:<project id>`), and revoking it turns
//! standing approval off.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::Json;
use quark_systems::{
    Decision, ErrorBody, Project, RecordRuleDecision, RuleKind, StandingRule, UpdateProject,
};
use serde::Deserialize;
use utoipa::IntoParams;

use super::routes::{daemon_user, set_standing_approval, trimmed, MAX_WHY_BYTES};
use super::{db, ApiError, AppState};

/// The rule id standing approval is listed under.
const MERGE_APPROVAL_PREFIX: &str = "merge-approval:";

/// One decision.
#[utoipa::path(
    get,
    path = "/v1/decisions/{id}",
    tag = "decisions",
    params(("id" = String, Path, description = "Decision id")),
    responses(
        (status = 200, body = Decision),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn get_decision(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Decision>, ApiError> {
    Ok(Json(db(&state, move |s| s.get_decision(&id)).await?))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RuleQuery {
    /// Only this Project's rules.
    pub project_id: Option<String>,
    /// Include revoked rules.
    #[serde(default)]
    pub include_revoked: bool,
}

/// The standing approval rule for `p`, when it is on.
fn merge_approval(p: &Project) -> Option<StandingRule> {
    p.standing_approval.then(|| StandingRule {
        id: format!("{MERGE_APPROVAL_PREFIX}{}", p.id),
        project_id: p.id.clone(),
        kind: RuleKind::MergeApproval,
        text: "Merge green pull requests without asking.".into(),
        ..Default::default()
    })
}

/// Standing rules: those made from answers, oldest first, then each
/// Project's standing approval while it is on.
#[utoipa::path(
    get,
    path = "/v1/rules",
    tag = "decisions",
    params(RuleQuery),
    responses((status = 200, body = Vec<StandingRule>))
)]
pub async fn list_rules(
    State(state): State<AppState>,
    Query(q): Query<RuleQuery>,
) -> Result<Json<Vec<StandingRule>>, ApiError> {
    Ok(Json(
        db(&state, move |s| {
            let mut rules = s.list_rules(q.project_id.as_deref(), q.include_revoked)?;
            let projects = s.list_projects()?;
            rules.extend(
                projects
                    .iter()
                    .filter(|p| q.project_id.as_deref().is_none_or(|id| id == p.id))
                    .filter_map(merge_approval),
            );
            Ok(rules)
        })
        .await?,
    ))
}

/// Dispatches `POST /v1/rules/{id}:revoke`.
pub async fn rule_action(
    state: State<AppState>,
    Path(target): Path<String>,
    _body: Bytes,
) -> Result<Json<StandingRule>, ApiError> {
    match target.rsplit_once(':') {
        Some((id, "revoke")) => revoke_rule(state, Path(id.to_string())).await,
        _ => Err(ApiError::not_found()),
    }
}

/// Revoke a standing rule.
///
/// The rule decides nothing from now on; its decisions stay in the log.
/// Revoking a Project's standing approval rule turns standing approval off.
/// The rule also arrives as a `rule.updated` event.
#[utoipa::path(
    post,
    path = "/v1/rules/{id}:revoke",
    tag = "decisions",
    params(("id" = String, Path, description = "Rule id")),
    responses(
        (status = 200, body = StandingRule),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`workspace_missing`: standing approval needs the Project's workspace"),
        (status = 502, body = ErrorBody, description = "The engine refused or failed the call")
    )
)]
pub async fn revoke_rule(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<StandingRule>, ApiError> {
    if let Some(project_id) = id.strip_prefix(MERGE_APPROVAL_PREFIX) {
        let pid = project_id.to_string();
        let project = db(&state, move |s| s.get_project(&pid)).await?;
        let mut rule = merge_approval(&project).ok_or_else(ApiError::not_found)?;
        set_standing_approval(&state, &project, false).await?;
        let pid = project.id.clone();
        db(&state, move |s| {
            s.update_project(
                &pid,
                UpdateProject {
                    standing_approval: Some(false),
                    ..Default::default()
                },
            )
        })
        .await?;
        rule.revoked_at = Some(crate::now_rfc3339());
        rule.revoked_by = Some(daemon_user());
        return Ok(Json(rule));
    }
    let by = daemon_user();
    Ok(Json(db(&state, move |s| s.revoke_rule(&id, &by)).await?))
}

/// Log a decision an agent made under a standing rule.
///
/// The decision is recorded as answered by the agent through the rule (and
/// acted on when `outcome` is given), so the log shows it as decided under
/// the rule. It arrives as `decision.opened` and `decision.answered` (and
/// `decision.acted`) events, and the rule's count as `rule.updated`.
#[utoipa::path(
    post,
    path = "/v1/rules/{id}/decisions",
    tag = "decisions",
    params(("id" = String, Path, description = "Rule id")),
    request_body = RecordRuleDecision,
    responses(
        (status = 200, body = Decision),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`conflict`: the rule was revoked")
    )
)]
pub async fn record_rule_decision(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(mut input): Json<RecordRuleDecision>,
) -> Result<Json<Decision>, ApiError> {
    if id.starts_with(MERGE_APPROVAL_PREFIX) {
        return Err(ApiError::invalid(
            "standing approval logs its merges in the PR center",
        ));
    }
    let (Some(question), Some(answer), Some(decided_by)) = (
        trimmed(Some(&input.question)),
        trimmed(Some(&input.answer)),
        trimmed(Some(&input.decided_by)),
    ) else {
        return Err(ApiError::invalid(
            "question, answer and decided_by must be non-empty",
        ));
    };
    if [&question, &answer].iter().any(|t| t.len() > MAX_WHY_BYTES) {
        return Err(ApiError::invalid(
            "question and answer must be at most 2048 bytes",
        ));
    }
    input.rule_id = id;
    input.question = question;
    input.answer = answer;
    input.decided_by = decided_by;
    input.outcome = trimmed(input.outcome.as_deref());
    Ok(Json(
        db(&state, move |s| s.record_rule_decision(&input)).await?,
    ))
}

/// This module's endpoints, merged into the served document by
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(get_decision, list_rules, revoke_rule, record_rule_decision),
    components(schemas(
        quark_systems::RuleKind,
        quark_systems::DecisionBrief,
        quark_systems::DecisionOption,
        quark_systems::EvidenceLink,
    ))
)]
pub(super) struct Api;

/// This module's routes, merged into the `/v1` router.
pub(super) fn router() -> axum::Router<AppState> {
    use axum::routing::{get, post};
    // `GET /v1/decisions/{id}` is routed with the decision actions in
    // `routes.rs`, which own that path.
    axum::Router::new()
        .route("/v1/rules", get(list_rules))
        // POST serves the custom method `/v1/rules/{id}:revoke`.
        .route("/v1/rules/{id}", post(rule_action))
        .route("/v1/rules/{id}/decisions", post(record_rule_decision))
}
