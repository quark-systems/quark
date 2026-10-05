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
//!
//! Any entry can be promoted to user-level memory: a copy under
//! `~/.quark/memory/` (see [`crate::provision::Layout::user_memory`]) that
//! every Project's coordinator reads, in the same file format plus where it
//! came from.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use quark_systems::{MemoryCommit, MemoryEntry, MemoryEvidence, MemorySource, UserMemoryEntry};

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

/// The entry's front-matter lines: evidence and dates.
fn front(e: &MemoryEntry) -> String {
    let source = e.source.map(|s| match s {
        MemorySource::Worker => "worker",
        MemorySource::Coordinator => "coordinator",
    });
    let files: Vec<String> = e.evidence.files.iter().map(|f| q(f)).collect();
    let mut m = String::new();
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
    m
}

/// The entry file: front matter with evidence and dates, then the text.
pub fn render(e: &MemoryEntry) -> String {
    format!("---\n{}---\n\n{}\n", front(e), e.text.trim())
}

/// Front matter and text of an entry file; `None` for a file without front
/// matter.
fn split_front(body: &str) -> Option<(&str, &str)> {
    body.strip_prefix("---\n")?.split_once("\n---\n")
}

/// A front-matter value: a JSON string, or `null`.
fn scalar(value: &str) -> Option<String> {
    serde_json::from_str::<Option<String>>(value.trim())
        .ok()
        .flatten()
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
    let Some((front, text)) = split_front(body) else {
        return entry;
    };
    entry.text = text.trim().to_string();
    for line in front.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        let s = || scalar(value);
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
    let added = added_in(bare)?;
    let mut entries = Vec::new();
    for name in names.lines().filter(|n| !n.starts_with('.')) {
        let path = format!("{MEMORY_DIR}/{name}");
        let object = format!("main:{path}");
        if git(bare, &["cat-file", "-t", &object])? != "blob" {
            continue;
        }
        let body = git(bare, &["show", &object])?;
        let mut entry = parse(project_id, &path, &body);
        entry.commit = added.get(&path).cloned();
        entries.push(entry);
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

/// The commit on `main` that added each file under `memory/`: the latest
/// one, for a file removed and added again.
fn added_in(bare: &Path) -> Result<HashMap<String, String>, String> {
    let log = git(
        bare,
        &[
            "-c",
            "core.quotePath=false",
            "log",
            "--diff-filter=A",
            "--name-only",
            "--format=commit %H",
            "main",
            "--",
            MEMORY_DIR,
        ],
    )?;
    let mut added = HashMap::new();
    let mut commit = "";
    for line in log.lines() {
        if let Some(sha) = line.strip_prefix("commit ") {
            commit = sha;
        } else if !line.is_empty() {
            added
                .entry(line.to_string())
                .or_insert_with(|| commit.to_string());
        }
    }
    Ok(added)
}

/// A commit on the Project repo's `main` with what it changed under
/// `memory/`. `None` when `commit` is not a commit id on `main`.
pub fn commit(bare: &Path, commit: &str) -> Result<Option<MemoryCommit>, String> {
    let hex = (7..=64).contains(&commit.len()) && commit.chars().all(|c| c.is_ascii_hexdigit());
    let object = format!("{commit}^{{commit}}");
    if !hex
        || git(bare, &["rev-parse", "--verify", "--quiet", &object]).is_err()
        || git(bare, &["merge-base", "--is-ancestor", commit, "main"]).is_err()
    {
        return Ok(None);
    }
    let shown = git(
        bare,
        &[
            "show",
            "--no-color",
            "--no-ext-diff",
            "--format=%H%n%an%n%aI%n%s",
            "--patch",
            commit,
            "--",
            MEMORY_DIR,
        ],
    )?;
    let mut head = shown.splitn(5, '\n');
    let mut next = || head.next().unwrap_or("").to_string();
    let (sha, author, date, subject) = (next(), next(), next(), next());
    Ok(Some(MemoryCommit {
        commit: sha,
        subject,
        author: Some(author).filter(|a| !a.is_empty()),
        date: Some(date).filter(|d| !d.is_empty()),
        patch: next().trim_start_matches('\n').to_string(),
    }))
}

/// The user-level entry file: the Project entry's front matter, then where
/// it was promoted from.
pub fn render_user(e: &UserMemoryEntry) -> String {
    let mut m = front(&MemoryEntry {
        id: String::new(),
        project_id: String::new(),
        path: String::new(),
        text: String::new(),
        evidence: e.evidence.clone(),
        source: e.source,
        date: e.date.clone(),
        accepted_at: None,
        accepted_by: None,
        proposal_id: None,
        commit: None,
    });
    for (key, value) in [
        ("project", &e.project_id),
        ("project_name", &e.project_name),
        ("entry", &e.entry_id),
        ("commit", &e.commit),
        ("promoted_at", &e.promoted_at),
        ("promoted_by", &e.promoted_by),
    ] {
        m.push_str(&format!("{key}: {}\n", q_opt(value.as_deref())));
    }
    format!("---\n{m}---\n\n{}\n", e.text.trim())
}

/// Reads a user-level entry file; one written by hand is all text.
pub fn parse_user(path: &Path, body: &str) -> UserMemoryEntry {
    let shown = path.to_string_lossy();
    let base = parse("", &shown, body);
    let mut entry = UserMemoryEntry {
        id: base.id,
        path: base.path,
        text: base.text,
        evidence: base.evidence,
        source: base.source,
        date: base.date,
        project_id: None,
        project_name: None,
        entry_id: None,
        commit: None,
        promoted_at: None,
        promoted_by: None,
    };
    let Some((front, _)) = split_front(body) else {
        return entry;
    };
    for line in front.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        match key.trim() {
            "project" => entry.project_id = scalar(value),
            "project_name" => entry.project_name = scalar(value),
            "entry" => entry.entry_id = scalar(value),
            "commit" => entry.commit = scalar(value),
            "promoted_at" => entry.promoted_at = scalar(value),
            "promoted_by" => entry.promoted_by = scalar(value),
            _ => {}
        }
    }
    entry
}

/// Every user-level entry in `dir`, by file name. A directory that does not
/// exist yet has none.
pub fn list_user(dir: &Path) -> Result<Vec<UserMemoryEntry>, String> {
    let read = match std::fs::read_dir(dir) {
        Ok(read) => read,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("reading {}: {e}", dir.display())),
    };
    let mut entries = Vec::new();
    for file in read {
        let path = file
            .map_err(|e| format!("reading {}: {e}", dir.display()))?
            .path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or(".");
        if name.starts_with('.') || !name.ends_with(".md") || !path.is_file() {
            continue;
        }
        // A file that is not UTF-8 is not an entry Quark can show.
        if let Ok(body) = std::fs::read_to_string(&path) {
            entries.push(parse_user(&path, &body));
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

/// Copies a Project entry into user-level memory at `dir`, as a new file
/// named after the entry. Returns the user-level entry and whether it was
/// written now: promoting an entry again returns the copy already there.
pub fn promote(
    dir: &Path,
    entry: &MemoryEntry,
    project_name: &str,
    promoted_at: &str,
    promoted_by: &str,
) -> Result<(UserMemoryEntry, bool), String> {
    if let Some(have) = list_user(dir)?.into_iter().find(|u| {
        u.project_id.as_deref() == Some(&entry.project_id)
            && u.entry_id.as_deref() == Some(&entry.id)
    }) {
        return Ok((have, false));
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let mut promoted = UserMemoryEntry {
        id: String::new(),
        path: String::new(),
        text: entry.text.clone(),
        evidence: entry.evidence.clone(),
        source: entry.source,
        date: entry.date.clone(),
        project_id: Some(entry.project_id.clone()),
        project_name: Some(project_name.to_string()),
        entry_id: Some(entry.id.clone()),
        commit: entry.commit.clone(),
        promoted_at: Some(promoted_at.to_string()),
        promoted_by: Some(promoted_by.to_string()),
    };
    let body = render_user(&promoted);
    // Another Project may have an entry of the same name: never replace it.
    for n in 1.. {
        let id = if n == 1 {
            entry.id.clone()
        } else {
            format!("{}-{n}", entry.id)
        };
        let path = dir.join(format!("{id}.md"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(body.as_bytes())
                    .map_err(|e| format!("writing {}: {e}", path.display()))?;
                promoted.id = id;
                promoted.path = path.to_string_lossy().into_owned();
                return Ok((promoted, true));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("writing {}: {e}", path.display())),
        }
    }
    unreachable!("the loop returns")
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
    fn promotes_once_and_never_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("memory");
        assert_eq!(list_user(&root).unwrap(), []);
        let mut e = parse("prj_1", "memory/2026-10-02-x.md", "Keep it small.\n");
        e.evidence.task_title = Some("Fix: login".into());
        e.commit = Some("abc1234".into());

        let (first, written) = promote(&root, &e, "Quark", "2026-10-03T00:00:00Z", "matt").unwrap();
        assert!(written);
        assert_eq!(first.id, "2026-10-02-x");
        assert_eq!(first.project_name.as_deref(), Some("Quark"));
        assert_eq!(first.commit.as_deref(), Some("abc1234"));
        let file = std::fs::read_to_string(root.join("2026-10-02-x.md")).unwrap();
        assert!(file.contains("project: \"prj_1\"\n"), "{file}");
        assert!(file.ends_with("---\n\nKeep it small.\n"), "{file}");
        let (again, written) = promote(&root, &e, "Quark", "later", "someone").unwrap();
        assert!(!written);
        assert_eq!(again, first);

        // The same file name from another Project lands beside it.
        e.project_id = "prj_2".into();
        let (other, written) = promote(&root, &e, "Other", "2026-10-04T00:00:00Z", "matt").unwrap();
        assert!(written);
        assert_eq!(other.id, "2026-10-02-x-2");
        std::fs::write(root.join("by-hand.md"), "# Note\n").unwrap();
        std::fs::write(root.join(".hidden.md"), "no").unwrap();
        let ids: Vec<_> = list_user(&root)
            .unwrap()
            .into_iter()
            .map(|u| u.id)
            .collect();
        assert_eq!(ids, ["2026-10-02-x-2", "2026-10-02-x", "by-hand"]);
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
