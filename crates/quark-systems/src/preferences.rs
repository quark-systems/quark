//! App-wide preferences: what the person sets once and expects on every
//! client this daemon serves.
//!
//! These are not Project settings and change nothing about how work runs:
//! they say how the app shows what is there. They live here rather than in
//! a client's own storage so that a second client, or a browser on another
//! machine once quarkd is reached over a network, opens the same way.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Every app-wide preference, with its default when the person has set none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Preferences {
    /// Whether the left list groups projects under their collections.
    /// Off shows every project flat, whatever the collections say.
    pub group_by_collection: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            group_by_collection: true,
        }
    }
}

/// Partial update; absent fields are left unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UpdatePreferences {
    pub group_by_collection: Option<bool>,
}
