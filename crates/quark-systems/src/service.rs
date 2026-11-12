//! Daemon health and the error body every endpoint returns.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Version of the `/v1` API contract. Clients compare it with the one they were built
/// against: the desktop app, QuarkBar and a daemon updated on its own (in WSL on Windows)
/// can differ in release version and still work while this matches. Bump it on a change a
/// client built for the old contract can't handle.
pub const API_CONTRACT: u32 = 1;

/// The oldest `/v1` contract this daemon still serves. A client built for contract `c`
/// works with a daemon when `API_CONTRACT_MIN <= c <= API_CONTRACT`. After a breaking
/// change bumps [`API_CONTRACT`], keep serving the old shape for one minor release, so an
/// app or QuarkBar that hasn't restarted since an update keeps working, then raise this.
pub const API_CONTRACT_MIN: u32 = 1;

/// Optional capabilities this daemon has, which clients check before showing what needs
/// them, instead of reading a release version. Add a name when a client must tell a
/// daemon with a feature from one without it; never remove one while it's served.
pub const FEATURES: &[&str] = &["compat", "update"];

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Health {
    pub status: String,
    pub version: String,
    /// The `/v1` contract version, [`API_CONTRACT`].
    pub api_version: u32,
    /// The oldest contract still served, [`API_CONTRACT_MIN`].
    #[serde(default = "contract_min_default")]
    pub api_min: u32,
    /// Optional capabilities, [`FEATURES`].
    #[serde(default)]
    pub features: Vec<String>,
    /// Name of the engine adapter in use.
    pub engine: String,
    /// Highest event `seq` in the store; 0 when empty.
    pub last_seq: i64,
}

/// Daemons from before `api_min` served only contract 1.
fn contract_min_default() -> u32 {
    1
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
