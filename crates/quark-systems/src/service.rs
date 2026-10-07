//! Daemon health and the error body every endpoint returns.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Health {
    pub status: String,
    pub version: String,
    /// Name of the engine adapter in use.
    pub engine: String,
    /// Highest event `seq` in the store; 0 when empty.
    pub last_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ErrorDetail {
    /// Stable machine-readable code, e.g. `not_found`.
    pub code: String,
    pub message: String,
}
