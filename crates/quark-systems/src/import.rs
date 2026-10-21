//! Importing an existing firstmate home into Quark.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Seed Quark from a firstmate home. The home is only read.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct ImportFirstmate {
    /// The firstmate home: the directory holding its `data/`, `state/`,
    /// `config/` and `projects/`.
    pub home: String,
    /// Report what would be imported and change nothing.
    #[serde(default)]
    pub dry_run: bool,
    /// Only these firstmate projects, by name; all of them when empty.
    #[serde(default)]
    pub projects: Vec<String>,
    /// Also import the Done entries of `data/done-archive.md`, which is
    /// often thousands of lines; only the Done section of the backlog is
    /// imported by default.
    #[serde(default)]
    pub include_archive: bool,
    /// Which project a backlog item belongs to when nothing in it says: an
    /// id prefix and the firstmate project it routes to, such as
    /// `"magenta-": "magenta-mono"`. The longest matching prefix wins.
    #[serde(default)]
    pub routes: std::collections::BTreeMap<String, String>,
    /// Seconds to wait for each Project to be provisioned and its Beads
    /// database to be set up before its tasks are imported; default 120.
    /// A Project not ready by then is reported as pending, and importing
    /// again finishes it.
    #[serde(default)]
    pub wait_secs: Option<u64>,
}

/// What an import did, or with `dry_run` would do.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FirstmateImportReport {
    pub home: String,
    pub dry_run: bool,
    /// Every category firstmate stores locally and where it went.
    pub inventory: Vec<ImportInventoryItem>,
    pub projects: Vec<ImportedProject>,
    /// Backlog items no Project could be chosen for. They are not imported.
    pub unrouted: Vec<String>,
    /// Things to know or do next, such as a stale Bob anchor.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportOutcome {
    /// Written to Quark, or with `dry_run` it would be.
    Imported,
    /// Already in Quark from an earlier import; left as it is.
    Present,
    /// Waits on something (a Project still provisioning); import again.
    Pending,
    /// Not carried over; `detail` says why.
    NotImported,
}

/// One category of firstmate data and its Quark equivalent.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ImportInventoryItem {
    /// `projects`, `dispatch`, `quota`, `tasks`, `holds`, `status`,
    /// `memory`, `reports`, `runtime`, `secrets`...
    pub category: String,
    /// Where it is in the firstmate home.
    pub source: String,
    /// What Quark keeps it as; empty when not imported.
    pub target: String,
    pub outcome: ImportOutcome,
    /// How many records the category holds.
    pub count: usize,
    pub detail: String,
}

/// One firstmate project and what became of it in Quark.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ImportedProject {
    /// The firstmate project's name.
    pub name: String,
    /// The Quark Project; absent on a dry run for a Project not yet created.
    pub project_id: Option<String>,
    /// `created` or `existing`.
    pub project: String,
    pub repo_url: Option<String>,
    pub outcome: ImportOutcome,
    /// Why, when `pending` or `not_imported`.
    pub detail: Option<String>,
    /// Whether `dispatch.yaml` was written.
    pub dispatch: ImportOutcome,
    /// Beads created, by what they came from.
    pub tasks: ImportCount,
    pub holds: ImportCount,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct ImportCount {
    pub imported: usize,
    pub present: usize,
}
