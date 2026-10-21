//! What a firstmate home becomes in Quark. Pure: [`plan`] reads nothing but
//! the [`Home`] it is given.
//!
//! | firstmate                                   | Quark                                                      |
//! |---------------------------------------------|------------------------------------------------------------|
//! | `data/projects.md` rows with a clone        | the Project's repos, cloned from each clone's `origin`     |
//! | ship modes, `+yolo`                         | delivery (`direct` only when every project is `direct-PR`), standing approval (`+yolo` on every project), per-repo modes in `instructions.md` |
//! | `data/backlog.md`, `data/done-archive.md`   | beads labelled `firstmate`: queued `open`, in flight `in_progress`, done `closed` |
//! | captain holds `(hold: …)`                   | a human gate on the task's bead                            |
//! | `blocked-by:`                               | bead dependencies                                          |
//! | `data/<id>/brief.md`                        | the bead's design, and a copy in `library/firstmate/tasks/<id>/` |
//! | other `data/<id>/` files, `state/<id>.status` | `library/firstmate/tasks/<id>/`                          |
//! | `config/crew-dispatch.json`                 | `dispatch.yaml`, harness names as Quark ids                |
//! | `config/crew-harness`                       | the coordinator's agent config                             |
//! | `data/captain.md`, `data/captain-shared.md` | user-level memory                                          |
//! | `data/learnings.md`                         | Project memory                                             |
//! | `config/bob-quota.json`                     | `~/.quark/quota/bob.json`, when there is none              |
//! | registry files                              | `library/firstmate/` as written                            |
//!
//! Live runtime state (task metadata, wake queues, watcher and Relay state),
//! secondmates, secrets and firstmate-only settings are listed and skipped.

use std::collections::{BTreeSet, HashSet};

use quark_systems::{
    AgentConfig, CreateProject, DeliveryPolicy, DispatchFloor, DispatchProfile, DispatchRuleSpec,
    DispatchRulesDraft, DispatchSelect, FirstmateImportPlan, ImportFirstmate, ImportItem,
    ImportOutcome, RepoSource,
};
use serde::Deserialize;

use super::read::{self, BacklogRow, Home, Mode, Section};

/// Where firstmate's files go in the Project repo.
pub const LIBRARY: &str = "library/firstmate";

/// The coordinator harness when firstmate names none Quark knows.
pub const FALLBACK_HARNESS: &str = "claude-code";

const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// Everything an import writes once the Project is provisioned.
#[derive(Debug, Clone)]
pub struct Seed {
    pub plan: FirstmateImportPlan,
    /// Replaces `dispatch.yaml`'s rules.
    pub dispatch: Option<DispatchRulesDraft>,
    /// Appended to `instructions.md`.
    pub instructions: String,
    /// Project memory entries.
    pub memory: Vec<String>,
    /// User-level memory entries.
    pub user_memory: Vec<String>,
    /// Project repo files: path, body.
    pub library: Vec<(String, String)>,
    pub issues: Vec<SeedIssue>,
    /// `~/.quark/quota/bob.json`, when there is none.
    pub bob_quota: Option<String>,
}

/// One backlog task as a bead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedIssue {
    /// The firstmate task id.
    pub key: String,
    pub title: String,
    pub description: String,
    /// The task's brief.
    pub design: Option<String>,
    pub labels: Vec<String>,
    pub priority: u8,
    pub status: SeedStatus,
    /// A captain hold's reason: the bead waits on a human gate.
    pub hold: Option<String>,
    /// Firstmate ids of the tasks this one waits on.
    pub blocked_by: Vec<String>,
    pub pull_request: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedStatus {
    Open,
    InProgress,
    Closed { reason: String },
}

/// Plans importing `home`. `resolve` maps a harness name firstmate uses
/// (`claude`, `pi`) to a Quark harness id. `today` dates the import.
pub fn plan(
    home: &Home,
    input: &ImportFirstmate,
    resolve: &dyn Fn(&str) -> Option<String>,
    today: &str,
) -> Result<Seed, String> {
    let mut warnings = Vec::new();
    let mut items = Vec::new();

    // Projects become repos.
    let mut repos = Vec::new();
    let mut imported_projects = Vec::new();
    for p in &home.projects {
        let url = p
            .origin
            .clone()
            .or_else(|| p.clone.as_ref().map(|c| c.to_string_lossy().into_owned()));
        match url {
            Some(url) => {
                repos.push(RepoSource {
                    url,
                    name: Some(p.name.clone()),
                });
                imported_projects.push(p);
            }
            None => warnings.push(format!(
                "{} has no clone in firstmate, so it is not one of the project's repositories",
                p.name
            )),
        }
    }
    if repos.is_empty() {
        return Err(
            "no project in data/projects.md has a clone under projects/ to import".to_string(),
        );
    }
    items.push(item(
        "Projects",
        &["data/projects.md"],
        home.projects.len(),
        ImportOutcome::Imported,
        format!(
            "{} repositor{} of the project, cloned from where each clone pushes",
            repos.len(),
            if repos.len() == 1 { "y" } else { "ies" }
        ),
    ));

    let delivery = if imported_projects.iter().all(|p| p.mode == Mode::DirectPr) {
        DeliveryPolicy::Direct
    } else {
        DeliveryPolicy::Gated
    };
    let standing_approval = imported_projects.iter().all(|p| p.yolo);
    items.push(item(
        "Ship modes and merge authority",
        &["data/projects.md"],
        imported_projects.len(),
        ImportOutcome::Imported,
        format!(
            "{} delivery{}; each repository's own mode is in the project's instructions",
            match delivery {
                DeliveryPolicy::Direct => "Direct",
                DeliveryPolicy::Gated => "Gated",
            },
            if standing_approval {
                ", and green PRs merge without asking"
            } else {
                ""
            }
        ),
    ));
    if imported_projects.iter().any(|p| p.mode == Mode::LocalOnly) {
        warnings.push("a local-only project gets gated PRs in Quark".to_string());
    }

    // Dispatch rules.
    let dispatch = match &home.crew_dispatch {
        Some(json) => match dispatch_draft(json, resolve, &mut warnings) {
            Ok(d) => {
                items.push(item(
                    "Dispatch rules",
                    &["config/crew-dispatch.json"],
                    d.rules.len(),
                    ImportOutcome::Imported,
                    format!(
                        "{} dispatch rule{} and {} default harness{}",
                        d.rules.len(),
                        plural(d.rules.len()),
                        d.default.len(),
                        plural(d.default.len())
                    ),
                ));
                Some(d)
            }
            Err(e) => {
                warnings.push(format!("firstmate's dispatch rules are not imported: {e}"));
                items.push(item(
                    "Dispatch rules",
                    &["config/crew-dispatch.json"],
                    1,
                    ImportOutcome::Skipped,
                    format!("not readable: {e}"),
                ));
                None
            }
        },
        None => None,
    };

    // The coordinator's agent config.
    let named = input
        .harness
        .clone()
        .or_else(|| home.crew_harness.clone())
        .or_else(|| {
            dispatch
                .as_ref()
                .and_then(|d| d.default.first())
                .map(|p| p.harness.clone())
        });
    let harness = match named.as_deref() {
        Some(h) => resolve(h).unwrap_or_else(|| {
            warnings.push(format!(
                "Quark has no harness named {h:?}, so the coordinator runs on {FALLBACK_HARNESS}"
            ));
            FALLBACK_HARNESS.to_string()
        }),
        None => FALLBACK_HARNESS.to_string(),
    };
    let from_default = dispatch
        .as_ref()
        .and_then(|d| d.default.first())
        .filter(|p| p.harness == harness);
    let agent = AgentConfig {
        harness: harness.clone(),
        model: from_default.and_then(|p| p.model.clone()),
        effort: from_default
            .and_then(|p| p.effort.clone())
            .filter(|e| EFFORTS.contains(&e.as_str())),
        pool: None,
    };
    if let Some(h) = &home.crew_harness {
        items.push(item(
            "Worker harness",
            &["config/crew-harness"],
            1,
            ImportOutcome::Imported,
            format!("{h}, which also runs the coordinator as {harness}"),
        ));
    }

    // The backlog.
    let issues = issues(home, &mut warnings);
    let (open, active, closed) = issues
        .iter()
        .fold((0, 0, 0), |(o, a, c), i| match i.status {
            SeedStatus::Open => (o + 1, a, c),
            SeedStatus::InProgress => (o, a + 1, c),
            SeedStatus::Closed { .. } => (o, a, c + 1),
        });
    items.push(item(
        "Backlog",
        &["data/backlog.md", "data/done-archive.md"],
        home.backlog.len(),
        ImportOutcome::Imported,
        format!(
            "{} issue{} labelled firstmate: {open} open, {active} started, {closed} closed",
            issues.len(),
            plural(issues.len())
        ),
    ));
    let holds = issues.iter().filter(|i| i.hold.is_some()).count();
    if holds > 0 {
        items.push(item(
            "Calls waiting on you",
            &["data/backlog.md"],
            holds,
            ImportOutcome::Imported,
            "each one's issue waits on you, with firstmate's reason".into(),
        ));
    }
    if active > 0 {
        warnings.push(format!(
            "{active} task{} {} being worked on in firstmate; {} started in Quark with no worker, \
             so start a worker on {} or close {}",
            plural(active),
            if active == 1 { "was" } else { "were" },
            if active == 1 { "it is" } else { "they are" },
            if active == 1 { "it" } else { "them" },
            if active == 1 { "it" } else { "them" },
        ));
    }

    // Files kept in the library.
    let mut library = Vec::new();
    for rel in &home.registry_files {
        if let Some(body) = read::text(&home.root.join(rel)) {
            let name = rel.trim_start_matches("data/");
            library.push((format!("{LIBRARY}/{name}"), body));
        }
    }
    let mut task_file_count = 0;
    for (id, files) in &home.task_files {
        for f in files {
            let name = f
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            match read::text(f) {
                Some(body) => {
                    library.push((format!("{LIBRARY}/tasks/{}/{name}", slug(id)), body));
                    task_file_count += 1;
                }
                None => warnings.push(format!(
                    "{id}'s {name} is not copied: it is not text, or is over 1 MB"
                )),
            }
        }
    }
    if task_file_count > 0 {
        let briefs = issues.iter().filter(|i| i.design.is_some()).count();
        items.push(item(
            "Briefs, reports and notes",
            &["data/<task>/"],
            task_file_count,
            ImportOutcome::Imported,
            format!(
                "the project's library; {briefs} brief{} also on {} issue{}",
                plural(briefs),
                if briefs == 1 { "its" } else { "their" },
                plural(briefs)
            ),
        ));
    }
    let mut logs = 0;
    for (id, path) in &home.status_logs {
        if let Some(body) = read::text(path) {
            library.push((format!("{LIBRARY}/tasks/{}/status.log", slug(id)), body));
            logs += 1;
        }
    }
    if logs > 0 {
        items.push(item(
            "Status history",
            &["state/<task>.status"],
            logs,
            ImportOutcome::Imported,
            "the project's library, next to each task's brief".into(),
        ));
    }

    // Memory.
    let user_memory = dedupe(&home.captain);
    if !user_memory.is_empty() {
        items.push(item(
            "Your preferences",
            &["data/captain.md", "data/captain-shared.md"],
            user_memory.len(),
            ImportOutcome::Imported,
            "memory every project's coordinator reads".into(),
        ));
    }
    let memory = dedupe(&home.learnings);
    if !memory.is_empty() {
        items.push(item(
            "What firstmate learned",
            &["data/learnings.md"],
            memory.len(),
            ImportOutcome::Imported,
            "this project's memory".into(),
        ));
    }
    if !home.archived_memory.is_empty() {
        items.push(item(
            "Retired memory",
            &["data/memory-archive.md"],
            home.archived_memory.len(),
            ImportOutcome::Imported,
            "the project's library only, as firstmate had retired them".into(),
        ));
    }

    // Accounts and quota.
    items.push(item(
        "Accounts",
        &[],
        0,
        ImportOutcome::Skipped,
        "firstmate keeps none; each harness's own sign-in is its default account in Quark".into(),
    ));
    if home.bob_quota.is_some() {
        items.push(item(
            "Bob quota settings",
            &["config/bob-quota.json"],
            1,
            ImportOutcome::Imported,
            "Quark's Bob quota settings, unless it has its own already".into(),
        ));
    }

    // Skipped.
    if !home.secondmates.is_empty() {
        items.push(item(
            "Secondmates",
            &["data/secondmates.md"],
            home.secondmates.len(),
            ImportOutcome::Skipped,
            "Quark runs workers on hosts instead: add each machine under Hosts. \
             The list is kept in the project's library"
                .into(),
        ));
    }
    if let Some(h) = &home.secondmate_harness {
        items.push(item(
            "Secondmate harness",
            &["config/secondmate-harness"],
            1,
            ImportOutcome::Skipped,
            format!("{h}; Quark has no secondmates"),
        ));
    }
    if !home.task_meta.is_empty() {
        items.push(item(
            "Running workers",
            &["state/<task>.meta"],
            home.task_meta.len(),
            ImportOutcome::Skipped,
            "firstmate's own sessions and working copies; Quark starts its own workers".into(),
        ));
    }
    if home.terminal_outcomes > 0 {
        items.push(item(
            "Outcome notices",
            &["state/terminal-outcomes/"],
            home.terminal_outcomes,
            ImportOutcome::Skipped,
            "the backlog already records each outcome".into(),
        ));
    }
    if !home.other_config.is_empty() {
        items.push(item(
            "Other settings",
            &["config/"],
            home.other_config.len(),
            ImportOutcome::Skipped,
            format!(
                "firstmate's own settings, with nothing like them in Quark: {}",
                home.other_config.join(", ")
            ),
        ));
    }
    if home.extensions > 0 {
        items.push(item(
            "Extensions",
            &["config/extensions.d/"],
            home.extensions,
            ImportOutcome::Skipped,
            "firstmate extensions do not run in Quark".into(),
        ));
    }
    if home.env_file {
        items.push(item(
            "Secrets",
            &[".env"],
            1,
            ImportOutcome::Skipped,
            "never copied; sign in to Relay and mail again in Quark".into(),
        ));
    }
    if home.away {
        items.push(item(
            "Away mode",
            &["state/.afk"],
            1,
            ImportOutcome::Skipped,
            "set away rules under Settings, Automation".into(),
        ));
    }

    let name = input
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| default_name(&imported_projects));
    let goal = imported_projects
        .iter()
        .filter(|p| !p.description.is_empty())
        .map(|p| format!("{}: {}", p.name, p.description))
        .collect::<Vec<_>>()
        .join("\n");
    let instructions = instructions(home, &imported_projects, delivery, standing_approval, today);
    let project = CreateProject {
        name,
        goal: (!goal.is_empty()).then_some(goal),
        workspace_path: None,
        repos,
        agent_config: Some(agent),
        dispatch_preset: None,
        delivery: Some(delivery),
    };
    let issue_count = issues.len();
    Ok(Seed {
        plan: FirstmateImportPlan {
            path: input.path.clone(),
            project,
            standing_approval,
            issues: issue_count,
            items,
            warnings,
        },
        dispatch,
        instructions,
        memory,
        user_memory,
        library,
        issues,
        bob_quota: home.bob_quota.clone(),
    })
}

fn item(
    what: &str,
    files: &[&str],
    count: usize,
    outcome: ImportOutcome,
    note: String,
) -> ImportItem {
    ImportItem {
        what: what.to_string(),
        files: files.iter().map(|f| f.to_string()).collect(),
        count,
        outcome,
        note,
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn dedupe(entries: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    entries
        .iter()
        .filter(|e| seen.insert(e.to_string()))
        .cloned()
        .collect()
}

/// A path segment for a task id.
fn slug(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_start_matches('.')
        .to_string()
}

fn default_name(projects: &[&read::FmProject]) -> String {
    match projects {
        [one] => one.name.clone(),
        [a, b] => format!("{} and {}", a.name, b.name),
        _ => "Firstmate fleet".to_string(),
    }
}

fn instructions(
    home: &Home,
    projects: &[&read::FmProject],
    delivery: DeliveryPolicy,
    standing_approval: bool,
    today: &str,
) -> String {
    let mut s = String::from("\n## From firstmate\n\n");
    s.push_str(&format!(
        "Imported on {today} from the firstmate home at `{}`.\n\n",
        home.root.display()
    ));
    s.push_str(&format!(
        "Ship mode of each repo in firstmate (Quark delivers the Project as {}{}):\n\n",
        match delivery {
            DeliveryPolicy::Direct => "direct pull requests",
            DeliveryPolicy::Gated => "gated pull requests",
        },
        if standing_approval {
            ", with standing approval to merge"
        } else {
            ""
        }
    ));
    for p in projects {
        let what = match p.mode {
            Mode::NoMistakes => "gated: every change goes through the no-mistakes pipeline",
            Mode::NoMistakesProdOnly => {
                "gated for production-bound changes; other work may go direct"
            }
            Mode::DirectPr => "direct pull requests, no pipeline",
            Mode::LocalOnly => "local only: land work locally, never push or open a PR",
            Mode::Other(_) => "gated (firstmate read this mode as no-mistakes)",
        };
        s.push_str(&format!(
            "- `{}` ({}{}): {what}.\n",
            p.name,
            p.mode.as_str(),
            if p.yolo { " +yolo" } else { "" }
        ));
    }
    s.push_str(&format!(
        "\nFirstmate's backlog tasks are beads labelled `firstmate`; its registry files, \
         briefs, reports and status logs are in `{LIBRARY}/`.\n"
    ));
    s
}

fn issues(home: &Home, warnings: &mut Vec<String>) -> Vec<SeedIssue> {
    let mut seen = HashSet::new();
    let rows: Vec<&BacklogRow> = home
        .backlog
        .iter()
        .filter(|r| seen.insert(r.id.clone()))
        .collect();
    let ids: BTreeSet<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    rows.into_iter()
        .map(|r| {
            let brief = home
                .task_files
                .get(&r.id)
                .and_then(|files| files.iter().find(|f| f.ends_with("brief.md")))
                .and_then(|f| read::text(f));
            let blocked_by: Vec<String> = r
                .blocked_by
                .iter()
                .filter(|b| {
                    let known = ids.contains(b.as_str());
                    if !known {
                        warnings.push(format!(
                            "task {} waits on {b}, which is not in the backlog",
                            r.id
                        ));
                    }
                    known
                })
                .cloned()
                .collect();
            SeedIssue {
                key: r.id.clone(),
                title: short_title(&r.title),
                description: description(home, r),
                design: brief,
                labels: labels(r),
                priority: r.priority.unwrap_or(2).min(4),
                status: status(r),
                hold: r
                    .hold
                    .clone()
                    .filter(|_| !matches!(status(r), SeedStatus::Closed { .. })),
                blocked_by,
                pull_request: r.pr.clone(),
            }
        })
        .collect()
}

/// Longest bead title; firstmate rows often carry a paragraph.
const TITLE_CHARS: usize = 120;

/// The row's title, cut to its first sentence or at a word when it is long.
fn short_title(title: &str) -> String {
    if title.chars().count() <= TITLE_CHARS {
        return title.to_string();
    }
    let head: String = title.chars().take(TITLE_CHARS).collect();
    if let Some(end) = head.find(". ").or_else(|| head.find(": ")) {
        if end >= 20 {
            return head[..end].to_string();
        }
    }
    let cut = head.rfind(' ').filter(|&i| i >= 20).unwrap_or(head.len());
    format!("{}…", head[..cut].trim_end_matches([',', ';', ':', ' ']))
}

fn status(r: &BacklogRow) -> SeedStatus {
    let done = r.checked || matches!(r.section, Some(Section::Done | Section::Archived));
    if done {
        let mut reason = String::from("Done in firstmate");
        if let Some(d) = r.date("merged") {
            reason = format!("Merged {d} in firstmate");
        } else if let Some(d) = r.date("done").or_else(|| r.date("reported")) {
            reason = format!("Done {d} in firstmate");
        }
        if let Some(pr) = &r.pr {
            reason.push_str(&format!(": {pr}"));
        }
        return SeedStatus::Closed { reason };
    }
    match r.section {
        Some(Section::InFlight) => SeedStatus::InProgress,
        _ => SeedStatus::Open,
    }
}

fn labels(r: &BacklogRow) -> Vec<String> {
    let mut labels = vec!["firstmate".to_string()];
    let clean = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                    c
                } else {
                    '-'
                }
            })
            .collect()
    };
    if let Some(repo) = &r.repo {
        labels.push(format!("repo:{}", clean(repo)));
    }
    if let Some(kind) = &r.kind {
        labels.push(format!("kind:{}", clean(kind)));
    }
    if r.hold.is_some() {
        labels.push("captain-hold".into());
    }
    labels
}

fn description(home: &Home, r: &BacklogRow) -> String {
    let mut s = String::new();
    if short_title(&r.title) != r.title {
        s.push_str(&r.title);
        s.push_str("\n\n");
    }
    let body = r.body.join("\n");
    if !body.trim().is_empty() {
        s.push_str(body.trim());
        s.push_str("\n\n");
    }
    s.push_str(&format!("Imported from firstmate task `{}`.", r.id));
    if let Some(repo) = &r.repo {
        s.push_str(&format!("\nRepo: {repo}"));
    }
    if let Some(pr) = &r.pr {
        s.push_str(&format!("\nPull request: {pr}"));
    }
    for link in &r.links {
        s.push_str(&format!("\nLink: {link}"));
    }
    if let Some(report) = &r.report {
        let id = report
            .trim_start_matches("data/")
            .trim_end_matches("/report.md");
        s.push_str(&format!(
            "\nReport: `{LIBRARY}/tasks/{}/report.md`",
            slug(id)
        ));
    }
    if let Some(hold) = &r.hold {
        s.push_str(&format!(
            "\nCaptain hold ({}): {hold}",
            r.hold_kind.as_deref().unwrap_or("captain")
        ));
        if let Some(until) = &r.hold_until {
            s.push_str(&format!(", until {until}"));
        }
    }
    for (word, date) in &r.dates {
        s.push_str(&format!(
            "\n{}{} {date}",
            word[..1].to_uppercase(),
            &word[1..]
        ));
    }
    if home.task_files.contains_key(&r.id) {
        s.push_str(&format!("\nFiles: `{LIBRARY}/tasks/{}/`", slug(&r.id)));
    }
    s.chars().take(60 * 1024).collect()
}

/// `config/crew-dispatch.json`, firstmate's dispatch profiles.
#[derive(Debug, Deserialize)]
struct CrewDispatch {
    #[serde(default)]
    rules: Vec<CrewRule>,
    #[serde(default)]
    default: Option<OneOrMany>,
    #[serde(default)]
    default_select: Option<DispatchSelect>,
}

#[derive(Debug, Deserialize)]
struct CrewRule {
    when: String,
    #[serde(rename = "use")]
    profiles: OneOrMany,
    #[serde(default)]
    select: Option<DispatchSelect>,
    #[serde(default)]
    why: Option<String>,
    #[serde(default)]
    approval: Option<String>,
    #[serde(default)]
    floor: Option<DispatchFloor>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(CrewProfile),
    Many(Vec<CrewProfile>),
}

#[derive(Debug, Clone, Deserialize)]
struct CrewProfile {
    harness: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    floor: Option<DispatchFloor>,
    #[serde(default)]
    pricing: Option<String>,
}

impl OneOrMany {
    fn into_vec(self) -> Vec<CrewProfile> {
        match self {
            OneOrMany::One(p) => vec![p],
            OneOrMany::Many(ps) => ps,
        }
    }
}

/// The rules of `config/crew-dispatch.json` as a `dispatch.yaml` draft:
/// harness names become Quark ids, and a profile on a harness Quark does not
/// know is dropped with a warning.
fn dispatch_draft(
    json: &str,
    resolve: &dyn Fn(&str) -> Option<String>,
    warnings: &mut Vec<String>,
) -> Result<DispatchRulesDraft, String> {
    let config: CrewDispatch = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let mut profiles = |ps: Vec<CrewProfile>, at: &str| -> Vec<DispatchProfile> {
        ps.into_iter()
            .filter_map(|p| match resolve(&p.harness) {
                Some(harness) => Some(DispatchProfile {
                    harness,
                    model: p.model,
                    effort: p.effort,
                    pool: None,
                    provider: p.provider,
                    floor: p.floor,
                    pricing: p.pricing,
                }),
                None => {
                    warnings.push(format!(
                        "dispatch {at}: Quark has no harness named {:?}, so that choice is dropped",
                        p.harness
                    ));
                    None
                }
            })
            .collect()
    };
    let mut rules = Vec::new();
    for (n, r) in config.rules.into_iter().enumerate() {
        let candidates = profiles(r.profiles.into_vec(), &format!("rule {}", n + 1));
        if candidates.is_empty() {
            continue;
        }
        rules.push(DispatchRuleSpec {
            name: Some(format!("firstmate-{}", n + 1)),
            when: r.when,
            candidates,
            select: r.select,
            why: r.why,
            approval: r.approval,
            floor: r.floor,
        });
    }
    let default = config
        .default
        .map(|d| profiles(d.into_vec(), "default"))
        .unwrap_or_default();
    Ok(DispatchRulesDraft {
        default_select: config.default_select,
        rules,
        default,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::*;
    use crate::firstmate_import::read::FmProject;

    fn resolve(name: &str) -> Option<String> {
        match name {
            "claude" | "claude-code" => Some("claude-code".into()),
            "pi" => Some("pi".into()),
            _ => None,
        }
    }

    fn project(name: &str, mode: Mode, yolo: bool) -> FmProject {
        FmProject {
            name: name.into(),
            mode,
            yolo,
            description: format!("{name} does things"),
            added: None,
            clone: Some(PathBuf::from(format!("/fm/projects/{name}"))),
            origin: Some(format!("git@github.com:o/{name}.git")),
        }
    }

    fn home() -> Home {
        let backlog = read::parse_backlog(
            "## In flight\n\
             - [ ] live - Live work (repo: hawkeye) (kind: ship) (since 2026-08-01)\n\
             ## Queued\n\
             - [ ] next - Next work blocked-by: live blocked-by: gone (repo: hawkeye) (kind: ship) (priority: 1)\n\
             - [ ] call - A call (repo: sushibot) (kind: captain) (hold: pick a host) (hold-kind: captain)\n\
             ## Done\n\
             - [x] shipped - Shipped https://github.com/o/sushibot/pull/2 (repo: sushibot) (kind: ship) (merged 2026-08-25)\n\
             \x20 PR #2 merged by captain.\n",
            None,
        );
        Home {
            root: PathBuf::from("/fm"),
            projects: vec![
                project("hawkeye", Mode::DirectPr, false),
                project("sushibot", Mode::NoMistakesProdOnly, false),
            ],
            backlog,
            task_files: BTreeMap::new(),
            captain: vec!["Prefer small PRs".into(), "Prefer small PRs".into()],
            learnings: vec!["The sandbox image builds locally".into()],
            crew_dispatch: Some(
                r#"{"rules":[{"when":"Trivial edits","use":[{"harness":"pi","model":"stealth/ox-alpha","effort":"low"}],"why":"cheap"},
                             {"when":"Odd","use":{"harness":"nope"}}],
                    "default":[{"harness":"pi","model":"stealth/ox-alpha","effort":"medium"}]}"#
                    .into(),
            ),
            crew_harness: Some("pi".into()),
            other_config: vec!["startup-memory-budget".into()],
            terminal_outcomes: 2,
            ..Home::default()
        }
    }

    fn input() -> ImportFirstmate {
        ImportFirstmate {
            path: "/fm".into(),
            ..ImportFirstmate::default()
        }
    }

    #[test]
    fn plans_the_project() {
        let seed = plan(&home(), &input(), &resolve, "2026-10-21").unwrap();
        let p = &seed.plan.project;
        assert_eq!(p.name, "hawkeye and sushibot");
        assert_eq!(p.repos.len(), 2);
        assert_eq!(p.repos[0].url, "git@github.com:o/hawkeye.git");
        assert_eq!(p.repos[0].name.as_deref(), Some("hawkeye"));
        assert_eq!(p.delivery, Some(DeliveryPolicy::Gated));
        let agent = p.agent_config.as_ref().unwrap();
        assert_eq!(agent.harness, "pi");
        assert_eq!(agent.model.as_deref(), Some("stealth/ox-alpha"));
        assert_eq!(agent.effort.as_deref(), Some("medium"));
        assert!(!seed.plan.standing_approval);
        assert!(seed.instructions.contains("`hawkeye` (direct-PR)"));
        assert!(seed
            .instructions
            .contains("`sushibot` (no-mistakes-prod-only)"));
        assert_eq!(seed.user_memory, vec!["Prefer small PRs"]);
        assert_eq!(seed.memory.len(), 1);
    }

    #[test]
    fn every_project_direct_and_yolo_is_direct_with_standing_approval() {
        let mut h = home();
        h.projects = vec![
            project("a", Mode::DirectPr, true),
            project("b", Mode::DirectPr, true),
        ];
        let seed = plan(&h, &input(), &resolve, "2026-10-21").unwrap();
        assert_eq!(seed.plan.project.delivery, Some(DeliveryPolicy::Direct));
        assert!(seed.plan.standing_approval);
    }

    #[test]
    fn dispatch_rules_use_quark_ids_and_drop_unknown_harnesses() {
        let seed = plan(&home(), &input(), &resolve, "2026-10-21").unwrap();
        let d = seed.dispatch.unwrap();
        assert_eq!(d.rules.len(), 1);
        assert_eq!(d.rules[0].name.as_deref(), Some("firstmate-1"));
        assert_eq!(d.rules[0].candidates[0].harness, "pi");
        assert_eq!(d.rules[0].why.as_deref(), Some("cheap"));
        assert_eq!(d.default[0].effort.as_deref(), Some("medium"));
        assert!(seed.plan.warnings.iter().any(|w| w.contains("\"nope\"")));
    }

    #[test]
    fn backlog_rows_become_beads() {
        let seed = plan(&home(), &input(), &resolve, "2026-10-21").unwrap();
        let by = |k: &str| seed.issues.iter().find(|i| i.key == k).unwrap();
        assert_eq!(by("live").status, SeedStatus::InProgress);
        assert_eq!(by("next").status, SeedStatus::Open);
        assert_eq!(by("next").priority, 1);
        assert_eq!(by("next").blocked_by, vec!["live"]);
        assert_eq!(by("call").hold.as_deref(), Some("pick a host"));
        assert!(by("call").labels.contains(&"captain-hold".to_string()));
        let shipped = by("shipped");
        assert_eq!(
            shipped.status,
            SeedStatus::Closed {
                reason: "Merged 2026-08-25 in firstmate: https://github.com/o/sushibot/pull/2"
                    .into()
            }
        );
        assert!(shipped.description.starts_with("PR #2 merged by captain."));
        assert_eq!(
            shipped.labels,
            vec!["firstmate", "repo:sushibot", "kind:ship"]
        );
        assert!(seed.plan.warnings.iter().any(|w| w.contains("gone")));
        assert!(seed
            .plan
            .warnings
            .iter()
            .any(|w| w.contains("being worked on")));
    }

    #[test]
    fn long_titles_keep_their_first_sentence() {
        let long = format!("Build the core: desktop app. {}", "More words ".repeat(20));
        assert_eq!(short_title(&long), "Build the core: desktop app");
        let run_on = "word ".repeat(40);
        let t = short_title(run_on.trim());
        assert!(t.ends_with("word…") && t.chars().count() <= TITLE_CHARS + 1);
        assert_eq!(short_title("Short"), "Short");
    }

    #[test]
    fn a_home_without_clones_is_refused() {
        let mut h = home();
        for p in &mut h.projects {
            p.clone = None;
            p.origin = None;
        }
        assert!(plan(&h, &input(), &resolve, "2026-10-21").is_err());
    }

    #[test]
    fn unknown_crew_harness_falls_back() {
        let mut h = home();
        h.crew_harness = Some("mystery".into());
        let seed = plan(&h, &input(), &resolve, "2026-10-21").unwrap();
        let agent = seed.plan.project.agent_config.unwrap();
        assert_eq!(agent.harness, FALLBACK_HARNESS);
        assert_eq!(agent.model, None);
    }
}
