//! Shadow comparison against the bash engine.
//!
//! Until slice 7 switches on, firstmate keeps its second mates in
//! `data/secondmates.md`. [`parse_registry`] reads that file and [`compare`]
//! sets it beside the native [`crate::Registry`], one
//! [`quark_core::slice::Divergence`] per sub-coordinator the two disagree
//! on, so the shadow engine can record them before the slice moves to
//! native.

use std::collections::BTreeMap;
use std::path::PathBuf;

use quark_core::slice::{Divergence, Slice};
use quark_runtime::RuntimeSpec;
use serde::Serialize;

use crate::registry::SubCoordinator;

/// The operation name divergences are recorded under.
pub const OPERATION: &str = "sub_coordinators";

/// One second mate as firstmate records it, in the fields both engines
/// keep.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub id: String,
    /// The SSH host for a remote one.
    pub host: Option<String>,
    pub home: PathBuf,
    pub scope: String,
    pub projects: Vec<String>,
}

/// The entries in a firstmate `data/secondmates.md`. Lines that are not
/// entries are skipped.
pub fn parse_registry(text: &str) -> Vec<Entry> {
    text.lines().filter_map(parse_line).collect()
}

/// One `- <id> - <summary> (home: ...; scope: ...; projects: ...; added
/// <date>)` line, or the remote form with `host:` and `root:` first.
fn parse_line(line: &str) -> Option<Entry> {
    let rest = line.trim_end().strip_prefix("- ")?;
    let (id, rest) = rest.split_once(" - ")?;
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
    {
        return None;
    }
    let start = [rest.rfind(" (home:"), rest.rfind(" (host:")]
        .into_iter()
        .flatten()
        .max()?;
    let suffix = rest[start + 2..].strip_suffix(')')?;
    let (fields, _added) = suffix.rsplit_once("; added ")?;
    let (head, projects) = fields.rsplit_once("; projects:")?;
    let (head, scope) = head.split_once("; scope:")?;
    let mut parts: BTreeMap<&str, &str> = BTreeMap::new();
    for f in head.split(';') {
        let (k, v) = f.split_once(':')?;
        parts.insert(k.trim(), v.trim());
    }
    let projects = projects
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(str::trim)
        .filter(|p| !p.is_empty() && *p != "none" && *p != "-")
        .map(str::to_string)
        .collect();
    Some(Entry {
        id: id.to_string(),
        host: parts.get("host").map(|h| h.to_string()),
        home: PathBuf::from(parts.get("home")?),
        scope: scope.trim().to_string(),
        projects,
    })
}

/// The native view of `s` in the same fields.
pub fn entry(s: &SubCoordinator) -> Entry {
    let r = &s.registration;
    let mut projects: Vec<String> = r.projects.iter().map(|p| p.name.clone()).collect();
    projects.sort();
    Entry {
        id: r.id.to_string(),
        host: match &r.placement.runtime {
            RuntimeSpec::Local => None,
            RuntimeSpec::Ssh { target } => Some(target.destination.clone()),
        },
        home: r.placement.home.clone(),
        scope: r.charter.scope.trim().to_string(),
        projects,
    }
}

/// Where the live native sub-coordinators and bash's second mates differ:
/// one divergence per id missing on either side or recorded differently.
pub fn compare(native: &[SubCoordinator], bash: &[Entry]) -> Vec<Divergence> {
    let native: BTreeMap<String, Entry> = native
        .iter()
        .filter(|s| s.is_live())
        .map(|s| (s.id().to_string(), entry(s)))
        .collect();
    let bash: BTreeMap<String, Entry> = bash
        .iter()
        .map(|e| {
            let mut e = e.clone();
            e.projects.sort();
            (e.id.clone(), e)
        })
        .collect();
    let ids: std::collections::BTreeSet<&String> = native.keys().chain(bash.keys()).collect();
    ids.into_iter()
        .filter(|id| native.get(*id) != bash.get(*id))
        .map(|id| Divergence {
            slice: Slice::SubCoordinators,
            operation: OPERATION.into(),
            bash: serde_json::to_value(bash.get(id)).unwrap_or_default(),
            native: serde_json::to_value(native.get(id)).unwrap_or_default(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_local_and_remote_lines() {
        let text = "# Second mates\n\n\
            - design - design domain mentions home: /x (home: /srv/fm/design; scope: design work; mentions home: /y; projects: alpha, beta; added 2026-06-22)\n\
            - ios - iOS delivery (host: remote-mac; root: /r/fm; home: /r/homes/ios; scope: iOS work; projects: alpha; added 2026-08-02)\n\
            not an entry\n";
        let e = parse_registry(text);
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].id, "design");
        assert_eq!(e[0].host, None);
        assert_eq!(e[0].home, PathBuf::from("/srv/fm/design"));
        assert_eq!(e[0].scope, "design work; mentions home: /y");
        assert_eq!(e[0].projects, ["alpha", "beta"]);
        assert_eq!(e[1].host.as_deref(), Some("remote-mac"));
        assert_eq!(e[1].home, PathBuf::from("/r/homes/ios"));
        assert_eq!(e[1].projects, ["alpha"]);
    }
}
