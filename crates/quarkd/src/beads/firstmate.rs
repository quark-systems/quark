//! Beads for a firstmate import. Each bead carries the label
//! `firstmate:<id>` naming the backlog item it came from, which is how an
//! import run again finds what an earlier one made and leaves it alone.
//!
//! Held items become `decision` beads, but without the `quark:` external ref
//! and human gate that [`super::decisions`] mirrors Quark decisions with:
//! these decisions were never asked in Quark, so closing the bead is the only
//! way to answer one.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use quark_systems::DraftIssue;

use super::{Beads, BeadsError};

/// Label on every imported bead.
pub const LABEL: &str = "firstmate";

/// The label that names the backlog item `id`.
pub fn id_label(id: &str) -> String {
    format!("{LABEL}:{id}")
}

/// Longest comment written, in bytes.
const MAX_COMMENT: usize = 60 * 1024;

/// Beads closed per `bd close`.
const CLOSE_BATCH: usize = 25;

/// One backlog item as a bead.
#[derive(Debug, Clone)]
pub struct ImportBead {
    /// The backlog item's id.
    pub id: String,
    pub title: String,
    pub description: String,
    pub issue_type: String,
    pub priority: u8,
    pub labels: Vec<String>,
    /// Backlog ids this waits for.
    pub blocked_by: Vec<String>,
    /// Closes the bead with this reason.
    pub close_reason: Option<String>,
    /// Comments to add, each its own text.
    pub comments: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Imported {
    pub created: usize,
    pub present: usize,
}

impl Beads {
    /// Creates a bead for each of `beads` that an earlier import did not.
    pub async fn import_firstmate(
        &self,
        home: &Path,
        project_id: &str,
        beads: &[ImportBead],
    ) -> Result<Imported, BeadsError> {
        let dir = Self::ready_dir(home, project_id)?;
        let existing: HashMap<String, String> = self
            .imported_firstmate(home, project_id)
            .await?
            .into_iter()
            .collect();
        let mut known = existing.clone();
        let mut out = Imported::default();
        let fresh: Vec<&ImportBead> = beads
            .iter()
            .filter(|b| {
                let present = existing.contains_key(&b.id);
                if present {
                    out.present += 1;
                }
                !present
            })
            .collect();
        let mut to_close: Vec<(String, String)> = Vec::new();
        let mut commented: Vec<(String, String)> = Vec::new();
        for chunk in order(&fresh).chunks(crate::beads::MAX_DRAFT_ISSUES) {
            let drafts: Vec<DraftIssue> = chunk
                .iter()
                .map(|b| DraftIssue {
                    key: b.id.clone(),
                    title: b.title.clone(),
                    issue_type: b.issue_type.clone(),
                    priority: b.priority,
                    labels: b.labels.clone(),
                    description: b.description.clone(),
                    // This bd drops `deps` in a graph plan; links are added below.
                    blocked_by: Vec::new(),
                })
                .collect();
            let ids = self.create(home, project_id, &drafts).await?;
            for b in chunk {
                let Some(bead) = ids.get(&b.id) else { continue };
                known.insert(b.id.clone(), bead.clone());
                out.created += 1;
                if let Some(reason) = &b.close_reason {
                    to_close.push((bead.clone(), reason.clone()));
                }
                for c in &b.comments {
                    commented.push((bead.clone(), c.clone()));
                }
            }
        }
        for b in &fresh {
            let Some(bead) = known.get(&b.id) else {
                continue;
            };
            for target in &b.blocked_by {
                // A blocker that is not in this home's backlog is only named
                // in the description.
                if let Some(blocker) = known.get(target) {
                    self.bd(&dir, &["dep", "add", bead, blocker], &[]).await?;
                }
            }
        }
        for batch in to_close.chunks(CLOSE_BATCH) {
            let mut args: Vec<String> = vec!["close".into()];
            args.extend(batch.iter().map(|(id, _)| id.clone()));
            for (_, reason) in batch {
                args.push("--reason".into());
                args.push(reason.clone());
            }
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            self.bd(&dir, &args, &[]).await?;
        }
        for (bead, text) in commented {
            let text = cut_bytes(&text, MAX_COMMENT);
            let file = tempfile::Builder::new()
                .prefix("quark-comment-")
                .suffix(".txt")
                .tempfile_in(&dir)
                .map_err(|e| BeadsError::Invalid(e.to_string()))?;
            std::fs::write(file.path(), text).map_err(|e| BeadsError::Invalid(e.to_string()))?;
            let path = file.path().to_string_lossy().into_owned();
            self.bd(&dir, &["comments", "add", &bead, "-f", &path], &[])
                .await?;
        }
        Ok(out)
    }

    /// The firstmate ids of the beads an earlier import made, with each
    /// bead's id.
    pub async fn imported_firstmate(
        &self,
        home: &Path,
        project_id: &str,
    ) -> Result<BTreeMap<String, String>, BeadsError> {
        let dir = Self::ready_dir(home, project_id)?;
        Ok(self
            .raw_issues(&dir)
            .await?
            .into_iter()
            .flat_map(|i| {
                let id = i.id;
                i.labels
                    .into_iter()
                    .filter_map(|l| l.strip_prefix("firstmate:").map(String::from))
                    .map(move |f| (f, id.clone()))
            })
            .collect())
    }
}

/// `beads` with each one after the ones it waits for, where both are in the
/// list; a cycle keeps its original order.
fn order<'a>(beads: &[&'a ImportBead]) -> Vec<&'a ImportBead> {
    let ids: HashSet<&str> = beads.iter().map(|b| b.id.as_str()).collect();
    let mut done: HashSet<&str> = HashSet::new();
    let mut out: Vec<&ImportBead> = Vec::with_capacity(beads.len());
    let mut left: Vec<&ImportBead> = beads.to_vec();
    while !left.is_empty() {
        let (ready, rest): (Vec<_>, Vec<_>) = left.into_iter().partition(|b| {
            b.blocked_by
                .iter()
                .all(|t| !ids.contains(t.as_str()) || done.contains(t.as_str()))
        });
        if ready.is_empty() {
            out.extend(rest);
            break;
        }
        for b in &ready {
            done.insert(b.id.as_str());
        }
        out.extend(ready);
        left = rest;
    }
    out
}

fn cut_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[cut]", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bead(id: &str, blocked_by: &[&str]) -> ImportBead {
        ImportBead {
            id: id.into(),
            title: id.into(),
            description: String::new(),
            issue_type: "task".into(),
            priority: 2,
            labels: vec![],
            blocked_by: blocked_by.iter().map(|s| s.to_string()).collect(),
            close_reason: None,
            comments: vec![],
        }
    }

    #[test]
    fn blockers_come_first_and_cycles_do_not_hang() {
        let (a, b, c) = (bead("a", &["b"]), bead("b", &[]), bead("c", &["d"]));
        let order: Vec<_> = order(&[&a, &b, &c]).iter().map(|b| b.id.clone()).collect();
        assert_eq!(order, ["b", "c", "a"]);
        let (x, y) = (bead("x", &["y"]), bead("y", &["x"]));
        assert_eq!(super::order(&[&x, &y]).len(), 2);
    }
}
