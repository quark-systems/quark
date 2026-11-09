use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use quark_systems::{Account, CreateAccount, ErrorBody, SetAccountKey, UpdateAccount};
use serde::Deserialize;
use utoipa::IntoParams;

use super::{ApiError, AppState};

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AccountQuery {
    /// Only this harness's accounts, e.g. `claude-code`.
    pub harness: Option<String>,
    /// Read every listed account's quota now (one quota-axi call each)
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
/// default account and while a task runs under the account.
#[utoipa::path(
    delete,
    path = "/v1/accounts/{id}",
    tag = "accounts",
    params(("id" = String, Path, description = "Account id")),
    responses(
        (status = 204, description = "The account is removed"),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "A default account, or one in use")
    )
)]
pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.accounts.delete(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Read an account's quota letting quota-axi prompt on the Mac for its
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

/// This module's endpoints, plus schemas the generator does not reach from
/// them (event payloads), merged into the served document by
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(list, get, create, update, delete, keychain_grant, set_key, clear_key),
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
}
