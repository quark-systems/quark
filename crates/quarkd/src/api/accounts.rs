use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use quark_systems::{Account, CreateAccount, ErrorBody, UpdateAccount};
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
