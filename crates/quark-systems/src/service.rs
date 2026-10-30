//! Daemon health and the error body every endpoint returns.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Version of the `/v1` API contract. Clients compare it with the one they were built
/// against: the desktop app, QuarkBar and a daemon updated on its own (in WSL on Windows)
/// can differ in release version and still work while this matches. Bump it on a change a
/// client built for the old contract can't handle.
pub const API_CONTRACT: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Health {
    pub status: String,
    pub version: String,
    /// The `/v1` contract version, [`API_CONTRACT`].
    pub api_version: u32,
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
