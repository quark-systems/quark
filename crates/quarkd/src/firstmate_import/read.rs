//! Reading a firstmate home: the files under its `data/`, `config/`,
//! `state/` and `projects/`, in the formats firstmate's scripts write them
//! (firstmate `docs/configuration.md` "Operational home layout"). Nothing
//! here writes to the home.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A file larger than this is listed but not read.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// What a firstmate home holds, as far as an import cares.
#[derive(Debug, Default)]
pub struct Home {
    pub root: PathBuf,
    /// `data/projects.md`.
    pub projects: Vec<FmProject>,
    /// `data/backlog.md`, then `data/done-archive.md`.
    pub backlog: Vec<BacklogRow>,
    /// `data/<task-id>/` files, by task id.
    pub task_files: BTreeMap<String, Vec<PathBuf>>,
    /// Preference bullets of `data/captain.md` and `data/captain-shared.md`.
    pub captain: Vec<String>,
    /// Bullets of `data/learnings.md`.
    pub learnings: Vec<String>,
    /// Bullets of `data/memory-archive.md`, the cold tier.
    pub archived_memory: Vec<String>,
    /// Rows of `data/secondmates.md`.
    pub secondmates: Vec<String>,
    /// `config/crew-dispatch.json` as written.
    pub crew_dispatch: Option<String>,
    /// `config/crew-harness`, unless it is `default`.
    pub crew_harness: Option<String>,
    /// `config/secondmate-harness`'s first line.
    pub secondmate_harness: Option<String>,
    /// `config/bob-quota.json` as written.
    pub bob_quota: Option<String>,
    /// Other files in `config/`, by name.
    pub other_config: Vec<String>,
    /// `state/<task-id>.status`, by task id.
    pub status_logs: BTreeMap<String, PathBuf>,
    /// Task ids with a `state/<task-id>.meta`.
    pub task_meta: Vec<String>,
    /// Receipts in `state/terminal-outcomes/`.
    pub terminal_outcomes: usize,
    /// Extension bindings in `config/extensions.d/`.
    pub extensions: usize,
    /// Whether the home has a `.env` (Relay and mail secrets).
    pub env_file: bool,
    /// Whether away mode was on (`state/.afk`).
    pub away: bool,
    /// Registry files kept as written, relative to the home.
    pub registry_files: Vec<String>,
}

/// One row of `data/projects.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FmProject {
    pub name: String,
    pub mode: Mode,
    /// `+yolo`: firstmate holds merge authority.
    pub yolo: bool,
    pub description: String,
    pub added: Option<String>,
    /// `projects/<name>`, resolved, when it exists.
    pub clone: Option<PathBuf>,
    /// The clone's `origin` URL.
    pub origin: Option<String>,
}

/// A project's ship mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    NoMistakes,
    /// `no-mistakes-prod-only`: no-mistakes for production-bound tasks.
    NoMistakesProdOnly,
    DirectPr,
    LocalOnly,
    /// Firstmate reads a mode it does not know as `no-mistakes`.
    Other(String),
}

impl Mode {
    fn parse(word: &str) -> Self {
        match word {
            "no-mistakes" => Mode::NoMistakes,
            "no-mistakes-prod-only" => Mode::NoMistakesProdOnly,
            "direct-PR" => Mode::DirectPr,
            "local-only" => Mode::LocalOnly,
            other => Mode::Other(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Mode::NoMistakes => "no-mistakes",
            Mode::NoMistakesProdOnly => "no-mistakes-prod-only",
            Mode::DirectPr => "direct-PR",
            Mode::LocalOnly => "local-only",
            Mode::Other(s) => s,
        }
    }
}

/// The backlog section a row sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    InFlight,
    Queued,
    Done,
    /// `data/done-archive.md`.
    Archived,
    /// A section firstmate's backlog does not define.
    Other,
}

/// One task row of the backlog.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BacklogRow {
    pub id: String,
    pub title: String,
    pub checked: bool,
    pub section: Option<Section>,
    pub pr: Option<String>,
    /// Other URLs in the row (issues, links).
    pub links: Vec<String>,
    pub repo: Option<String>,
    pub kind: Option<String>,
    pub priority: Option<u8>,
    /// `(hold: <reason>)`: a captain decision or external wait.
    pub hold: Option<String>,
    pub hold_kind: Option<String>,
    pub hold_until: Option<String>,
    pub blocked_by: Vec<String>,
    /// `data/<id>/report.md` named in the row.
    pub report: Option<String>,
    /// `(since …)`, `(merged …)`, `(reported …)`, `(done …)`, in row order.
    pub dates: Vec<(String, String)>,
    /// The indented lines under the row.
    pub body: Vec<String>,
}

impl BacklogRow {
    pub fn date(&self, word: &str) -> Option<&str> {
        self.dates
            .iter()
            .find(|(w, _)| w == word)
            .map(|(_, d)| d.as_str())
    }
}

/// Reads the firstmate home at `root`. Fails only when `root` is not a
/// firstmate home; files that are missing or unreadable are left out.
pub fn read(root: &Path) -> Result<Home, String> {
    let data = root.join("data");
    if !data.join("projects.md").is_file() && !data.join("backlog.md").is_file() {
        return Err(format!(
            "{} is not a firstmate home: it has no data/projects.md or data/backlog.md",
            root.display()
        ));
    }
    let mut home = Home {
        root: root.to_path_buf(),
        ..Home::default()
    };
    if let Some(text) = text(&data.join("projects.md")) {
        home.projects = parse_projects(&text);
        for p in &mut home.projects {
            let clone = root.join("projects").join(&p.name);
            if clone.is_dir() {
                p.origin = origin(&clone);
                p.clone = Some(clone.canonicalize().unwrap_or(clone));
            }
        }
    }
    if let Some(text) = text(&data.join("backlog.md")) {
        home.backlog = parse_backlog(&text, None);
    }
    if let Some(text) = text(&data.join("done-archive.md")) {
        home.backlog
            .extend(parse_backlog(&text, Some(Section::Archived)));
    }
    for file in ["captain.md", "captain-shared.md"] {
        if let Some(text) = text(&data.join(file)) {
            home.captain.extend(parse_bullets(&text));
        }
    }
    if let Some(text) = text(&data.join("learnings.md")) {
        home.learnings = parse_bullets(&text);
    }
    if let Some(text) = text(&data.join("memory-archive.md")) {
        home.archived_memory = parse_bullets(&text);
    }
    if let Some(text) = text(&data.join("secondmates.md")) {
        home.secondmates = text
            .lines()
            .filter_map(|l| l.strip_prefix("- "))
            .map(|l| l.trim().to_string())
            .collect();
    }
    for file in [
        "projects.md",
        "backlog.md",
        "done-archive.md",
        "captain.md",
        "captain-shared.md",
        "learnings.md",
        "memory-archive.md",
        "secondmates.md",
    ] {
        if data.join(file).is_file() {
            home.registry_files.push(format!("data/{file}"));
        }
    }
    home.task_files = task_files(&data);
    read_config(&root.join("config"), &mut home);
    read_state(&root.join("state"), &mut home);
    home.env_file = root.join(".env").is_file();
    Ok(home)
}

/// A UTF-8 file of at most [`MAX_FILE_BYTES`].
pub fn text(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

fn origin(clone: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(clone)
        .args(["remote", "get-url", "origin"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .ok()?;
    let url = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !url.is_empty()).then_some(url)
}

/// `data/projects.md`: `- <name> [<mode>[ +yolo]] - <description> (added <date>)`,
/// the mode part optional.
pub fn parse_projects(text: &str) -> Vec<FmProject> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) else {
            continue;
        };
        let rest = rest.trim();
        let (name, mut rest) = rest.split_once(' ').unwrap_or((rest, ""));
        if name.is_empty() {
            continue;
        }
        let (mut mode, mut yolo) = (Mode::NoMistakes, false);
        rest = rest.trim_start();
        if let Some(inner) = rest.strip_prefix('[') {
            if let Some((flags, after)) = inner.split_once(']') {
                let mut words = flags.split_whitespace();
                if let Some(m) = words.next() {
                    mode = Mode::parse(m);
                }
                yolo = words.any(|w| w == "+yolo");
                rest = after.trim_start();
            }
        }
        let mut description = rest.strip_prefix('-').unwrap_or(rest).trim().to_string();
        let mut added = None;
        if let Some((head, tail)) = description.rsplit_once("(added ") {
            if let Some(date) = tail.strip_suffix(')') {
                added = Some(date.trim().to_string());
                description = head.trim_end().to_string();
            }
        }
        out.push(FmProject {
            name: name.to_string(),
            mode,
            yolo,
            description,
            added,
            clone: None,
            origin: None,
        });
    }
    out
}

/// Colon metadata a row ends with: `(repo: alpha)`.
const COLON_KEYS: [&str; 6] = [
    "repo",
    "kind",
    "priority",
    "hold",
    "hold-kind",
    "hold-until",
];
/// Date metadata: `(merged 2026-08-25)`.
const DATE_WORDS: [&str; 5] = ["since", "merged", "reported", "done", "added"];

/// The backlog, firstmate's tasks-axi markdown: `## In flight`, `## Queued`
/// and `## Done` sections of `- [ ] <id> - <title> … (key: value)` rows, each
/// followed by indented body lines. `section` overrides the headings (the
/// done archive).
pub fn parse_backlog(text: &str, section: Option<Section>) -> Vec<BacklogRow> {
    let mut rows: Vec<BacklogRow> = Vec::new();
    let mut current = section.unwrap_or(Section::Other);
    let mut in_row = false;
    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            if section.is_none() {
                current = match heading.trim().to_ascii_lowercase().as_str() {
                    "in flight" => Section::InFlight,
                    "queued" => Section::Queued,
                    "done" => Section::Done,
                    _ => Section::Other,
                };
            }
            in_row = false;
            continue;
        }
        if let Some(mut row) = parse_row(line) {
            row.section = Some(current);
            rows.push(row);
            in_row = true;
            continue;
        }
        if in_row && (line.starts_with(' ') || line.starts_with('\t') || line.trim().is_empty()) {
            if let Some(row) = rows.last_mut() {
                row.body.push(line.trim_end().to_string());
            }
            continue;
        }
        in_row = false;
    }
    for row in &mut rows {
        while row.body.last().is_some_and(|l| l.trim().is_empty()) {
            row.body.pop();
        }
        let indent = row
            .body
            .iter()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.len() - l.trim_start().len())
            .min()
            .unwrap_or(0);
        for l in &mut row.body {
            *l = l.get(indent..).unwrap_or("").to_string();
        }
    }
    rows
}

fn parse_row(line: &str) -> Option<BacklogRow> {
    let rest = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))?;
    let (checked, rest) = if let Some(r) = rest.strip_prefix("[ ] ") {
        (false, r)
    } else if let Some(r) = rest
        .strip_prefix("[x] ")
        .or_else(|| rest.strip_prefix("[X] "))
    {
        (true, r)
    } else {
        // The legacy form: `- **<id>** - <rest>`.
        let (id, after) = rest.strip_prefix("**")?.split_once("**")?;
        return Some(fill_row(id, after.trim_start(), false));
    };
    let rest = rest.trim_start();
    let (id, after) = rest.split_once(char::is_whitespace)?;
    Some(fill_row(id, after.trim_start(), checked))
}

fn fill_row(id: &str, rest: &str, checked: bool) -> BacklogRow {
    let mut row = BacklogRow {
        id: id.trim().to_string(),
        checked,
        ..BacklogRow::default()
    };
    let mut rest = rest
        .strip_prefix("- ")
        .or_else(|| rest.strip_prefix('-'))
        .unwrap_or(rest)
        .trim()
        .to_string();
    // Metadata groups at the end of the row, last first.
    let mut dates = Vec::new();
    while rest.ends_with(')') {
        let Some(open) = matching_open(&rest) else {
            break;
        };
        let inner = &rest[open + 1..rest.len() - 1];
        if let Some((key, value)) = inner.split_once(':') {
            let (key, value) = (key.trim(), value.trim().to_string());
            if COLON_KEYS.contains(&key) {
                match key {
                    "repo" => row.repo = Some(value),
                    "kind" => row.kind = Some(value),
                    "priority" => row.priority = value.parse().ok(),
                    "hold" => row.hold = Some(value),
                    "hold-kind" => row.hold_kind = Some(value),
                    _ => row.hold_until = Some(value),
                }
                rest.truncate(open);
                rest.truncate(rest.trim_end().len());
                continue;
            }
        }
        if let Some((word, date)) = inner.split_once(' ') {
            if DATE_WORDS.contains(&word) {
                dates.push((word.to_string(), date.trim().to_string()));
                rest.truncate(open);
                rest.truncate(rest.trim_end().len());
                continue;
            }
        }
        break;
    }
    dates.reverse();
    row.dates = dates;
    // Tokens inside the title.
    let mut title = Vec::new();
    let mut words = rest.split(' ').peekable();
    while let Some(word) = words.next() {
        if word == "blocked-by:" {
            if let Some(target) = words.next() {
                row.blocked_by.push(target.to_string());
            }
            continue;
        }
        if word.starts_with("https://") || word.starts_with("http://") {
            if word.contains("/pull/") || word.contains("/merge_requests/") {
                row.pr.get_or_insert_with(|| word.to_string());
            } else {
                row.links.push(word.to_string());
            }
            continue;
        }
        if word.starts_with("data/") && word.ends_with("/report.md") {
            row.report = Some(word.to_string());
            continue;
        }
        title.push(word);
    }
    row.title = title.join(" ").trim().trim_end_matches(" -").to_string();
    if row.title.is_empty() {
        row.title = row.id.clone();
    }
    row
}

/// The index of the `(` that the final `)` of `s` closes.
fn matching_open(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in s.char_indices().rev() {
        match c {
            ')' => depth += 1,
            '(' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The bullets of a memory file (captain preferences, learnings), each with
/// its continuation lines and without its tier marker (`<!--a:2026-08-01-->`).
pub fn parse_bullets(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut open = false;
    for line in text.lines() {
        if let Some(item) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            out.push(item.to_string());
            open = true;
        } else if open && (line.starts_with("  ") || line.starts_with('\t')) {
            if let Some(last) = out.last_mut() {
                last.push('\n');
                last.push_str(line.trim());
            }
        } else {
            open = false;
        }
    }
    out.into_iter()
        .map(|b| strip_markers(&b))
        .filter(|b| !b.is_empty())
        .collect()
}

fn strip_markers(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => {
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out.lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// `data/<task-id>/` files, without firstmate's hidden temporaries.
fn task_files(data: &Path) -> BTreeMap<String, Vec<PathBuf>> {
    let mut out = BTreeMap::new();
    let Ok(dirs) = std::fs::read_dir(data) else {
        return out;
    };
    for dir in dirs.flatten() {
        let path = dir.path();
        let name = dir.file_name().to_string_lossy().into_owned();
        if !path.is_dir()
            || name.starts_with('.')
            || matches!(
                name.as_str(),
                "extensions" | "handoff" | "remote-secondmates"
            )
        {
            continue;
        }
        let mut files: Vec<PathBuf> = std::fs::read_dir(&path)
            .into_iter()
            .flatten()
            .flatten()
            .map(|f| f.path())
            .filter(|f| {
                f.is_file()
                    && !f
                        .file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with('.'))
            })
            .collect();
        files.sort();
        if !files.is_empty() {
            out.insert(name, files);
        }
    }
    out
}

fn first_line(path: &Path) -> Option<String> {
    text(path)?
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
}

fn read_config(config: &Path, home: &mut Home) {
    home.crew_dispatch = text(&config.join("crew-dispatch.json"));
    home.crew_harness = first_line(&config.join("crew-harness")).filter(|h| h != "default");
    home.secondmate_harness = first_line(&config.join("secondmate-harness"));
    home.bob_quota = text(&config.join("bob-quota.json"));
    home.extensions = std::fs::read_dir(config.join("extensions.d"))
        .map(|d| d.flatten().count())
        .unwrap_or(0);
    let known = [
        "crew-dispatch.json",
        "crew-harness",
        "secondmate-harness",
        "bob-quota.json",
        "extensions.d",
    ];
    let mut other: Vec<String> = std::fs::read_dir(config)
        .into_iter()
        .flatten()
        .flatten()
        .map(|f| f.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.') && !known.contains(&n.as_str()))
        .collect();
    other.sort();
    home.other_config = other;
}

fn read_state(state: &Path, home: &mut Home) {
    for f in std::fs::read_dir(state).into_iter().flatten().flatten() {
        let name = f.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || !f.path().is_file() {
            continue;
        }
        if let Some(id) = name.strip_suffix(".status") {
            home.status_logs.insert(id.to_string(), f.path());
        } else if let Some(id) = name.strip_suffix(".meta") {
            home.task_meta.push(id.to_string());
        }
    }
    home.task_meta.sort();
    home.terminal_outcomes = std::fs::read_dir(state.join("terminal-outcomes"))
        .map(|d| d.flatten().count())
        .unwrap_or(0);
    home.away = first_line(&state.join(".afk")).is_some();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_project_rows() {
        let p = parse_projects(
            "# Projects\n\n\
             - hawkeye [direct-PR] - AI-powered WHEEL strategy trading platform (Supabase-backed) (added 2026-07-16)\n\
             - sushibot [no-mistakes-prod-only +yolo] - Self-hosted bots (added 2026-08-23)\n\
             - legacy - An old row with no mode (added 2026-01-01)\n",
        );
        assert_eq!(p.len(), 3);
        assert_eq!(p[0].name, "hawkeye");
        assert_eq!(p[0].mode, Mode::DirectPr);
        assert!(!p[0].yolo);
        assert_eq!(
            p[0].description,
            "AI-powered WHEEL strategy trading platform (Supabase-backed)"
        );
        assert_eq!(p[0].added.as_deref(), Some("2026-07-16"));
        assert_eq!(p[1].mode, Mode::NoMistakesProdOnly);
        assert!(p[1].yolo);
        assert_eq!(p[2].mode, Mode::NoMistakes);
        assert_eq!(p[2].description, "An old row with no mode");
    }

    #[test]
    fn reads_backlog_rows_and_bodies() {
        let rows = parse_backlog(
            "# Backlog\n\n## In flight\n\
             - [ ] ship-task - Ship Task https://github.com/o/r/pull/9 (repo: alpha) (kind: ship) (priority: 1) (since 2026-07-07)\n\
             \x20 Preserve this detail.\n\
             \x20   nested\n\
             ## Queued\n\
             - [ ] queued-task - Queued Task blocked-by: ship-task (repo: alpha) (kind: ship)\n\
             - [ ] dated-hold - Dated call (repo: s) (kind: captain) (hold: revisit later) (hold-kind: captain) (hold-until: 2026-12-01)\n\
             \x20 Captain hold set: 2026-07-20T00:00:00Z\n\
             ## Done\n\
             - [x] spec - Discovery phase (/skill:discover → research) for v1 (repo: sushibot) (kind: scout) (done 2026-08-25)\n\
             - [x] scout - Scout data/scout/report.md (repo: alpha) (kind: scout) (reported 2026-07-01)\n",
            None,
        );
        assert_eq!(rows.len(), 5);
        let ship = &rows[0];
        assert_eq!(ship.section, Some(Section::InFlight));
        assert_eq!(ship.title, "Ship Task");
        assert_eq!(ship.pr.as_deref(), Some("https://github.com/o/r/pull/9"));
        assert_eq!(ship.repo.as_deref(), Some("alpha"));
        assert_eq!(ship.priority, Some(1));
        assert_eq!(ship.date("since"), Some("2026-07-07"));
        assert_eq!(ship.body, vec!["Preserve this detail.", "  nested"]);
        assert_eq!(rows[1].blocked_by, vec!["ship-task"]);
        assert_eq!(rows[1].section, Some(Section::Queued));
        let hold = &rows[2];
        assert_eq!(hold.hold.as_deref(), Some("revisit later"));
        assert_eq!(hold.hold_kind.as_deref(), Some("captain"));
        assert_eq!(hold.hold_until.as_deref(), Some("2026-12-01"));
        assert_eq!(hold.kind.as_deref(), Some("captain"));
        let spec = &rows[3];
        assert!(spec.checked);
        assert_eq!(
            spec.title,
            "Discovery phase (/skill:discover → research) for v1"
        );
        assert_eq!(spec.date("done"), Some("2026-08-25"));
        assert_eq!(rows[4].report.as_deref(), Some("data/scout/report.md"));
        assert_eq!(rows[4].title, "Scout");
    }

    #[test]
    fn archive_rows_are_archived_and_legacy_rows_parse() {
        let rows = parse_backlog(
            "## Done\n- [x] old - Old work (merged 2026-01-02)\n- **legacy** - Legacy row (repo: r)\n",
            Some(Section::Archived),
        );
        assert_eq!(rows[0].section, Some(Section::Archived));
        assert_eq!(rows[1].id, "legacy");
        assert_eq!(rows[1].repo.as_deref(), Some("r"));
    }

    #[test]
    fn bullets_lose_tier_markers() {
        let b = parse_bullets(
            "<!-- memory tiers: see the stow skill -->\n# Captain\n\
             - Prefer small PRs <!--P-->\n\
             - Run the gates first <!--a:2026-08-01/2-->\n\
             \x20 even for docs\n\
             - <!--g-->\n",
        );
        assert_eq!(
            b,
            vec!["Prefer small PRs", "Run the gates first\neven for docs"]
        );
    }
}
