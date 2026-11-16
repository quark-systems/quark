use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use quark_systems::{
    Account, AccountChangeAction, AccountChangeApplied, AccountChangeOutcome, AccountChangePlan,
    AccountDisposition, ApplyAccountChange, CreateAccount, ErrorBody, PlanAccountChange,
    SetAccountKey, UpdateAccount,
};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use utoipa::IntoParams;

use super::{db, ApiError, AppState};
use crate::account_change::{self, ProjectProfiles};
use crate::{classifier, crew_dispatch, project_repo};

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AccountQuery {
    /// Only this harness's accounts, e.g. `claude-code`.
    pub harness: Option<String>,
    /// Read every listed account's quota now (one read each)
    /// instead of returning the last reading.
    #[serde(default)]
    pub refresh: bool,
}

/// Every account per harness, with credential health from the harness
/// adapter and the latest quota reading. A harness with accounts lists its
/// default account first.
#[utoipa::path(
    get,
    path = "/v1/accounts",
    tag = "accounts",
    params(AccountQuery),
    responses((status = 200, body = Vec<Account>))
)]
pub async fn list(
    State(state): State<AppState>,
    Query(q): Query<AccountQuery>,
) -> Result<Json<Vec<Account>>, ApiError> {
    if q.refresh {
        state.accounts.refresh_quota(None).await?;
    }
    let mut accounts = state.accounts.list().await?;
    if let Some(h) = q.harness {
        accounts.retain(|a| a.harness == h);
    }
    Ok(Json(accounts))
}

/// One account.
#[utoipa::path(
    get,
    path = "/v1/accounts/{id}",
    tag = "accounts",
    params(("id" = String, Path, description = "Account id")),
    responses((status = 200, body = Account), (status = 404, body = ErrorBody))
)]
pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Account>, ApiError> {
    Ok(Json(state.accounts.get(&id).await?))
}

/// Add an account: a config directory for a harness that supports more than
/// one. Its quota is read right away and arrives as `account.quota_changed`.
#[utoipa::path(
    post,
    path = "/v1/accounts",
    tag = "accounts",
    request_body = CreateAccount,
    responses(
        (status = 201, body = Account),
        (status = 400, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "The directory is already an account")
    )
)]
pub async fn create(
    State(state): State<AppState>,
    Json(input): Json<CreateAccount>,
) -> Result<(StatusCode, Json<Account>), ApiError> {
    let account = state.accounts.create(input).await?;
    let (accounts, id) = (state.accounts.clone(), account.id.clone());
    tokio::spawn(async move {
        if let Err(e) = accounts.refresh_quota(Some(&id)).await {
            tracing::warn!(account = %id, error = %e, "quota read failed");
        }
    });
    Ok((StatusCode::CREATED, Json(account)))
}

/// Rename an added account or replace any account's pools.
#[utoipa::path(
    patch,
    path = "/v1/accounts/{id}",
    tag = "accounts",
    params(("id" = String, Path, description = "Account id")),
    request_body = UpdateAccount,
    responses(
        (status = 200, body = Account),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<UpdateAccount>,
) -> Result<Json<Account>, ApiError> {
    Ok(Json(state.accounts.update(&id, input).await?))
}

/// Remove an added account. Its directory is left as it is. Refused for a
/// default account, while a task runs under the account, and while a
/// dispatch profile would be left with no enabled account; `POST
/// /v1/accounts/{id}/remove` handles both.
#[utoipa::path(
    delete,
    path = "/v1/accounts/{id}",
    tag = "accounts",
    params(("id" = String, Path, description = "Account id")),
    responses(
        (status = 204, description = "The account is removed"),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "A default account, one in use, or one a profile needs (`needs_disposition`)")
    )
)]
pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let (_, plan, _) = gather(&state, &id, AccountChangeAction::Remove).await?;
    account_change::check(&plan, &BTreeMap::new())
        .map_err(|e| ApiError::new(StatusCode::CONFLICT, "needs_disposition", e))?;
    state.accounts.delete(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Read an account's quota waiting on the Mac's Keychain prompt for its
/// login's Keychain item (choose "Always Allow"), for a reading with
/// `keychain_grant`. Later reads use the grant. Answers with the account and
/// the new reading once the prompt is answered.
#[utoipa::path(
    post,
    path = "/v1/accounts/{id}/keychain-grant",
    tag = "accounts",
    params(("id" = String, Path, description = "Account id")),
    responses(
        (status = 200, body = Account),
        (status = 400, body = ErrorBody, description = "A key account"),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn keychain_grant(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Account>, ApiError> {
    Ok(Json(state.accounts.grant_keychain(&id).await?))
}

/// Keep an API key in the Keychain for a default account to sign in with,
/// for a harness that takes keys (`HarnessInfo.key_env`, e.g. Kiro CLI).
/// Its launches and quota reads then use the key, not quarkd's own
/// environment, so a quarkd launchd started signs in too. Replaces an
/// earlier key. The account's quota is read again right away.
#[utoipa::path(
    put,
    path = "/v1/accounts/{id}/key",
    tag = "accounts",
    params(("id" = String, Path, description = "A default account's id, e.g. `default-kiro`")),
    request_body = SetAccountKey,
    responses(
        (status = 200, body = Account),
        (status = 400, body = ErrorBody, description = "Not a default account, or its harness takes no key"),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn set_key(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<SetAccountKey>,
) -> Result<Json<Account>, ApiError> {
    let account = state.accounts.set_default_key(&id, &input.api_key).await?;
    let accounts = state.accounts.clone();
    tokio::spawn(async move {
        if let Err(e) = accounts.refresh_quota(Some(&id)).await {
            tracing::warn!(account = %id, error = %e, "quota read failed");
        }
    });
    Ok(Json(account))
}

/// Forget a default account's kept key: it signs in from quarkd's own
/// environment again.
#[utoipa::path(
    delete,
    path = "/v1/accounts/{id}/key",
    tag = "accounts",
    params(("id" = String, Path, description = "A default account's id")),
    responses(
        (status = 200, body = Account),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn clear_key(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Account>, ApiError> {
    Ok(Json(state.accounts.clear_default_key(&id).await?))
}

/// What disabling or removing an account touches, before anything changes.
///
/// Lists the tasks and coordinators running under the account (they keep
/// running until they finish), and every dispatch profile and coordinator
/// agent, in every Project, that can run under it. A profile left with no
/// enabled account is `stranded`, with what Quark proposes for it in
/// `options`, the proposal first. Confirm with `POST
/// /v1/accounts/{id}/disable` or `/remove`.
#[utoipa::path(
    post,
    path = "/v1/accounts/{id}/plan",
    tag = "accounts",
    params(("id" = String, Path, description = "Account id")),
    request_body = PlanAccountChange,
    responses(
        (status = 200, body = AccountChangePlan),
        (status = 404, body = ErrorBody)
    )
)]
pub async fn plan(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<PlanAccountChange>,
) -> Result<Json<AccountChangePlan>, ApiError> {
    Ok(Json(gather(&state, &id, input.action).await?.1))
}

/// Disable an account for Quark, in every Project.
///
/// From now on no task or coordinator starts under it; those running under
/// it carry on, and the account is `draining` until they finish. Every
/// profile the plan lists as `stranded` needs one of its options in
/// `dispositions`; those changes are committed to each Project's
/// `dispatch.yaml` (or set as its coordinator's agent for its next start)
/// first.
#[utoipa::path(
    post,
    path = "/v1/accounts/{id}/disable",
    tag = "accounts",
    params(("id" = String, Path, description = "Account id")),
    request_body = ApplyAccountChange,
    responses(
        (status = 200, body = AccountChangeOutcome),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "A stranded profile has no choice (`needs_disposition`), or a Project's rules could not be saved")
    )
)]
pub async fn disable(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<ApplyAccountChange>,
) -> Result<Json<AccountChangeOutcome>, ApiError> {
    let changed = settle(
        &state,
        &id,
        AccountChangeAction::Disable,
        &input.dispositions,
    )
    .await?;
    let account = state.accounts.disable(&id).await?;
    Ok(Json(AccountChangeOutcome {
        account: Some(account),
        removed: false,
        changed,
    }))
}

/// Remove an added account, once the work running under it finishes.
///
/// As `disable`, then the account is removed: now when nothing runs under
/// it, else when the last of that finishes (`removing` until then). Its
/// directory is left as it is; a key account's key leaves the Keychain.
#[utoipa::path(
    post,
    path = "/v1/accounts/{id}/remove",
    tag = "accounts",
    params(("id" = String, Path, description = "Account id")),
    request_body = ApplyAccountChange,
    responses(
        (status = 200, body = AccountChangeOutcome),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "A default account, a stranded profile with no choice (`needs_disposition`), or a Project's rules could not be saved")
    )
)]
pub async fn remove(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<ApplyAccountChange>,
) -> Result<Json<AccountChangeOutcome>, ApiError> {
    let changed = settle(
        &state,
        &id,
        AccountChangeAction::Remove,
        &input.dispositions,
    )
    .await?;
    let removed = state.accounts.remove_when_idle(&id).await?;
    let account = if removed {
        None
    } else {
        Some(state.accounts.get(&id).await?)
    };
    Ok(Json(AccountChangeOutcome {
        account,
        removed,
        changed,
    }))
}

/// Enable an account for Quark again; calls off a pending removal.
#[utoipa::path(
    post,
    path = "/v1/accounts/{id}/enable",
    tag = "accounts",
    params(("id" = String, Path, description = "Account id")),
    responses((status = 200, body = Account), (status = 404, body = ErrorBody))
)]
pub async fn enable(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Account>, ApiError> {
    let account = state.accounts.enable(&id).await?;
    let accounts = state.accounts.clone();
    tokio::spawn(async move {
        if let Err(e) = accounts.refresh_quota(Some(&id)).await {
            tracing::warn!(account = %id, error = %e, "quota read failed");
        }
    });
    Ok(Json(account))
}

/// The plan for `action` on account `id`, with the Projects it read.
async fn gather(
    state: &AppState,
    id: &str,
    action: AccountChangeAction,
) -> Result<(Account, AccountChangePlan, Vec<quark_systems::Project>), ApiError> {
    let accounts = state.accounts.list().await?;
    let target = accounts
        .iter()
        .find(|a| a.id == id)
        .cloned()
        .ok_or_else(ApiError::not_found)?;
    let projects = db(state, |s| s.list_projects()).await?;
    let user_config = state.layout.user_config();
    let mut profiles = Vec::with_capacity(projects.len());
    for p in &projects {
        let rules = match p.project_repo_path.clone() {
            Some(bare) => {
                let (pid, user_config) = (p.id.clone(), user_config.clone());
                let read = tokio::task::spawn_blocking(move || {
                    super::dispatch::read_rules(&pid, &PathBuf::from(bare), &user_config)
                })
                .await
                .map_err(|e| ApiError::internal(e.to_string()))?;
                match read {
                    Ok(r) => Some(quark_systems::DispatchRulesDraft {
                        default_select: r.default_select,
                        rules: r.rules,
                        default: r.default,
                    }),
                    Err(e) => {
                        tracing::warn!(project = %p.id, error = %e.message(), "dispatch rules unreadable; left out of the account plan");
                        None
                    }
                }
            }
            None => None,
        };
        profiles.push(ProjectProfiles {
            id: p.id.clone(),
            name: p.name.clone(),
            rules,
            coordinator: p.agent_config.clone(),
        });
    }
    let id2 = id.to_string();
    let running = db(state, move |s| s.account_work(&id2)).await?;
    let harnesses = state.harnesses.clone();
    let resolve = move |name: &str| harnesses.resolve(name).map(|h| h.id().to_string());
    let plan = account_change::plan(&target, action, &accounts, &profiles, running, &resolve);
    Ok((target, plan, projects))
}

/// Checks the person's choices against a fresh plan and makes them, Project
/// by Project. Returns what changed.
async fn settle(
    state: &AppState,
    id: &str,
    action: AccountChangeAction,
    choices: &BTreeMap<String, AccountDisposition>,
) -> Result<Vec<AccountChangeApplied>, ApiError> {
    let (target, plan, projects) = gather(state, id, action).await?;
    account_change::check(&plan, choices)
        .map_err(|e| ApiError::new(StatusCode::CONFLICT, "needs_disposition", e))?;
    let message = format!(
        "Move dispatch rules off the {} account `{}`",
        target.harness, target.label
    );
    let user_config = state.layout.user_config();
    let mut out = Vec::new();
    for p in projects {
        let mine = |r: &&quark_systems::AccountReference| r.project_id == p.id;
        let touched = |place: quark_systems::AccountPlace| {
            plan.references.iter().filter(mine).any(|r| {
                r.place == place
                    && choices
                        .get(&r.id)
                        .is_some_and(|c| !matches!(c, AccountDisposition::Keep))
            })
        };
        let rules = touched(quark_systems::AccountPlace::Rule)
            || touched(quark_systems::AccountPlace::Default);
        let coordinator = touched(quark_systems::AccountPlace::Coordinator);
        if !rules && !coordinator {
            continue;
        }
        let mut commit = None;
        if rules {
            let (Some(bare), Some(root)) = (p.project_repo_path.clone(), p.workspace_path.clone())
            else {
                continue;
            };
            let (bare, checkout) = (PathBuf::from(bare), PathBuf::from(root).join("project"));
            let (pid, choices, user_config, message) = (
                p.id.clone(),
                choices.clone(),
                user_config.clone(),
                message.clone(),
            );
            // Memory entries and rule edits are committed through the same checkout.
            let _one = super::memory::COMMITTING.lock().await;
            commit = tokio::task::spawn_blocking(move || {
                project_repo::commit_file(
                    &bare,
                    &checkout,
                    crew_dispatch::FILE,
                    |current| {
                        let failed =
                            |e: String| ApiError::new(StatusCode::CONFLICT, "dispatch_invalid", e);
                        let text = current.map(|(_, t)| t).unwrap_or_default();
                        let written = crew_dispatch::as_written(text).map_err(failed)?;
                        let mut draft = crew_dispatch::draft(&written);
                        account_change::apply_rules(&pid, &mut draft, &choices).map_err(failed)?;
                        let body = crew_dispatch::render(&draft, current.map(|(_, t)| t))
                            .map_err(failed)?;
                        let user = classifier::user_default_at(&user_config).map_err(failed)?;
                        crew_dispatch::compile(&body, user.as_ref()).map_err(failed)?;
                        Ok::<_, ApiError>(body)
                    },
                    &message,
                )
            })
            .await
            .map_err(|e| ApiError::internal(e.to_string()))??;
        }
        let mut agent_changed = false;
        if coordinator {
            if let Some(agent) = p
                .agent_config
                .as_ref()
                .and_then(|a| account_change::apply_agent(&p.id, a, choices))
            {
                let pid = p.id.clone();
                db(state, move |s| s.set_project_agent(&pid, Some(agent))).await?;
                agent_changed = true;
            }
        }
        out.push(AccountChangeApplied {
            project_id: p.id,
            project_name: p.name,
            commit,
            coordinator: agent_changed,
        });
    }
    Ok(out)
}

/// This module's endpoints, plus schemas the generator does not reach from
/// them (event payloads), merged into the served document by
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(
        list,
        get,
        create,
        update,
        delete,
        keychain_grant,
        set_key,
        clear_key,
        plan,
        disable,
        remove,
        enable
    ),
    components(schemas(
        quark_systems::QuotaState,
        quark_systems::AccountQuotaChanged,
        quark_systems::AccountFailover,
        quark_systems::FailoverOutcome,
    ))
)]
pub(super) struct Api;

/// This module's routes, merged into the `/v1` router.
pub(super) fn router() -> axum::Router<AppState> {
    use axum::routing::{get, post, put};
    axum::Router::new()
        .route("/v1/accounts", get(list).post(create))
        .route(
            "/v1/accounts/{id}",
            get(self::get).patch(update).delete(self::delete),
        )
        .route("/v1/accounts/{id}/keychain-grant", post(keychain_grant))
        .route("/v1/accounts/{id}/key", put(set_key).delete(clear_key))
        .route("/v1/accounts/{id}/plan", post(plan))
        .route("/v1/accounts/{id}/disable", post(disable))
        .route("/v1/accounts/{id}/remove", post(remove))
        .route("/v1/accounts/{id}/enable", post(enable))
}
