//! How ready each slice of the native port is to switch on, read from the
//! shadows' `shadow.divergence` events in the native event log.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Every slice's shadow readiness over the last `days`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ShadowReadiness {
    /// Days of history the counts and examples cover, ending now.
    pub days: u32,
    /// When the daemon last started, and which shadows it ran.
    pub latest_start: Option<ShadowStart>,
    /// Slices 1 to 9, in switch-on order.
    pub slices: Vec<SliceReadiness>,
    /// Set when the event log could not be read; `slices` is then empty.
    pub error: Option<String>,
}

/// One daemon start's shadows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ShadowStart {
    /// RFC 3339.
    pub at: String,
    /// Slice names whose shadow ran, such as `dispatch`.
    pub slices: Vec<String>,
    /// Slice names that ran native, such as `event_log`.
    #[serde(default)]
    pub native: Vec<String>,
}

/// Where a slice stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShadowStatus {
    /// The slice has no shadow that compares native with bash, so
    /// divergences can't judge it.
    NoCheck,
    /// Its shadow did not run on the daemon's latest start.
    Off,
    /// On with no divergences, but for less than the whole window.
    Watching,
    /// At least one divergence in the window.
    Diverging,
    /// On for the whole window with no divergences.
    Agreeing,
    /// Switched to native on the daemon's latest start.
    Native,
}

/// One slice's shadow and what it recorded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SliceReadiness {
    /// 1 to 9.
    pub number: u8,
    /// Such as `verification`.
    pub slice: String,
    pub status: ShadowStatus,
    /// The setting that turns this slice's shadow on, when it has one.
    pub switch: Option<String>,
    /// What the shadow compares, or why there is none.
    pub note: String,
    /// Start of the unbroken run of daemon starts with this shadow on
    /// (RFC 3339).
    pub on_since: Option<String>,
    /// While `watching`: when it reads `agreeing` if it keeps running and
    /// nothing diverges, `on_since` plus the window (RFC 3339).
    #[serde(default)]
    pub agrees_at: Option<String>,
    /// Divergences in the window.
    pub divergences: u64,
    /// The window's divergences per operation, most first.
    pub operations: Vec<OperationCount>,
    /// The newest divergence ever recorded for this slice (RFC 3339).
    pub last_divergence_at: Option<String>,
    /// The window's newest divergences, newest first.
    pub examples: Vec<DivergenceExample>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OperationCount {
    pub operation: String,
    pub count: u64,
}

/// One recorded disagreement, with each side cut to a short preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DivergenceExample {
    /// Its position in the event log.
    pub seq: u64,
    /// RFC 3339.
    pub at: String,
    pub project_id: String,
    pub task: Option<String>,
    pub operation: String,
    /// What bash answered (what callers got), as JSON.
    pub bash: String,
    /// What native answered, as JSON.
    pub native: String,
}
