//! Collections: named sets of Projects, for the left list alone.
//!
//! A collection groups Projects so the left list can show them under a
//! heading. A Project may be in several collections, or none, and a
//! collection decides nothing about how a Project runs: it is what the
//! person sees. Turning collections off in the app shows every Project
//! flat, whatever the collections say.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A named set of Projects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Collection {
    pub id: String,
    pub name: String,
    /// Where it sits among the collections, ascending. Ties break by name.
    pub position: i64,
    /// Whether it is folded shut in the left list, showing its heading alone.
    #[serde(default)]
    pub collapsed: bool,
    /// The Projects in it, in the order the left list shows them. Ids of
    /// Projects that no longer exist are left out.
    pub projects: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// A new collection. It starts empty, after the collections that exist.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct CreateCollection {
    pub name: String,
}

/// Partial update; absent fields are left unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UpdateCollection {
    pub name: Option<String>,
    /// Where it sits among the collections.
    pub position: Option<i64>,
    /// Fold it shut in the left list, or open it again.
    pub collapsed: Option<bool>,
    /// Replaces what is in the collection, in the order given. An id that
    /// names no Project is refused; a repeated one is kept once.
    pub projects: Option<Vec<String>>,
}
