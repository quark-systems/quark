//! A Project's automation: its inbox, trigger rules and away policy, served
//! from the slice 7 engine (`quark-triggers`).
//!
//! Until slice 7 switches on, that engine runs in shadow: rules and the
//! away policy are recorded and evaluated, but only what the engine would
//! have done is logged. A note posted to the inbox still reaches today's
//! coordinator, through the engine adapter.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use quark_core::ProjectId;
use quark_systems::{
    AwayOccasion, AwayPosture, AwayReach, AwayRoute, AwaySettings, ErrorBody, InboxMessage,
    NewInboxNote, ProjectAutomation, PutTriggerRule, TriggerRule, UpdateAwayPolicy,
};
use quark_triggers::trigger::Defined;
use quark_triggers::{AwayPolicy, Engine, Mode, Occasion, Posture, Reach, Route, Rule, RuleId};
use time::format_description::well_known::Rfc3339;

use super::{db, ApiError, AppState};
use crate::engine::WorkspaceRef;

/// Who the API records as making a change.
const BY: &str = "user";

fn engine(state: &AppState) -> Result<Arc<Engine>, ApiError> {
    state.triggers.clone().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "automation_off",
            "the automation engine is not running on this daemon",
        )
    })
}

fn core(e: quark_core::CoreError) -> ApiError {
    match e {
        quark_core::CoreError::Invalid(m) => ApiError::invalid(m),
        quark_core::CoreError::NotFound(_) => ApiError::not_found(),
        other => ApiError::internal(other.to_string()),
    }
}

/// The Project's inbox, rules and away policy.
#[utoipa::path(
    get,
    path = "/v1/projects/{id}/automation",
    tag = "projects",
    params(("id" = String, Path, description = "Project id")),
    responses(
        (status = 200, body = ProjectAutomation),
        (status = 404, body = ErrorBody),
        (status = 503, description = "The automation engine is off (`automation_off`)", body = ErrorBody)
    )
)]
pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ProjectAutomation>, ApiError> {
    let project = db(&state, move |s| s.get_project(&id)).await?;
    let engine = engine(&state)?;
    engine.refresh().await.map_err(core)?;
    Ok(Json(read(&engine, &ProjectId::new(project.id)).await))
}

async fn read(engine: &Engine, project: &ProjectId) -> ProjectAutomation {
    let inbox = engine
        .inbox(project)
        .await
        .into_iter()
        .map(|m| InboxMessage {
            id: m.id,
            channel: m.channel,
            from: m.from,
            body: m.body,
            at: m.at.format(&Rfc3339).unwrap_or_default(),
            task_id: m.task.map(|t| t.0),
        })
        .collect();
    let mut rules = Vec::new();
    for d in engine.rules(project).await {
        let fires = engine.fires(project, &d.rule.id).await as u32;
        rules.push(rule_view(d, fires));
    }
    ProjectAutomation {
        project_id: project.to_string(),
        acting: engine.mode() == Mode::Native,
        inbox,
        rules,
        away: away_view(engine, project).await,
    }
}

fn rule_view(d: Defined, fires: u32) -> TriggerRule {
    TriggerRule {
        id: d.rule.id.0.clone(),
        description: d.rule.description.clone(),
        enabled: d.rule.enabled,
        once: d.rule.once,
        when: serde_json::to_value(&d.rule.when).unwrap_or_default(),
        then: serde_json::to_value(&d.rule.then).unwrap_or_default(),
        defined_at: d.at.format(&Rfc3339).unwrap_or_default(),
        fires,
    }
}

async fn away_view(engine: &Engine, project: &ProjectId) -> AwaySettings {
    let policy = engine.policy(project).await;
    let mut routes = Vec::new();
    for posture in [Posture::Present, Posture::Away, Posture::Quiet] {
        for occasion in Occasion::ALL {
            let r = policy.route(posture, occasion);
            routes.push(AwayRoute {
                posture: posture_out(posture),
                occasion: occasion_out(occasion),
                wake: r.wake,
                user: reach_out(r.user),
                overridden: r != quark_triggers::away::default_route(posture, occasion),
            });
        }
    }
    AwaySettings {
        posture: posture_out(engine.posture(project).await),
        digest_secs: policy.digest_secs,
        routes,
        waiting: engine.digest(project).await.len() as u32,
        held: engine.held(project).await.len() as u32,
    }
}

/// Leave a note for the coordinator.
///
/// Until slice 7 switches on, the note goes to today's coordinator through
/// the engine, which wakes it at its next check; the automation engine
/// mirrors it into the inbox shortly after.
#[utoipa::path(
    post,
    path = "/v1/projects/{id}/inbox",
    tag = "projects",
    params(("id" = String, Path, description = "Project id")),
    request_body = NewInboxNote,
    responses(
        (status = 202, description = "Queued for the coordinator"),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "The Project has no workspace yet (`workspace_missing`)", body = ErrorBody),
        (status = 503, description = "The automation engine is off (`automation_off`)", body = ErrorBody)
    )
)]
pub async fn post_note(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(note): Json<NewInboxNote>,
) -> Result<StatusCode, ApiError> {
    if note.body.trim().is_empty() {
        return Err(ApiError::invalid("the note is empty"));
    }
    let project = db(&state, move |s| s.get_project(&id)).await?;
    let engine = engine(&state)?;
    if engine.mode() == Mode::Native {
        let message = quark_triggers::Inbound::note(BY, note.body);
        engine
            .receive(&ProjectId::new(project.id), message)
            .await
            .map_err(core)?;
        return Ok(StatusCode::ACCEPTED);
    }
    let Some(root) = project.workspace_path else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "workspace_missing",
            "the Project has no workspace yet; retry once it is provisioned",
        ));
    };
    let ws = WorkspaceRef {
        project_id: project.id,
        root: root.into(),
    };
    state.engine.inbox_note(&ws, &note.body).await?;
    Ok(StatusCode::ACCEPTED)
}

/// Add or replace a trigger rule.
#[utoipa::path(
    put,
    path = "/v1/projects/{id}/triggers/{rule}",
    tag = "projects",
    params(
        ("id" = String, Path, description = "Project id"),
        ("rule" = String, Path, description = "Rule id: 1-64 letters, digits, '-', '_' or '.'")
    ),
    request_body = PutTriggerRule,
    responses(
        (status = 200, body = TriggerRule),
        (status = 400, description = "The rule does not parse or would never run safely", body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 503, description = "The automation engine is off (`automation_off`)", body = ErrorBody)
    )
)]
pub async fn put_rule(
    State(state): State<AppState>,
    Path((id, rule_id)): Path<(String, String)>,
    Json(input): Json<PutTriggerRule>,
) -> Result<Json<TriggerRule>, ApiError> {
    let project = db(&state, move |s| s.get_project(&id)).await?;
    let engine = engine(&state)?;
    let when =
        serde_json::from_value(input.when).map_err(|e| ApiError::invalid(format!("when: {e}")))?;
    let then =
        serde_json::from_value(input.then).map_err(|e| ApiError::invalid(format!("then: {e}")))?;
    let mut rule = Rule::new(rule_id, when, then);
    rule.description = input.description;
    rule.enabled = input.enabled;
    rule.once = input.once;
    let pid = ProjectId::new(project.id);
    let rid = rule.id.clone();
    engine.define(&pid, rule, BY).await.map_err(core)?;
    let defined = engine
        .rules(&pid)
        .await
        .into_iter()
        .find(|d| d.rule.id == rid)
        .ok_or_else(|| ApiError::internal("the rule was not recorded"))?;
    let fires = engine.fires(&pid, &rid).await as u32;
    Ok(Json(rule_view(defined, fires)))
}

/// Remove a trigger rule.
#[utoipa::path(
    delete,
    path = "/v1/projects/{id}/triggers/{rule}",
    tag = "projects",
    params(
        ("id" = String, Path, description = "Project id"),
        ("rule" = String, Path, description = "Rule id")
    ),
    responses(
        (status = 204, description = "Removed"),
        (status = 404, body = ErrorBody),
        (status = 503, description = "The automation engine is off (`automation_off`)", body = ErrorBody)
    )
)]
pub async fn delete_rule(
    State(state): State<AppState>,
    Path((id, rule_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let project = db(&state, move |s| s.get_project(&id)).await?;
    let engine = engine(&state)?;
    engine
        .remove(&ProjectId::new(project.id), &RuleId::new(rule_id), BY)
        .await
        .map_err(core)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Replace the Project's away policy.
///
/// `routes` lists the cells that differ from the default; every other cell
/// goes back to its default. A policy where a decision or a failure would
/// reach no one is refused.
#[utoipa::path(
    put,
    path = "/v1/projects/{id}/away/policy",
    tag = "projects",
    params(("id" = String, Path, description = "Project id")),
    request_body = UpdateAwayPolicy,
    responses(
        (status = 200, body = AwaySettings),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 503, description = "The automation engine is off (`automation_off`)", body = ErrorBody)
    )
)]
pub async fn put_policy(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<UpdateAwayPolicy>,
) -> Result<Json<AwaySettings>, ApiError> {
    let project = db(&state, move |s| s.get_project(&id)).await?;
    let engine = engine(&state)?;
    let mut policy = AwayPolicy {
        digest_secs: input.digest_secs,
        ..AwayPolicy::default()
    };
    for r in input.routes {
        let (posture, occasion) = (posture_in(r.posture), occasion_in(r.occasion));
        let route = Route::new(r.wake, reach_in(r.user));
        if route != quark_triggers::away::default_route(posture, occasion) {
            policy.set(posture, occasion, route);
        }
    }
    let pid = ProjectId::new(project.id);
    engine.set_policy(&pid, policy, BY).await.map_err(core)?;
    Ok(Json(away_view(&engine, &pid).await))
}

fn posture_out(p: Posture) -> AwayPosture {
    match p {
        Posture::Present => AwayPosture::Present,
        Posture::Away => AwayPosture::Away,
        Posture::Quiet => AwayPosture::Quiet,
    }
}

fn posture_in(p: AwayPosture) -> Posture {
    match p {
        AwayPosture::Present => Posture::Present,
        AwayPosture::Away => Posture::Away,
        AwayPosture::Quiet => Posture::Quiet,
    }
}

fn reach_out(r: Reach) -> AwayReach {
    match r {
        Reach::Silent => AwayReach::Silent,
        Reach::Digest => AwayReach::Digest,
        Reach::Notify => AwayReach::Notify,
        Reach::Hold => AwayReach::Hold,
    }
}

fn reach_in(r: AwayReach) -> Reach {
    match r {
        AwayReach::Silent => Reach::Silent,
        AwayReach::Digest => Reach::Digest,
        AwayReach::Notify => Reach::Notify,
        AwayReach::Hold => Reach::Hold,
    }
}

const OCCASIONS: [(Occasion, AwayOccasion); 10] = [
    (Occasion::Progress, AwayOccasion::Progress),
    (Occasion::Paused, AwayOccasion::Paused),
    (Occasion::Done, AwayOccasion::Done),
    (Occasion::Decision, AwayOccasion::Decision),
    (Occasion::Blocked, AwayOccasion::Blocked),
    (Occasion::Failed, AwayOccasion::Failed),
    (Occasion::Stale, AwayOccasion::Stale),
    (Occasion::Inbound, AwayOccasion::Inbound),
    (Occasion::TriggerFired, AwayOccasion::TriggerFired),
    (Occasion::TriggerFailed, AwayOccasion::TriggerFailed),
];

fn occasion_out(o: Occasion) -> AwayOccasion {
    OCCASIONS
        .iter()
        .find(|(n, _)| *n == o)
        .expect("every occasion")
        .1
}

fn occasion_in(o: AwayOccasion) -> Occasion {
    OCCASIONS
        .iter()
        .find(|(_, a)| *a == o)
        .expect("every occasion")
        .0
}

/// This module's endpoints, merged into the served document by
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(get, post_note, put_rule, delete_rule, put_policy))]
pub(super) struct Api;

/// This module's routes, merged into the `/v1` router.
pub(super) fn router() -> axum::Router<AppState> {
    use axum::routing::{get, post, put};
    axum::Router::new()
        .route("/v1/projects/{id}/automation", get(self::get))
        .route("/v1/projects/{id}/inbox", post(post_note))
        .route(
            "/v1/projects/{id}/triggers/{rule}",
            put(put_rule).delete(delete_rule),
        )
        .route("/v1/projects/{id}/away/policy", put(put_policy))
}
