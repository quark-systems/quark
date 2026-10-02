//! `state/home-summary.json`: the structured ledger a home publishes about
//! itself (schema `fm-secondmate-home-summary.v1`). The parent home and the
//! daemon read it to show a Project workspace without walking its files.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::workspace::read_optional;
use crate::{Error, Result};

pub const SCHEMA: &str = "fm-secondmate-home-summary.v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeSummary {
    pub schema: String,
    pub generated: String,
    #[serde(default)]
    pub generated_epoch: Option<i64>,
    #[serde(default)]
    pub hold_classifier_schema: Option<String>,
    /// Whether the home's backlog and metadata agree. A false value still
    /// carries trustworthy structured surfaces; `reason` says what is off.
    #[serde(default)]
    pub valid: bool,
    #[serde(default)]
    pub reason: Option<String>,
    /// Home-level state word, e.g. `idle`, `working`, `captain_decision`.
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub counts: Option<SummaryCounts>,
    #[serde(default)]
    pub active_children: Vec<ActiveChild>,
    #[serde(default)]
    pub decisions_open: Vec<SummaryDecision>,
    /// Remaining sections, untyped until the daemon projects them.
    #[serde(default)]
    pub holds: Vec<Value>,
    #[serde(default)]
    pub queued: Vec<Value>,
    #[serde(default)]
    pub landed: Vec<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SummaryCounts {
    #[serde(default)]
    pub active_children: u64,
    #[serde(default)]
    pub decisions_open: u64,
    #[serde(default)]
    pub holds: u64,
    #[serde(default)]
    pub queued: u64,
    #[serde(default)]
    pub landed: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveChild {
    pub id: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub doing: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryDecision {
    pub id: String,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub verb: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    /// `status` for a worker's open decision, `backlog` for a captain hold.
    #[serde(default)]
    pub source: Option<String>,
}

/// Read the ledger; `None` when this home has never published one.
pub fn read(path: &Path) -> Result<Option<HomeSummary>> {
    let Some(bytes) = read_optional(path)? else {
        return Ok(None);
    };
    parse(&bytes).map(Some)
}

pub fn parse(bytes: &[u8]) -> Result<HomeSummary> {
    let value: Value = serde_json::from_slice(bytes).map_err(|source| Error::Json {
        what: "home summary",
        source,
    })?;
    let found = value.get("schema").and_then(Value::as_str);
    if found != Some(SCHEMA) {
        return Err(Error::Schema {
            what: "home summary",
            found: found.map(str::to_string),
            expected: SCHEMA,
        });
    }
    serde_json::from_value(value).map_err(|source| Error::Json {
        what: "home summary",
        source,
    })
}
