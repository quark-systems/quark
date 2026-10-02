//! Project memory (journey J8): learnings from finished tasks, reviewed as
//! proposals, kept as one file per entry under the Project repo's `memory/`.
//!
//! A learning is a status-log line on the task:
//!
//! ```text
//! learned: Run the gates on the committed head, not the working tree.
//! learned [files=crates/quarkd/src/gates.rs,app/e2e/app.spec.ts]: ...
//! learned [source=coordinator]: ...
//! ```
//!
//! The worker writes it before `done:`, or the coordinator adds it for a
//! finished task. The engine ignores the verb, so the line never changes the
//! task's state. Once the task is finished (in review, done or failed), each
//! line becomes a [`MemoryProposal`](quark_systems::MemoryProposal). An
//! accepted one is committed to the Project repo as a Markdown file whose
//! front matter carries its evidence and date, the same JSON-quoted YAML the
//! rest of the Project repo uses.

use std::path::Path;
use std::process::Command;

use quark_systems::{MemoryEntry, MemoryEvidence, MemorySource};

/// The status verb that reports a learning.
pub const LEARNED_VERB: &str = "learned";

/// Directory of entries in the Project repo.
pub const MEMORY_DIR: &str = "memory";

/// Longest entry text accepted, in bytes.
pub const MAX_TEXT_BYTES: usize = 8 * 1024;

/// Most files kept as evidence for one learning.
pub const MAX_EVIDENCE_FILES: usize = 50;

/// A learning as the status line states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Learning {
    pub text: String,
    /// Files named with `[files=a,b]`; empty when the line names none.
    pub files: Vec<String>,
    pub source: MemorySource,
}

/// Reads a `learned` line: `note` is the text after the colon, `raw` the
/// whole line, whose head may carry `[files=...]` and `[source=...]` tags.
pub fn parse_learning(note: &str, raw: &str) -> Learning {
    let head = raw.split_once(':').map_or("", |(h, _)| h);
    let files = tag(head, "files")
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|f| !f.is_empty())
                .take(MAX_EVIDENCE_FILES)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let source = match tag(head, "source").map(str::trim) {
        Some("coordinator") => MemorySource::Coordinator,
        _ => MemorySource::Worker,
    };
    Learning {
        text: note.trim().to_string(),
        files,
        source,
    }
}

/// Value of a `[name=value]` tag in `s`.
fn tag<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("[{name}=");
    let start = s.find(&open)? + open.len();
    let len = s[start..].find(']')?;
    Some(&s[start..start + len])
}

/// Why `text` cannot be kept as a memory entry, if it cannot.
pub fn validate_text(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("text must not be empty".into());
    }
    if text.len() > MAX_TEXT_BYTES {
        return Err(format!("text is longer than {MAX_TEXT_BYTES} bytes"));
    }
    if text
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err("text must not contain control characters other than newline and tab".into());
    }
    Ok(())
}

/// The entry's file name: the date it was learned and a slug of its first
/// words, e.g. `2026-10-02-run-the-gates-on-the-committed-head.md`. `taken`
/// says whether a name is in use; a numeric suffix makes it unique.
pub fn file_name(date: &str, text: &str, taken: impl Fn(&str) -> bool) -> String {
    let day = date.get(..10).filter(|d| d.len() == 10).unwrap_or(date);
    let mut slug = String::new();
    for word in text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        if !slug.is_empty() && slug.len() + 1 + word.len() > 48 {
            break;
        }
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(&word.to_ascii_lowercase());
        slug.truncate(48);
    }
    if slug.is_empty() {
        slug.push_str("entry");
    }
    let base = format!("{day}-{slug}");
    let mut name = format!("{base}.md");
    let mut n = 2;
    while taken(&name) {
        name = format!("{base}-{n}.md");
        n += 1;
    }
    name
}

/// A YAML double-quoted scalar.
fn q(s: &str) -> String {
    serde_json::to_string(s).expect("strings serialize")
}

fn q_opt(s: Option<&str>) -> String {
    s.map_or_else(|| "null".into(), q)
}

/// The entry file: front matter with evidence and dates, then the text.
pub fn render(e: &MemoryEntry) -> String {
    let source = e.source.map(|s| match s {
        MemorySource::Worker => "worker",
        MemorySource::Coordinator => "coordinator",
    });
    let files: Vec<String> = e.evidence.files.iter().map(|f| q(f)).collect();
    let mut m = String::from("---\n");
    m.push_str(&format!("date: {}\n", q_opt(e.date.as_deref())));
    m.push_str(&format!("source: {}\n", q_opt(source)));
    m.push_str(&format!("task: {}\n", q_opt(e.evidence.task_id.as_deref())));
    m.push_str(&format!(
        "task_title: {}\n",
        q_opt(e.evidence.task_title.as_deref())
    ));
    m.push_str(&format!(
        "pull_request: {}\n",
        q_opt(e.evidence.pull_request_url.as_deref())
    ));
    m.push_str(&format!("files: [{}]\n", files.join(", ")));
    m.push_str(&format!("proposal: {}\n", q_opt(e.proposal_id.as_deref())));
    m.push_str(&format!(
        "accepted_at: {}\n",
        q_opt(e.accepted_at.as_deref())
    ));
    m.push_str(&format!(
        "accepted_by: {}\n",
        q_opt(e.accepted_by.as_deref())
    ));
    m.push_str("---\n\n");
    m.push_str(e.text.trim());
    m.push('\n');
    m
}

/// Reads an entry file. A file without front matter (written by hand) is all
/// text; unknown or malformed front-matter values are left out.
pub fn parse(project_id: &str, path: &str, body: &str) -> MemoryEntry {
    let id = Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string();
    let mut entry = MemoryEntry {
        id,
        project_id: project_id.to_string(),
        path: path.to_string(),
        text: body.trim().to_string(),
        evidence: MemoryEvidence::default(),
        source: None,
        date: None,
        accepted_at: None,
        accepted_by: None,
        proposal_id: None,
        commit: None,
    };
    let Some(rest) = body.strip_prefix("---\n") else {
        return entry;
    };
    let Some((front, text)) = rest.split_once("\n---\n") else {
        return entry;
    };
    entry.text = text.trim().to_string();
    for line in front.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        let s = || serde_json::from_str::<Option<String>>(value).ok().flatten();
        match key.trim() {
            "date" => entry.date = s(),
            "source" => {
                entry.source = match s().as_deref() {
                    Some("worker") => Some(MemorySource::Worker),
                    Some("coordinator") => Some(MemorySource::Coordinator),
                    _ => None,
                }
            }
            "task" => entry.evidence.task_id = s(),
            "task_title" => entry.evidence.task_title = s(),
            "pull_request" => entry.evidence.pull_request_url = s(),
            "files" => {
                entry.evidence.files = serde_json::from_str(value).unwrap_or_default();
            }
            "proposal" => entry.proposal_id = s(),
            "accepted_at" => entry.accepted_at = s(),
            "accepted_by" => entry.accepted_by = s(),
            _ => {}
        }
    }
    entry
}

/// Every entry on the Project repo's `main`, by path. A repo without
/// `memory/` has none.
pub fn list(bare: &Path, project_id: &str) -> Result<Vec<MemoryEntry>, String> {
    let tree = format!("main:{MEMORY_DIR}");
    if git(bare, &["rev-parse", "--verify", "--quiet", &tree]).is_err() {
        return Ok(Vec::new());
    }
    let names = git(bare, &["ls-tree", "--name-only", &tree])?;
    let mut entries = Vec::new();
    for name in names.lines().filter(|n| !n.starts_with('.')) {
        let path = format!("{MEMORY_DIR}/{name}");
        let object = format!("main:{path}");
        if git(bare, &["cat-file", "-t", &object])? != "blob" {
            continue;
        }
        let body = git(bare, &["show", &object])?;
        entries.push(parse(project_id, &path, &body));
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_learning_tags() {
        let l = parse_learning(
            " Gates run on the committed head. ",
            "learned [files=a.rs, b/c.ts ,]: Gates run on the committed head.",
        );
        assert_eq!(l.text, "Gates run on the committed head.");
        assert_eq!(l.files, ["a.rs", "b/c.ts"]);
        assert_eq!(l.source, MemorySource::Worker);

        let l = parse_learning("x", "learned [source=coordinator]: x");
        assert_eq!(l.source, MemorySource::Coordinator);
        assert!(l.files.is_empty());
        // Tags after the colon are part of the text.
        let l = parse_learning("see [files=x]", "learned: see [files=x]");
        assert!(l.files.is_empty());
    }

    #[test]
    fn names_files_by_date_and_slug() {
        let name = file_name(
            "2026-10-02T10:00:00Z",
            "Run the gates on the *committed* head, not the working tree!",
            |_| false,
        );
        assert_eq!(
            name,
            "2026-10-02-run-the-gates-on-the-committed-head-not-the.md"
        );
        assert!(name.len() <= 10 + 1 + 48 + 3);
        let taken = ["2026-10-02-entry.md", "2026-10-02-entry-2.md"];
        assert_eq!(
            file_name("2026-10-02T10:00:00Z", "!!!", |n| taken.contains(&n)),
            "2026-10-02-entry-3.md"
        );
        let long = "a".repeat(80);
        assert_eq!(
            file_name("2026-10-02", &long, |_| false),
            format!("2026-10-02-{}.md", "a".repeat(48))
        );
    }

    #[test]
    fn render_round_trips() {
        let e = MemoryEntry {
            id: "2026-10-02-x".into(),
            project_id: "prj_1".into(),
            path: "memory/2026-10-02-x.md".into(),
            text: "Line one: \"quoted\"\n\n---\nstill text".into(),
            evidence: MemoryEvidence {
                task_id: Some("tsk_1".into()),
                task_title: Some("Fix: login".into()),
                pull_request_url: Some("https://github.com/o/r/pull/1".into()),
                files: vec!["a.rs".into(), "b \"c\".rs".into()],
            },
            source: Some(MemorySource::Coordinator),
            date: Some("2026-10-02T10:00:00Z".into()),
            accepted_at: Some("2026-10-02T11:00:00Z".into()),
            accepted_by: Some("matt".into()),
            proposal_id: Some("mpr_1".into()),
            commit: None,
        };
        let body = render(&e);
        assert!(
            body.starts_with("---\ndate: \"2026-10-02T10:00:00Z\"\n"),
            "{body}"
        );
        assert_eq!(parse("prj_1", &e.path, &body), e);
    }

    #[test]
    fn hand_written_file_is_all_text() {
        let e = parse("p", "memory/note.md", "# Note\n\nKeep it short.\n");
        assert_eq!(e.id, "note");
        assert_eq!(e.text, "# Note\n\nKeep it short.");
        assert_eq!(e.date, None);
    }

    #[test]
    fn validates_text() {
        assert!(validate_text("ok\n\tfine").is_ok());
        assert!(validate_text(" ").is_err());
        assert!(validate_text("\u{1b}[2J").is_err());
        assert!(validate_text(&"x".repeat(MAX_TEXT_BYTES + 1)).is_err());
    }
}
