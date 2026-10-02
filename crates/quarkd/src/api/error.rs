use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use quark_systems::{ErrorBody, ErrorDetail};

use crate::accounts::AccountError;
use crate::engine::EngineError;
use crate::store::StoreError;

/// Longest engine failure detail returned to a client, in bytes.
const ENGINE_DETAIL_MAX: usize = 2048;

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "resource not found")
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", message)
    }

    /// The machine-readable error code.
    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound => ApiError::not_found(),
            StoreError::Invalid(m) => ApiError::invalid(m),
            StoreError::Conflict(m) => ApiError::new(StatusCode::CONFLICT, "conflict", m),
            other => {
                tracing::error!(error = %other, "store error");
                ApiError::internal("internal store error")
            }
        }
    }
}

impl From<AccountError> for ApiError {
    fn from(e: AccountError) -> Self {
        match e {
            AccountError::Invalid(m) => ApiError::invalid(m),
            AccountError::NotFound => ApiError::not_found(),
            AccountError::Conflict(m) => ApiError::new(StatusCode::CONFLICT, "conflict", m),
            AccountError::Store(e) => e.into(),
        }
    }
}

impl From<EngineError> for ApiError {
    fn from(e: EngineError) -> Self {
        match e {
            EngineError::Invalid(m) => ApiError::invalid(m),
            EngineError::TaskNotFound(_) => ApiError::not_found(),
            EngineError::WorkspaceNotFound(_) => ApiError::new(
                StatusCode::CONFLICT,
                "workspace_missing",
                "the Project's workspace is not on this machine",
            ),
            other => {
                let message = other.to_string();
                tracing::warn!(error = %message, "engine call failed");
                ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "engine_failed",
                    tail(&message, ENGINE_DETAIL_MAX),
                )
            }
        }
    }
}

/// The last `max` bytes of `s`, on a character boundary.
fn tail(s: &str, max: usize) -> String {
    let s = s.trim_end();
    let mut start = s.len().saturating_sub(max);
    while !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            error: ErrorDetail {
                code: self.code.to_string(),
                message: self.message,
            },
        };
        (self.status, Json(body)).into_response()
    }
}
