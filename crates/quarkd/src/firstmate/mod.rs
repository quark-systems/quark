//! Reading a firstmate home, so Quark can be seeded from it.
//!
//! [`read`] is pure and read-only: it opens files in the home with
//! `std::fs` and nothing else. It never runs firstmate's `bin/` scripts
//! (including the `*.check.sh` polls under `state/`), never runs `bd` or a
//! git command that can fetch in a project clone, and never opens `.env`.
//! The API's import then writes only to Quark's own home. What each category of
//! firstmate data becomes is in `docs/configuration/firstmate-import.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use quark_systems::{DeliveryPolicy, ImportFirstmate};

pub mod backlog;
pub mod dispatch;
pub mod knowledge;
pub mod workspace;

use backlog::{Item, Section};

/// A file larger than this is not read: nothing firstmate keeps in text is
/// anywhere near it.
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Status lines kept per task.
const STATUS_TAIL: usize = 12;

/// One project in `data/projects.md` or under `projects/`.
#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    pub name: String,
    /// `direct-PR`, `no-mistakes` or `local-only`.
    pub mode: Option<String>,
    pub description: Option<String>,
    /// The clone's `origin` URL, with any credentials removed.
    pub repo_url: Option<String>,
    pub delivery: DeliveryPolicy,
    /// Whether the clone has its own `.beads`, which Quark adopts itself.
    pub has_beads: bool,
}

/// A backlog item with what firstmate kept beside it.
#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub item: Item,
    /// The firstmate project name, when one could be chosen.
    pub project: Option<String>,
    /// Why no project could be chosen.
    pub unrouted: Option<String>,
    /// `state/<id>.meta`: only harness, model, effort and kind.
    pub meta: BTreeMap<String, String>,
    /// The last lines of `state/<id>.status`.
    pub status: Vec<String>,
    pub brief: Option<String>,
    pub report: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Quota {
    /// `config/kiro-quota.json`, when it is valid for `quota/kiro.json`.
    pub kiro_overage_allowed: Option<bool>,
    /// The allowance of `state/bob-quota-anchor.json`.
    pub bob_budget_coins: Option<f64>,
    /// When that anchor was taken (RFC 3339), which says it is stale.
    pub bob_anchor_at: Option<String>,
}

/// Counts of files that are read, listed or deliberately skipped.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Skipped {
    pub status_logs_without_item: usize,
    pub watcher_files: usize,
    pub check_scripts: usize,
    pub task_dirs: usize,
    pub variant_dispatch: usize,
    pub secondmates: bool,
    pub env_file: bool,
    pub backend: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub home: PathBuf,
    pub projects: Vec<Project>,
    /// The dispatch rules as `dispatch.yaml`, with its notes.
    pub dispatch: Option<dispatch::Converted>,
    pub dispatch_error: Option<String>,
    /// `config/crew-harness`'s first line, unless it is `default`: the
    /// harness firstmate ran its crew on.
    pub crew_harness: Option<String>,
    pub quota: Quota,
    pub tasks: Vec<Task>,
    pub captain: Vec<knowledge::Entry>,
    pub learnings: Vec<knowledge::Entry>,
    pub skipped: Skipped,
    pub notes: Vec<String>,
}

fn read_text(path: &Path) -> Result<Option<String>, String> {
    match std::fs::metadata(path) {
        Ok(m) if m.len() > MAX_FILE_BYTES => {
            Err(format!("{} is too large to import", path.display()))
        }
        Ok(m) if m.is_file() => std::fs::read_to_string(path)
            .map(Some)
            .map_err(|e| format!("reading {}: {e}", path.display())),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("reading {}: {e}", path.display())),
    }
}

/// Reads the home at `req.home`. Fails when it does not look like a
/// firstmate home.
pub fn read(req: &ImportFirstmate) -> Result<Plan, String> {
    let home = PathBuf::from(&req.home);
    if !home.is_absolute() {
        return Err("home must be an absolute path".into());
    }
    if !home.join("data").is_dir() && !home.join("config").is_dir() && !home.join("state").is_dir()
    {
        return Err(format!(
            "{} is not a firstmate home: it has no data/, config/ or state/",
            home.display()
        ));
    }
    let mut plan = Plan {
        home: home.clone(),
        ..Plan::default()
    };
    plan.projects = projects(&home)?;
    if !req.projects.is_empty() {
        for want in &req.projects {
            if !plan.projects.iter().any(|p| &p.name == want) {
                return Err(format!("the home has no project `{want}`"));
            }
        }
        plan.projects.retain(|p| req.projects.contains(&p.name));
    }

    if let Some(json) = read_text(&home.join("config/crew-dispatch.json"))? {
        match dispatch::convert(&json) {
            Ok(c) => plan.dispatch = Some(c),
            Err(e) => plan.dispatch_error = Some(e),
        }
    }
    plan.crew_harness = read_text(&home.join("config/crew-harness"))?
        .and_then(|t| t.lines().next().map(|l| l.trim().to_string()))
        .filter(|h| !h.is_empty() && h != "default");
    plan.skipped.variant_dispatch = count_dir(&home.join("config"), |n| {
        (n.starts_with("crew-dispatch-") && n.ends_with(".json")) || n.contains(".bak-")
    });
    plan.skipped.backend = read_text(&home.join("config/backend"))?.map(|s| s.trim().to_string());
    plan.quota = quota(&home, &mut plan.notes)?;

    plan.captain = read_text(&home.join("data/captain.md"))?
        .map(|t| knowledge::captain(&t))
        .unwrap_or_default();
    plan.learnings = read_text(&home.join("data/learnings.md"))?
        .map(|t| knowledge::learnings(&t))
        .unwrap_or_default();

    let mut items = read_text(&home.join("data/backlog.md"))?
        .map(|t| backlog::parse(&t, false))
        .unwrap_or_default();
    if req.include_archive {
        if let Some(t) = read_text(&home.join("data/done-archive.md"))? {
            items.extend(backlog::parse(&t, true));
        }
    }
    let mut seen = std::collections::HashSet::new();
    items.retain(|i| seen.insert(i.id.clone()));
    let routes = Router::new(&plan.projects, &req.routes);
    let mut with_status = std::collections::HashSet::new();
    for item in items {
        let meta = meta(&home, &item.id);
        let status = status_tail(&home, &item.id)?;
        if !status.is_empty() {
            with_status.insert(item.id.clone());
        }
        let (project, unrouted) = routes.route(&item, &meta);
        plan.tasks.push(Task {
            brief: read_text(&home.join("data").join(&item.id).join("brief.md"))?,
            report: read_text(&home.join("data").join(&item.id).join("report.md"))?,
            item,
            project,
            unrouted,
            meta,
            status,
        });
    }
    if !req.projects.is_empty() {
        // Items for projects left out are not this import's.
        plan.tasks.retain(|t| match &t.project {
            Some(p) => req.projects.contains(p),
            None => true,
        });
    }

    let state = home.join("state");
    for name in list_dir(&state) {
        if name.ends_with(".status") {
            let id = name.trim_end_matches(".status");
            if !with_status.contains(id) {
                plan.skipped.status_logs_without_item += 1;
            }
        } else if name.ends_with(".check.sh") || name.ends_with(".check-trust") {
            plan.skipped.check_scripts += 1;
        } else {
            plan.skipped.watcher_files += 1;
        }
    }
    plan.skipped.task_dirs = list_dir(&home.join("data"))
        .iter()
        .filter(|n| home.join("data").join(n).is_dir())
        .count();
    plan.skipped.secondmates = home.join("data/secondmates.md").is_file();
    plan.skipped.env_file = home.join(".env").exists();
    Ok(plan)
}

fn list_dir(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

fn count_dir(dir: &Path, pred: impl Fn(&str) -> bool) -> usize {
    list_dir(dir).iter().filter(|n| pred(n)).count()
}

/// The projects registered in `data/projects.md`, plus any clone under
/// `projects/` that is not.
fn projects(home: &Path) -> Result<Vec<Project>, String> {
    let mut out: Vec<Project> = Vec::new();
    let registry = read_text(&home.join("data/projects.md"))?.unwrap_or_default();
    for line in registry.lines() {
        let Some(rest) = line.strip_prefix("- ") else {
            continue;
        };
        let (head, description) = match rest.split_once(" - ") {
            Some((h, d)) => (h, Some(d.trim().to_string())),
            None => (rest, None),
        };
        let (name, mode) = match head.split_once(" [") {
            Some((n, m)) => (n.trim(), Some(m.trim_end_matches(']').trim().to_string())),
            None => (head.trim(), None),
        };
        if name.is_empty() || name.contains(char::is_whitespace) {
            continue;
        }
        out.push(project(home, name, mode, description));
    }
    for name in list_dir(&home.join("projects")) {
        if home.join("projects").join(&name).is_dir() && !out.iter().any(|p| p.name == name) {
            out.push(project(home, &name, None, None));
        }
    }
    Ok(out)
}

fn project(home: &Path, name: &str, mode: Option<String>, description: Option<String>) -> Project {
    let clone = home.join("projects").join(name);
    let delivery = match mode.as_deref() {
        Some("direct-PR") => DeliveryPolicy::Direct,
        _ => DeliveryPolicy::Gated,
    };
    Project {
        name: name.to_string(),
        repo_url: origin_url(&clone),
        has_beads: clone.join(".beads").is_dir(),
        mode,
        description,
        delivery,
    }
}

/// `origin`'s URL from the clone's `.git/config`, read as text: no git
/// command runs in the clone, so nothing in it can change.
fn origin_url(clone: &Path) -> Option<String> {
    let config = std::fs::read_to_string(clone.join(".git/config")).ok()?;
    let mut in_origin = false;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_origin = line == "[remote \"origin\"]";
        } else if in_origin {
            if let Some(url) = line.strip_prefix("url") {
                let url = url.trim_start().strip_prefix('=')?.trim();
                return Some(without_credentials(url));
            }
        }
    }
    None
}

/// `https://user:token@host/x` as `https://host/x`.
pub fn without_credentials(url: &str) -> String {
    if let Some((scheme, rest)) = url.split_once("://") {
        if let Some((auth, host)) = rest.split_once('@') {
            if !auth.contains('/') {
                return format!("{scheme}://{host}");
            }
        }
    }
    url.to_string()
}

fn meta(home: &Path, id: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(home.join("state").join(format!("{id}.meta"))) else {
        return out;
    };
    // Paths, pane ids and generations are tied to this machine's running
    // session and mean nothing in Quark.
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            if ["harness", "model", "effort", "kind", "project"].contains(&k) {
                let v = if k == "project" {
                    v.rsplit('/').next().unwrap_or(v)
                } else {
                    v
                };
                out.insert(k.to_string(), v.trim().to_string());
            }
        }
    }
    out
}

fn status_tail(home: &Path, id: &str) -> Result<Vec<String>, String> {
    let text = read_text(&home.join("state").join(format!("{id}.status")))?.unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let from = lines.len().saturating_sub(STATUS_TAIL);
    Ok(lines[from..].iter().map(|l| l.to_string()).collect())
}

fn quota(home: &Path, notes: &mut Vec<String>) -> Result<Quota, String> {
    let mut q = Quota::default();
    if let Some(t) = read_text(&home.join("config/kiro-quota.json"))? {
        match serde_json::from_str::<serde_json::Value>(&t) {
            Ok(v) => q.kiro_overage_allowed = v.get("overage_allowed").and_then(|b| b.as_bool()),
            Err(e) => notes.push(format!(
                "config/kiro-quota.json is not valid JSON ({e}); not imported"
            )),
        }
    }
    if let Some(t) = read_text(&home.join("state/bob-quota-anchor.json"))? {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
            q.bob_budget_coins = v.get("budgetCoins").and_then(|b| b.as_f64());
            q.bob_anchor_at = v
                .get("recordedAt")
                .and_then(|s| s.as_str())
                .map(String::from);
        }
    }
    Ok(q)
}

/// Chooses a firstmate project for a backlog item.
struct Router<'a> {
    projects: &'a [Project],
    /// Id prefix to project name, from the request.
    routes: &'a BTreeMap<String, String>,
}

impl<'a> Router<'a> {
    fn new(projects: &'a [Project], routes: &'a BTreeMap<String, String>) -> Self {
        Router { projects, routes }
    }

    fn named(&self, name: &str) -> Option<String> {
        self.projects
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.name.clone())
    }

    /// In order: the item's `repo:`, a route from the request, the project in
    /// its `state/<id>.meta`, the project whose name starts the id, then the
    /// one project that shares the id's first word. More than one candidate
    /// at the last step is not guessed.
    fn route(
        &self,
        item: &Item,
        meta: &BTreeMap<String, String>,
    ) -> (Option<String>, Option<String>) {
        if let Some(repo) = &item.repo {
            return match self.named(repo) {
                Some(p) => (Some(p), None),
                None => (
                    None,
                    Some(format!("repo `{repo}` is not a project of this home")),
                ),
            };
        }
        if let Some((_, p)) = self
            .routes
            .iter()
            .filter(|(prefix, _)| item.id.starts_with(prefix.as_str()))
            .max_by_key(|(prefix, _)| prefix.len())
        {
            return match self.named(p) {
                Some(p) => (Some(p), None),
                None => (
                    None,
                    Some(format!(
                        "route names `{p}`, which is not a project of this home"
                    )),
                ),
            };
        }
        if let Some(p) = meta.get("project").and_then(|p| self.named(p)) {
            return (Some(p), None);
        }
        if let Some(p) = self
            .projects
            .iter()
            .filter(|p| item.id == p.name || item.id.starts_with(&format!("{}-", p.name)))
            .max_by_key(|p| p.name.len())
        {
            return (Some(p.name.clone()), None);
        }
        let word = item.id.split('-').next().unwrap_or(&item.id);
        let same: Vec<&str> = self
            .projects
            .iter()
            .filter(|p| p.name.split('-').next() == Some(word))
            .map(|p| p.name.as_str())
            .collect();
        match same.as_slice() {
            [one] => (Some(one.to_string()), None),
            [] => (None, Some("no project name matches its id".into())),
            many => (
                None,
                Some(format!(
                    "its id fits {}; pass a route for `{word}-`",
                    many.join(", ")
                )),
            ),
        }
    }
}

/// Whether `item` is a captain call rather than work.
pub fn is_decision(item: &Item) -> bool {
    item.hold.is_some() || item.kind.as_deref() == Some("captain")
}

/// Whether the item's hold already has the captain's answer recorded.
pub fn is_answered(item: &Item) -> bool {
    item.note.contains("Resolution recorded by fm-")
}

/// The captain's answer: the text after `Captain decision:`.
pub fn answer(item: &Item) -> Option<String> {
    let at = item.note.find("Captain decision:")?;
    let body = item.note[at + "Captain decision:".len()..].trim();
    let body = body.split("\n\nOrigin:").next().unwrap_or(body).trim();
    (!body.is_empty()).then(|| body.to_string())
}

pub fn section_name(s: Section) -> &'static str {
    match s {
        Section::Done => "Done",
        Section::InFlight => "In flight",
        Section::Queued => "Queued",
        Section::Archive => "Done (archive)",
    }
}

/// A bead for a task, with the notes firstmate kept about it.
pub fn bead_for(task: &Task) -> crate::beads::firstmate::ImportBead {
    let item = &task.item;
    let decision = is_decision(item);
    let answered = decision && is_answered(item);
    let mut labels = vec![
        crate::beads::firstmate::LABEL.to_string(),
        crate::beads::firstmate::id_label(&item.id),
    ];
    if let Some(k) = &item.kind {
        labels.push(format!("kind-{k}"));
    }
    match item.section {
        Section::InFlight => labels.push("was-in-flight".into()),
        Section::Archive => labels.push("archived".into()),
        _ => {}
    }
    let mut d: Vec<String> = Vec::new();
    if let Some(hold) = &item.hold {
        d.push(format!("Held for the captain in firstmate: {hold}"));
    }
    if !item.note.is_empty() {
        d.push(item.note.clone());
    }
    let mut facts = vec![format!(
        "Imported from firstmate backlog ({}).",
        section_name(item.section)
    )];
    for (label, v) in [
        ("since", &item.since),
        ("closed", &item.closed),
        ("link", &item.link),
    ] {
        if let Some(v) = v {
            facts.push(format!("{label}: {v}"));
        }
    }
    for k in ["harness", "model", "effort"] {
        if let Some(v) = task.meta.get(k) {
            facts.push(format!("{k}: {v}"));
        }
    }
    if !item.blocked_by.is_empty() {
        facts.push(format!(
            "blocked by (firstmate): {}",
            item.blocked_by.join(", ")
        ));
    }
    if item.section == Section::InFlight {
        facts.push(
            "It was running in firstmate. Its worker is not adopted: dispatch it again if the work is still needed."
                .into(),
        );
    }
    d.push(facts.join("\n"));
    let mut comments = Vec::new();
    if !task.status.is_empty() {
        comments.push(format!(
            "firstmate status log (last {} lines):\n{}",
            task.status.len(),
            task.status.join("\n")
        ));
    }
    if let Some(r) = &task.report {
        comments.push(format!(
            "firstmate report (data/{}/report.md):\n\n{r}",
            item.id
        ));
    }
    if let Some(b) = &task.brief {
        comments.push(format!(
            "firstmate brief (data/{}/brief.md):\n\n{b}",
            item.id
        ));
    }
    let close_reason = if answered {
        Some(answer(item).unwrap_or_else(|| "answered in firstmate".into()))
    } else if item.done {
        Some(match (&item.link, &item.closed) {
            (Some(l), Some(c)) => format!("done in firstmate on {c}: {l}"),
            (Some(l), None) => format!("done in firstmate: {l}"),
            (None, Some(c)) => format!("done in firstmate on {c}"),
            (None, None) => "done in firstmate".into(),
        })
    } else {
        None
    };
    crate::beads::firstmate::ImportBead {
        id: item.id.clone(),
        title: item.title.clone(),
        description: d.join("\n\n"),
        issue_type: if decision { "decision" } else { "task" }.into(),
        priority: item
            .priority
            .filter(|p| *p <= 4)
            .unwrap_or(if decision { 1 } else { 2 }),
        labels,
        blocked_by: item.blocked_by.clone(),
        close_reason: close_reason.map(|r| cut(&r, 1000)),
        comments,
    }
}

/// Writes `entries` to user-level memory at `dir` as files named like the
/// ones promotion writes. An entry an earlier import wrote (same `entry`
/// key) is left alone. Returns (written, already there).
pub fn write_user_memory(
    dir: &Path,
    entries: &[knowledge::Entry],
    today: &str,
    at: &str,
) -> Result<(usize, usize), String> {
    let have: std::collections::HashSet<String> = crate::memory::list_user(dir)?
        .into_iter()
        .filter_map(|u| u.entry_id)
        .collect();
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let (mut written, mut present) = (0, 0);
    for e in entries {
        if have.contains(&e.key) {
            present += 1;
            continue;
        }
        let text = cut(&e.text, crate::memory::MAX_TEXT_BYTES - 16);
        if let Err(why) = crate::memory::validate_text(&text) {
            tracing::warn!(key = %e.key, "not importing a firstmate memory entry: {why}");
            continue;
        }
        let date = e.date.clone().unwrap_or_else(|| today.to_string());
        let name = crate::memory::file_name(&date, &text, |n| dir.join(n).exists());
        let entry = quark_systems::UserMemoryEntry {
            id: name.trim_end_matches(".md").to_string(),
            path: dir.join(&name).to_string_lossy().into_owned(),
            text,
            evidence: Default::default(),
            source: None,
            date: Some(date),
            project_id: None,
            project_name: Some("firstmate".into()),
            entry_id: Some(e.key.clone()),
            commit: None,
            promoted_at: Some(at.to_string()),
            promoted_by: Some("firstmate import".into()),
        };
        std::fs::write(dir.join(&name), crate::memory::render_user(&entry))
            .map_err(|e| format!("writing {name}: {e}"))?;
        written += 1;
    }
    Ok((written, present))
}

/// `s` cut to at most `max` bytes, on a character, marked when cut.
fn cut(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max - "…".len();
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}
