//! Verification gate manifests (ADR-15).
//!
//! The gate runner writes `state/<id>.gates.json` (schema `quark.gates.v1`)
//! atomically and keeps the artifacts it lists under `state/<id>.gates/`.
//! Artifact paths are relative to that directory; anything that could reach
//! outside it is refused when the manifest is read.

use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

use crate::workspace::read_optional;
use crate::{Error, Result};

pub const SCHEMA: &str = "quark.gates.v1";
const WHAT: &str = "gate manifest";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Pending,
    Running,
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Checks,
    Journeys,
    Holdout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Trace,
    Screenshot,
    Video,
    Log,
    Report,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateManifest {
    pub schema: String,
    pub head_sha: Option<String>,
    pub state: State,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    #[serde(default)]
    pub gates: Vec<Gate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gate {
    pub kind: Kind,
    pub state: State,
    pub summary: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    #[serde(default)]
    pub cases: Vec<Case>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Case {
    pub name: String,
    pub state: State,
    pub duration_ms: Option<u64>,
    pub message: Option<String>,
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub kind: ArtifactKind,
    /// Relative to `state/<id>.gates/`.
    pub path: String,
    pub content_type: Option<String>,
}

pub fn read(path: &Path) -> Result<Option<GateManifest>> {
    let Some(bytes) = read_optional(path)? else {
        return Ok(None);
    };
    parse(&bytes).map(Some)
}

/// Parses a manifest, refusing another schema and unsafe artifact paths.
/// Holdout cases lose any message and artifacts, so holdout detail can never
/// reach a client even if the runner wrote it.
pub fn parse(bytes: &[u8]) -> Result<GateManifest> {
    let mut m: GateManifest =
        serde_json::from_slice(bytes).map_err(|source| Error::Json { what: WHAT, source })?;
    if m.schema != SCHEMA {
        return Err(Error::Schema {
            what: WHAT,
            found: Some(m.schema),
            expected: SCHEMA,
        });
    }
    for gate in &mut m.gates {
        for case in &mut gate.cases {
            if gate.kind == Kind::Holdout {
                case.message = None;
                case.artifacts.clear();
            }
            for a in &case.artifacts {
                if !safe_relative(&a.path) {
                    return Err(Error::Malformed {
                        what: WHAT,
                        detail: format!("artifact path {:?} leaves the gates directory", a.path),
                    });
                }
            }
        }
    }
    Ok(m)
}

/// A non-empty relative path of normal components only.
pub fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\0')
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"{
        "schema": "quark.gates.v1", "head_sha": "abc", "state": "failed",
        "started_at": "2026-10-02T16:00:00Z", "completed_at": "2026-10-02T16:05:00Z",
        "gates": [
          {"kind": "checks", "state": "passed", "cases": []},
          {"kind": "journeys", "state": "failed", "summary": "1 of 2 journeys failed",
           "cases": [
             {"name": "create a project", "state": "passed", "duration_ms": 4200, "artifacts": []},
             {"name": "merge a PR", "state": "failed", "message": "timed out",
              "artifacts": [
                {"kind": "trace", "path": "journeys/merge/trace.zip"},
                {"kind": "screenshot", "path": "journeys/merge/failed.png", "content_type": "image/png"}
              ]}
           ]},
          {"kind": "holdout", "state": "failed",
           "cases": [{"name": "auth", "state": "failed", "message": "secret detail",
                      "artifacts": [{"kind": "log", "path": "holdout.log"}]}]}
        ]
    }"#;

    #[test]
    fn parses_and_hides_holdout_detail() {
        let m = parse(MANIFEST.as_bytes()).unwrap();
        assert_eq!(m.state, State::Failed);
        assert_eq!(m.gates[1].cases[1].artifacts.len(), 2);
        let holdout = &m.gates[2].cases[0];
        assert_eq!(holdout.name, "auth");
        assert_eq!(holdout.message, None);
        assert!(holdout.artifacts.is_empty());
    }

    #[test]
    fn refuses_other_schemas_and_escaping_paths() {
        assert!(parse(
            MANIFEST
                .replace("quark.gates.v1", "quark.gates.v2")
                .as_bytes()
        )
        .is_err());
        for bad in ["../x", "/etc/passwd", "a/../../b", ""] {
            let m = MANIFEST.replace("journeys/merge/trace.zip", bad);
            assert!(parse(m.as_bytes()).is_err(), "{bad}");
        }
        assert!(safe_relative("a/b.png"));
    }

    #[test]
    fn absent_manifest_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(&dir.path().join("x.gates.json")).unwrap(), None);
    }
}
