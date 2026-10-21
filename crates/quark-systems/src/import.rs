//! Seeding a Project from another tool's local data (firstmate).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{CreateProject, Project};

/// Import a firstmate home into a new Project.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ImportFirstmate {
    /// The firstmate home (its `FM_HOME`, usually the firstmate clone):
    /// the directory holding `data/`, `config/`, `state/` and `projects/`.
    pub path: String,
    /// The new Project's name; derived from the firstmate projects when
    /// absent.
    #[serde(default)]
    pub name: Option<String>,
    /// The coordinator's harness id, when not the firstmate crew harness.
    #[serde(default)]
    pub harness: Option<String>,
    /// Only read the home and answer the plan; nothing is created.
    #[serde(default)]
    pub dry_run: bool,
}

/// Where one kind of firstmate data goes in Quark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportOutcome {
    /// Carried into the Project.
    Imported,
    /// Read but not carried over, with the reason in `note`.
    Skipped,
}

/// One kind of firstmate data and what the import does with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ImportItem {
    /// What it is, in plain words, e.g. `Backlog`.
    pub what: String,
    /// Where firstmate keeps it, relative to its home.
    #[serde(default)]
    pub files: Vec<String>,
    /// How many records or files were found.
    pub count: usize,
    pub outcome: ImportOutcome,
    /// Where it goes in Quark, or why it does not.
    pub note: String,
}

/// What a firstmate import creates.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FirstmateImportPlan {
    /// The firstmate home read, as given.
    pub path: String,
    /// The Project the import creates.
    pub project: CreateProject,
    /// Whether the Project gets standing approval to merge (firstmate's
    /// `+yolo` on every project).
    pub standing_approval: bool,
    /// Beads the backlog becomes.
    pub issues: usize,
    /// Every kind of firstmate data found, imported or not.
    pub items: Vec<ImportItem>,
    /// Problems that do not stop the import.
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// The answer to an import: the plan, and the Project when one was created.
/// The Project is created at once and provisions in the background; the
/// rest of the import (dispatch rules, memory, library files, beads) lands
/// once it is ready, and `library/firstmate/IMPORT.md` in its Project repo
/// records what was imported.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FirstmateImport {
    pub plan: FirstmateImportPlan,
    #[serde(default)]
    pub project: Option<Project>,
}
