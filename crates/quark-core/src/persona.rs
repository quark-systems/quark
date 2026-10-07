//! Persona packs: voice and role names, never logic.
//!
//! The engine, API, event log and code use the neutral [`Role`] names only.
//! A pack maps them to flavored labels for prompts, the app and
//! notifications. Switching a Project's pack changes how it reads, not what
//! it does.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{ProjectId, Result};

/// Neutral roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Coordinator,
    Worker,
    SubCoordinator,
    Investigation,
    Decision,
}

impl Role {
    pub const ALL: [Role; 6] = [
        Role::User,
        Role::Coordinator,
        Role::Worker,
        Role::SubCoordinator,
        Role::Investigation,
        Role::Decision,
    ];

    /// The neutral name, used when a pack has no label.
    pub fn neutral(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Coordinator => "coordinator",
            Role::Worker => "worker",
            Role::SubCoordinator => "sub-coordinator",
            Role::Investigation => "investigation",
            Role::Decision => "decision",
        }
    }
}

/// A pack, as loaded from TOML plus prompt fragment files.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaPack {
    /// Stable id such as `nautical` or `kitchen-brigade`.
    pub id: String,
    pub name: String,
    /// Display name per role, such as `coordinator = "first mate"`.
    #[serde(default)]
    pub roles: BTreeMap<Role, String>,
    /// How agents address the user, such as `captain`.
    #[serde(default)]
    pub address: Option<String>,
    /// Voice and tone guidance added to coordinator and worker prompts.
    #[serde(default)]
    pub voice: String,
    /// Optional flavor words agents may use sparingly.
    #[serde(default)]
    pub vocabulary: Vec<String>,
    /// App label overrides keyed by neutral label id.
    #[serde(default)]
    pub ui_labels: BTreeMap<String, String>,
    /// Prompt fragments keyed by the neutral role they apply to.
    #[serde(default)]
    pub prompts: BTreeMap<Role, String>,
}

impl PersonaPack {
    /// The label for a role, falling back to the neutral name.
    pub fn label(&self, role: Role) -> &str {
        self.roles
            .get(&role)
            .map(String::as_str)
            .unwrap_or(role.neutral())
    }

    /// An app label, falling back to `default`.
    pub fn ui<'a>(&'a self, id: &str, default: &'a str) -> &'a str {
        self.ui_labels
            .get(id)
            .map(String::as_str)
            .unwrap_or(default)
    }
}

/// Resolves the pack a Project uses: its own override, else the global one.
#[async_trait]
pub trait PersonaSource: Send + Sync {
    async fn packs(&self) -> Result<Vec<PersonaPack>>;

    async fn for_project(&self, project: &ProjectId) -> Result<PersonaPack>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::StaticPersonas;

    fn nautical() -> PersonaPack {
        PersonaPack {
            id: "nautical".into(),
            name: "Nautical".into(),
            roles: [
                (Role::Coordinator, "first mate".into()),
                (Role::User, "captain".into()),
            ]
            .into_iter()
            .collect(),
            address: Some("captain".into()),
            ..Default::default()
        }
    }

    #[test]
    fn labels_fall_back_to_neutral() {
        let p = nautical();
        assert_eq!(p.label(Role::Coordinator), "first mate");
        assert_eq!(p.label(Role::Worker), "worker");
        assert_eq!(p.ui("nav.tasks", "Tasks"), "Tasks");
    }

    #[tokio::test]
    async fn project_override_wins() {
        let kitchen = PersonaPack {
            id: "kitchen-brigade".into(),
            ..Default::default()
        };
        let src = StaticPersonas {
            packs: vec![nautical(), kitchen],
            default: "nautical".into(),
            overrides: [(ProjectId::from("p2"), "kitchen-brigade".into())]
                .into_iter()
                .collect(),
        };
        assert_eq!(src.for_project(&"p1".into()).await.unwrap().id, "nautical");
        assert_eq!(
            src.for_project(&"p2".into()).await.unwrap().id,
            "kitchen-brigade"
        );
    }
}
