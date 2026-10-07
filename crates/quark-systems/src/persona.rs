//! Persona packs: the role names, form of address and app labels a Project
//! reads with. Packs are data; every other type and event keeps the neutral
//! names.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// One persona pack, as the app needs it. Prompt fragments stay in the
/// daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Persona {
    /// Stable id such as `nautical` or `kitchen-brigade`.
    pub id: String,
    pub name: String,
    /// Shipped with Quark rather than added under the Quark home.
    pub builtin: bool,
    /// How agents address the user, such as `captain`.
    pub address: Option<String>,
    /// Voice and tone guidance added to agent prompts.
    pub voice: String,
    /// Flavor words agents may use sparingly.
    pub vocabulary: Vec<String>,
    /// Label per neutral role (`user`, `coordinator`, `worker`,
    /// `sub_coordinator`, `investigation`, `decision`). A role left out
    /// keeps its neutral name.
    pub roles: BTreeMap<String, String>,
    /// App label per neutral label id (`tasks`, `decisions`, `memory`, ...).
    /// An id left out keeps the app's neutral text.
    pub ui_labels: BTreeMap<String, String>,
}

/// Every installed pack and the global default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PersonaList {
    /// Id of the pack every Project uses unless it overrides it.
    pub default: String,
    pub packs: Vec<Persona>,
    /// Packs under the Quark home that could not be loaded, and why.
    pub errors: Vec<String>,
}

/// The pack a Project reads with, and where that choice comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProjectPersona {
    pub project_id: String,
    /// The pack in effect.
    pub persona: Persona,
    /// The Project's own choice; absent when it uses the default.
    pub project_override: Option<String>,
    /// The global default's id.
    pub default: String,
    /// Why the pack in effect is not the one chosen, when a chosen pack is
    /// no longer installed.
    pub fallback: Option<String>,
}

/// Choose a Project's pack. `null` clears its override so it follows the
/// global default again.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SetProjectPersona {
    pub persona: Option<String>,
}

/// Choose the global default pack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SetDefaultPersona {
    pub persona: String,
}
