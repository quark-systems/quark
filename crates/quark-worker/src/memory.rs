//! The Project's memory as a worker reads it.
//!
//! A worker runs in a code repo that may keep its own Beads database, with
//! its own issues and memories that the repo's `AGENTS.md` asks agents to
//! use. Quark leaves that `bd` alone: the worker gets no `BEADS_DIR`. The
//! Project's memories reach it on Quark's own channel instead: an index in
//! its brief ([`brief_section`]), and the full text ([`recall`]) from the
//! MCP `recall` tool or `GET $QUARK_MEMORY_URL`.

use std::collections::BTreeMap;

use async_trait::async_trait;
use quark_core::{ProjectId, Result};
use serde::{Deserialize, Serialize};

/// What the `memory.primed` and `memory.read` events say, so how sessions
/// use the Project's memory can be seen over time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MemoryEvent {
    /// A session was given the memory.
    Primed {
        /// `coordinator` or `worker`.
        role: String,
        /// Memories the Project had.
        count: usize,
        /// Memories named in what the session was given.
        shown: usize,
        /// Bytes it was given.
        bytes: usize,
    },
    /// A worker asked for memory.
    Read {
        /// `mcp` or `http`.
        via: String,
        key: Option<String>,
        query: Option<String>,
        /// Memories the answer held.
        found: usize,
    },
}

/// Memories `text` (an index or a session's memory) names: its `- ` lines
/// and `## ` headings, less the line saying how many were left out.
pub fn shown(text: &str) -> usize {
    text.lines()
        .filter(|l| (l.starts_with("- `") || l.starts_with("## ")) && !l.starts_with("- …"))
        .count()
}

/// How many memories [`recall`] answers with.
pub fn found(memories: &BTreeMap<String, String>, key: Option<&str>, query: Option<&str>) -> usize {
    if let Some(key) = key {
        return usize::from(memories.contains_key(key));
    }
    if let Some(q) = query {
        let q = q.to_lowercase();
        return memories
            .iter()
            .filter(|(k, t)| k.to_lowercase().contains(&q) || t.to_lowercase().contains(&q))
            .count();
    }
    memories.len()
}

/// Most bytes of the index a brief carries.
pub const MAX_INDEX_BYTES: usize = 4 * 1024;
/// Most characters of one memory's line in the index.
const MAX_LINE_CHARS: usize = 160;
/// Most bytes one `recall` answers with.
pub const MAX_RECALL_BYTES: usize = 16 * 1024;

/// Reads a Project's memories: key to text. quarkd reads them from the
/// Project's Beads database.
#[async_trait]
pub trait ProjectMemory: Send + Sync {
    /// The Project's memories; empty when it has none or no database yet.
    async fn memories(&self, project: &ProjectId) -> Result<BTreeMap<String, String>>;
}

/// The memories' index: one line per memory, its key and the start of its
/// text, cut at `max_bytes` with a note naming how many were left out.
pub fn index(memories: &BTreeMap<String, String>, max_bytes: usize) -> String {
    let mut out = String::new();
    for (shown, (key, text)) in memories.iter().enumerate() {
        let line = format!("- `{key}`: {}\n", first_line(text));
        if out.len() + line.len() > max_bytes {
            out.push_str(&format!(
                "- …and {} more; call `recall` with no arguments for the whole list.\n",
                memories.len() - shown
            ));
            break;
        }
        out.push_str(&line);
    }
    out
}

/// The section a worker's brief ends with, or `None` without memories.
pub fn brief_section(memories: &BTreeMap<String, String>) -> Option<String> {
    if memories.is_empty() {
        return None;
    }
    Some(format!(
        "Project memory: what this Quark Project has learned, kept apart from any Beads \
         database the repo has (use the repo's own `bd` as its instructions say). Each line \
         is a memory's key and how it starts. Read one in full with the `recall` tool, or \
         with `curl -s \"$QUARK_MEMORY_URL?key=<key>\"` (`?query=<words>` searches, no \
         argument lists them all). Report something worth keeping with a `learned` report.\n\n{}",
        index(memories, MAX_INDEX_BYTES)
    ))
}

/// What `recall` answers. With `key`, that memory's text; with `query`, the
/// memories whose key or text contain it (any case); with neither, the
/// whole index.
pub fn recall(
    memories: &BTreeMap<String, String>,
    key: Option<&str>,
    query: Option<&str>,
) -> String {
    if memories.is_empty() {
        return "This Project has no memories yet.".into();
    }
    if let Some(key) = key {
        return match memories.get(key) {
            Some(text) => cut(text.trim(), MAX_RECALL_BYTES),
            None => format!(
                "No memory is named `{key}`. Call `recall` with a `query` to search, or with \
                 no arguments for the list."
            ),
        };
    }
    if let Some(q) = query {
        let q = q.to_lowercase();
        let found: Vec<_> = memories
            .iter()
            .filter(|(k, t)| k.to_lowercase().contains(&q) || t.to_lowercase().contains(&q))
            .collect();
        if found.is_empty() {
            return format!("No memory mentions {q:?}.");
        }
        let mut out = String::new();
        for (k, t) in found {
            out.push_str(&format!("## {k}\n\n{}\n\n", t.trim()));
        }
        return cut(out.trim_end(), MAX_RECALL_BYTES);
    }
    index(memories, MAX_RECALL_BYTES)
}

fn first_line(text: &str) -> String {
    let line = text.trim().lines().next().unwrap_or("").trim();
    if line.chars().count() <= MAX_LINE_CHARS {
        return line.to_string();
    }
    let cut: String = line.chars().take(MAX_LINE_CHARS - 1).collect();
    format!("{}…", cut.trim_end())
}

fn cut(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n\n(Cut here; ask for one `key` at a time.)",
        &text[..end]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mems(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_index_lists_keys_with_their_first_line() {
        let m = mems(&[
            (
                "gates",
                "Run the gates on the committed head.\nNot the tree.",
            ),
            ("aws", "Use the apptio-dse profile."),
        ]);
        assert_eq!(
            index(&m, MAX_INDEX_BYTES),
            "- `aws`: Use the apptio-dse profile.\n- `gates`: Run the gates on the committed head.\n"
        );
    }

    #[test]
    fn a_long_index_is_cut_with_what_was_left_out() {
        let m: BTreeMap<String, String> = (0..100)
            .map(|i| (format!("key-{i:03}"), "x".repeat(150)))
            .collect();
        let out = index(&m, 1024);
        assert!(out.len() < 1024 + 100, "{}", out.len());
        assert!(out.ends_with("more; call `recall` with no arguments for the whole list.\n"));
        let shown = out.lines().filter(|l| l.starts_with("- `key-")).count();
        assert!(out.contains(&format!("…and {} more", 100 - shown)));
    }

    #[test]
    fn counts_what_was_shown_and_found() {
        let m = mems(&[("gates", "Run the gates."), ("aws", "Profile apptio-dse.")]);
        assert_eq!(shown(&index(&m, MAX_INDEX_BYTES)), 2);
        assert_eq!(shown("- `a`: x\n- …and 3 more; call\n## rule\n"), 2);
        assert_eq!(found(&m, Some("gates"), None), 1);
        assert_eq!(found(&m, Some("nope"), None), 0);
        assert_eq!(found(&m, None, Some("APPTIO")), 1);
        assert_eq!(found(&m, None, None), 2);
    }

    #[test]
    fn no_memories_add_no_section() {
        assert_eq!(brief_section(&BTreeMap::new()), None);
        let s = brief_section(&mems(&[("k", "v")])).unwrap();
        assert!(s.contains("repo's own `bd`"));
        assert!(s.ends_with("- `k`: v\n"));
    }

    #[test]
    fn recall_by_key_by_query_and_whole() {
        let m = mems(&[
            ("gates", "Run the gates.\nAlways."),
            ("aws", "Profile apptio-dse."),
        ]);
        assert_eq!(recall(&m, Some("gates"), None), "Run the gates.\nAlways.");
        assert!(recall(&m, Some("nope"), None).starts_with("No memory is named `nope`"));
        assert_eq!(
            recall(&m, None, Some("APPTIO")),
            "## aws\n\nProfile apptio-dse."
        );
        assert!(recall(&m, None, Some("zzz")).starts_with("No memory mentions"));
        assert_eq!(recall(&m, None, None), index(&m, MAX_RECALL_BYTES));
        assert_eq!(
            recall(&BTreeMap::new(), None, None),
            "This Project has no memories yet."
        );
    }
}
