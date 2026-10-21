//! Seeding Quark from a firstmate home: `POST /v1/imports/firstmate`.
//!
//! The home is read by [`crate::firstmate::read`], which only opens files.
//! What is written goes to Quark's own home: Projects (through the same path
//! as `POST /v1/projects`), each Project's `dispatch.yaml` and Beads
//! database, `quota/` files and user-level memory.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::Json;
use quark_systems::{
    AgentConfig, AgentRole, BeadsState, CreateProject, ErrorBody, FirstmateImportReport,
    ImportCount, ImportFirstmate, ImportInventoryItem, ImportOutcome, ImportedProject, Project,
    ProjectStatus, RepoSource,
};

use super::{db, ApiError, AppState};
use crate::beads::firstmate::ImportBead;
use crate::crew_dispatch;
use crate::firstmate::{self, Plan, Task};
use crate::project_repo::{self, RepoError};

const DEFAULT_WAIT: u64 = 120;
/// The coordinator's harness when firstmate's crew harness can't coordinate:
/// the one the native engine starts coordinators on.
const FALLBACK_HARNESS: &str = crate::engine::native_project::COORDINATOR_HARNESS;
const DISPATCH_COMMIT: &str = "Import dispatch rules from firstmate";

/// Import a firstmate home into Quark.
///
/// Reads the firstmate home at `home` (only reads it: no script of it runs and
/// nothing in it changes) and seeds Quark: one Project per firstmate project,
/// with its repo and delivery posture; the home's `config/crew-dispatch.json`
/// as each Project's `dispatch.yaml`; the backlog as Beads, with held items
/// as `decision` beads and each item's status log, report and brief as
/// comments; Kiro's overage setting and Bob's allowance under `quota/`; and
/// `captain.md` and `learnings.md` as user-level memory.
/// The response lists every category firstmate stores and what became of
/// it. Importing again leaves what an earlier import made alone, and creates
/// what it missed. A Project still provisioning after `wait_secs` is
/// reported `pending`; import again once it is ready. With `dry_run`
/// nothing is written.
#[utoipa::path(
    post,
    path = "/v1/imports/firstmate",
    tag = "imports",
    request_body = ImportFirstmate,
    responses(
        (status = 200, body = FirstmateImportReport),
        (status = 400, description = "Not a firstmate home, or a name that is not one of its projects", body = ErrorBody)
    )
)]
pub async fn import(
    State(state): State<AppState>,
    Json(req): Json<ImportFirstmate>,
) -> Result<Json<FirstmateImportReport>, ApiError> {
    let read_req = req.clone();
    let plan = tokio::task::spawn_blocking(move || firstmate::read(&read_req))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .map_err(ApiError::invalid)?;
    run(&state, &req, plan).await.map(Json)
}

struct Written {
    memory: Option<(usize, usize)>,
    kiro: ImportOutcome,
    bob: ImportOutcome,
}

async fn run(
    state: &AppState,
    req: &ImportFirstmate,
    plan: Plan,
) -> Result<FirstmateImportReport, ApiError> {
    let mut notes = plan.notes.clone();
    let mut unrouted = Vec::new();
    let mut by_project: std::collections::BTreeMap<&str, Vec<&Task>> = Default::default();
    for t in &plan.tasks {
        match &t.project {
            Some(p) => by_project.entry(p.as_str()).or_default().push(t),
            None => unrouted.push(format!(
                "{}: {}",
                t.item.id,
                t.unrouted.as_deref().unwrap_or("no project")
            )),
        }
    }
    let wait = Duration::from_secs(req.wait_secs.unwrap_or(DEFAULT_WAIT));
    let agent = coordinator(state, &plan, &mut notes);
    let mut projects = Vec::new();
    for p in &plan.projects {
        let tasks = by_project.remove(p.name.as_str()).unwrap_or_default();
        projects.push(project(state, req, &plan, p, &tasks, &agent, wait).await);
    }
    if let Some(e) = &plan.dispatch_error {
        notes.push(format!("config/crew-dispatch.json was not imported: {e}"));
    }
    if let Some(d) = &plan.dispatch {
        notes.extend(d.notes.iter().cloned());
    }
    if plan.projects.iter().any(|p| p.repo_url.is_none()) {
        notes.push("a project without an origin URL was recorded without a repo, so it has no Project repo to keep dispatch rules in".into());
    }
    if plan.projects.iter().any(|p| p.has_beads) {
        notes.push("a project clone's own .beads is not read: Quark adopts the database its first repo has when it sets up the Project's Beads".into());
    }
    let written = write_local(state, req, &plan, &mut notes);
    Ok(FirstmateImportReport {
        home: req.home.clone(),
        dry_run: req.dry_run,
        inventory: inventory(&plan, &projects, &written, req, &agent),
        projects,
        unrouted,
        notes,
    })
}

/// The coordinator for each new Project: firstmate's crew harness
/// (`config/crew-harness`) when Quark can run a coordinator on it, otherwise
/// claude-code with a note saying why, at high effort.
fn coordinator(state: &AppState, plan: &Plan, notes: &mut Vec<String>) -> AgentConfig {
    let agent = |harness: &str| AgentConfig {
        harness: harness.into(),
        model: None,
        effort: Some("high".into()),
        pool: None,
    };
    let Some(name) = plan.crew_harness.as_deref() else {
        return agent(FALLBACK_HARNESS);
    };
    let Some(id) = firstmate::dispatch::quark_harness(name) else {
        notes.push(format!(
            "config/crew-harness names `{name}`, which is not a Quark harness, so coordinators run on {FALLBACK_HARNESS}"
        ));
        return agent(FALLBACK_HARNESS);
    };
    let config = agent(&id);
    let check = state.harnesses.validate(&config, AgentRole::Coordinator);
    let why: Vec<_> = if !check.valid {
        check.errors.into_iter().map(|e| e.message).collect()
    } else if id != FALLBACK_HARNESS {
        vec![format!(
            "the native engine starts coordinators on {FALLBACK_HARNESS} only so far"
        )]
    } else {
        return config;
    };
    notes.push(format!(
        "{id} (config/crew-harness) cannot run a coordinator ({}), so coordinators run on {FALLBACK_HARNESS}; dispatch rules still send workers to it",
        why.join("; ")
    ));
    agent(FALLBACK_HARNESS)
}

/// Quark-home files that need no Project: quota settings and user memory.
fn write_local(
    state: &AppState,
    req: &ImportFirstmate,
    plan: &Plan,
    notes: &mut Vec<String>,
) -> Written {
    let would = ImportOutcome::Imported;
    let quota_dir = state.layout.home.join("quota");
    let mut kiro = ImportOutcome::NotImported;
    if let Some(allowed) = plan.quota.kiro_overage_allowed {
        let file = quota_dir.join("kiro.json");
        kiro = if file.exists() {
            ImportOutcome::Present
        } else if req.dry_run {
            would
        } else {
            match write_file(&file, &format!("{{ \"overage_allowed\": {allowed} }}\n")) {
                Ok(()) => would,
                Err(e) => {
                    notes.push(e);
                    ImportOutcome::NotImported
                }
            }
        };
    }
    let mut bob = ImportOutcome::NotImported;
    if let Some(budget) = plan.quota.bob_budget_coins {
        // 1500 is Quark's own default, so there is nothing to write.
        if (budget - 1500.0).abs() > f64::EPSILON && budget > 0.0 {
            let file = quota_dir.join("bob.json");
            bob = if file.exists() {
                ImportOutcome::Present
            } else if req.dry_run {
                would
            } else {
                match write_file(&file, &format!("{{ \"monthly_coins\": {budget} }}\n")) {
                    Ok(()) => would,
                    Err(e) => {
                        notes.push(e);
                        ImportOutcome::NotImported
                    }
                }
            };
        }
        notes.push(format!(
            "Bob's usage anchor{} was not imported: it is tied to the month it was taken in and goes stale within a day, so Bob reads as partial until you run `quarkd quota bob --anchor <coins>` with the figure /status shows",
            plan.quota
                .bob_anchor_at
                .as_deref()
                .map(|a| format!(" (taken {a})"))
                .unwrap_or_default()
        ));
    }
    let entries: Vec<_> = plan
        .captain
        .iter()
        .chain(plan.learnings.iter())
        .cloned()
        .collect();
    let memory = if entries.is_empty() {
        None
    } else if req.dry_run {
        let have: std::collections::HashSet<String> =
            crate::memory::list_user(&state.layout.user_memory())
                .unwrap_or_default()
                .into_iter()
                .filter_map(|u| u.entry_id)
                .collect();
        let present = entries.iter().filter(|e| have.contains(&e.key)).count();
        Some((entries.len() - present, present))
    } else {
        let today = crate::now_rfc3339();
        match firstmate::write_user_memory(
            &state.layout.user_memory(),
            &entries,
            &today[..10],
            &today,
        ) {
            Ok(r) => Some(r),
            Err(e) => {
                notes.push(format!("user-level memory: {e}"));
                None
            }
        }
    };
    Written { memory, kiro, bob }
}

fn write_file(path: &Path, body: &str) -> Result<(), String> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    std::fs::write(path, body).map_err(|e| format!("writing {}: {e}", path.display()))
}

async fn project(
    state: &AppState,
    req: &ImportFirstmate,
    plan: &Plan,
    p: &firstmate::Project,
    tasks: &[&Task],
    agent: &AgentConfig,
    wait: Duration,
) -> ImportedProject {
    let mut out = ImportedProject {
        name: p.name.clone(),
        project_id: None,
        project: "existing".into(),
        repo_url: p.repo_url.clone(),
        outcome: ImportOutcome::Imported,
        detail: None,
        dispatch: ImportOutcome::NotImported,
        tasks: ImportCount::default(),
        holds: ImportCount::default(),
    };
    let beads: Vec<ImportBead> = tasks.iter().map(|t| firstmate::bead_for(t)).collect();
    let (holds, work): (Vec<_>, Vec<_>) = beads.iter().partition(|b| b.issue_type == "decision");
    let existing = match db(state, |s| s.list_all_projects()).await {
        Ok(all) => all.into_iter().find(|x| x.name == p.name),
        Err(e) => return fail(out, e.message()),
    };
    let created_now = existing.is_none();
    let project = match existing {
        Some(x) => x,
        None if req.dry_run => {
            out.project = "created".into();
            out.dispatch = if plan.dispatch.is_some() && p.repo_url.is_some() {
                ImportOutcome::Imported
            } else {
                ImportOutcome::NotImported
            };
            out.tasks.imported = work.len();
            out.holds.imported = holds.len();
            return out;
        }
        None => {
            out.project = "created".into();
            let input = CreateProject {
                name: p.name.clone(),
                goal: Some(
                    p.description
                        .clone()
                        .unwrap_or_else(|| "Imported from firstmate".into()),
                ),
                repos: p
                    .repo_url
                    .iter()
                    .map(|u| RepoSource {
                        url: u.clone(),
                        name: None,
                    })
                    .collect(),
                agent_config: Some(agent.clone()),
                delivery: Some(p.delivery),
                ..Default::default()
            };
            match super::routes::create_and_provision(state, input).await {
                Ok(x) => x,
                Err(e) => return fail(out, e.message()),
            }
        }
    };
    out.project_id = Some(project.id.clone());
    if req.dry_run {
        out.dispatch = ImportOutcome::Imported;
        let (w, h) = count_present(state, &project.id, &work, &holds).await;
        out.tasks = ImportCount {
            imported: work.len() - w,
            present: w,
        };
        out.holds = ImportCount {
            imported: holds.len() - h,
            present: h,
        };
        return out;
    }
    let deadline = Instant::now() + wait;
    let project = match ready(state, &project.id, deadline).await {
        Ok(x) => x,
        Err(why) => {
            out.outcome = ImportOutcome::Pending;
            out.dispatch = ImportOutcome::Pending;
            out.detail = Some(format!("{why}; import again once the Project is ready"));
            return out;
        }
    };
    out.dispatch = match (
        &plan.dispatch,
        &project.project_repo_path,
        &project.workspace_path,
    ) {
        (Some(d), Some(bare), Some(root)) => {
            match write_dispatch(state, bare, root, &d.yaml, created_now).await {
                Ok(o) => o,
                Err(e) => {
                    out.detail = Some(format!("dispatch.yaml: {e}"));
                    ImportOutcome::NotImported
                }
            }
        }
        _ => ImportOutcome::NotImported,
    };
    if let Err(why) = beads_ready(state, &project.id, deadline).await {
        out.outcome = ImportOutcome::Pending;
        out.detail = Some(format!("{why}; import again once Beads is set up"));
        return out;
    }
    let count = |r: crate::beads::firstmate::Imported| ImportCount {
        imported: r.created,
        present: r.present,
    };
    match state
        .beads
        .import_firstmate(&state.layout.home, &project.id, &work_owned(&work))
        .await
    {
        Ok(r) => out.tasks = count(r),
        Err(e) => {
            out.outcome = ImportOutcome::Pending;
            out.detail = Some(format!("tasks: {e}"));
            return out;
        }
    }
    match state
        .beads
        .import_firstmate(&state.layout.home, &project.id, &work_owned(&holds))
        .await
    {
        Ok(r) => out.holds = count(r),
        Err(e) => {
            out.outcome = ImportOutcome::Pending;
            out.detail = Some(format!("holds: {e}"));
        }
    }
    out
}

fn work_owned(v: &[&ImportBead]) -> Vec<ImportBead> {
    v.iter().map(|b| (*b).clone()).collect()
}

fn fail(mut out: ImportedProject, why: &str) -> ImportedProject {
    out.outcome = ImportOutcome::NotImported;
    out.detail = Some(why.to_string());
    out
}

async fn count_present(
    state: &AppState,
    project_id: &str,
    work: &[&ImportBead],
    holds: &[&ImportBead],
) -> (usize, usize) {
    let Ok(have) = state
        .beads
        .imported_firstmate(&state.layout.home, project_id)
        .await
    else {
        return (0, 0);
    };
    let n = |v: &[&ImportBead]| v.iter().filter(|b| have.contains_key(&b.id)).count();
    (n(work), n(holds))
}

/// Waits for the Project to be provisioned.
async fn ready(state: &AppState, id: &str, deadline: Instant) -> Result<Project, String> {
    loop {
        let sid = id.to_string();
        let p = db(state, move |s| s.get_project(&sid))
            .await
            .map_err(|e| e.message().to_string())?;
        match p.status {
            ProjectStatus::Failed => {
                return Err(format!(
                    "provisioning failed: {}",
                    p.status_detail.unwrap_or_default()
                ))
            }
            ProjectStatus::Ready => return Ok(p),
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err("the Project was not ready in time".into());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Waits for the Project's Beads database to be set up.
async fn beads_ready(state: &AppState, id: &str, deadline: Instant) -> Result<(), String> {
    loop {
        let b = state.beads.status(&state.layout.home, id).await;
        match b.state {
            BeadsState::Ready => return Ok(()),
            BeadsState::Unavailable | BeadsState::Failed => {
                return Err(format!(
                    "Beads is not available: {}",
                    b.detail.unwrap_or_default()
                ))
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err("its Beads database was not set up in time".into());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

enum DispatchError {
    Repo(RepoError),
    Keep,
}

impl From<RepoError> for DispatchError {
    fn from(e: RepoError) -> Self {
        DispatchError::Repo(e)
    }
}

/// Commits `yaml` as the Project's `dispatch.yaml`. A Project this import
/// created, or whose file nobody changed since it was created, takes it. One
/// the user has edited since keeps their file.
async fn write_dispatch(
    state: &AppState,
    bare: &str,
    root: &str,
    yaml: &str,
    created_now: bool,
) -> Result<ImportOutcome, String> {
    let _one = super::memory::COMMITTING.lock().await;
    let (bare, checkout) = (PathBuf::from(bare), PathBuf::from(root).join("project"));
    let yaml = yaml.to_string();
    let user = crate::classifier::user_default_at(&state.layout.user_config())?;
    tokio::task::spawn_blocking(move || {
        let history = crate::gates::git(
            &bare,
            &["log", "--format=%s", "main", "--", crew_dispatch::FILE],
        )
        .unwrap_or_default();
        let untouched = history.lines().count() <= 1;
        let imported_before = history.lines().any(|l| l == DISPATCH_COMMIT);
        let take = created_now || untouched || imported_before;
        let committed = project_repo::commit_file(
            &bare,
            &checkout,
            crew_dispatch::FILE,
            |current| {
                if current.is_some_and(|(_, t)| t == yaml) {
                    return Ok(yaml.clone());
                }
                if !take {
                    return Err(DispatchError::Keep);
                }
                crew_dispatch::compile(&yaml, user.as_ref()).map_err(|_| DispatchError::Keep)?;
                Ok(yaml.clone())
            },
            DISPATCH_COMMIT,
        );
        match committed {
            Ok(Some(_)) => Ok(ImportOutcome::Imported),
            Ok(None) => Ok(ImportOutcome::Present),
            Err(DispatchError::Keep) => Ok(ImportOutcome::Present),
            Err(DispatchError::Repo(e)) => Err(e.to_string()),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

fn item(
    category: &str,
    source: &str,
    target: &str,
    outcome: ImportOutcome,
    count: usize,
    detail: &str,
) -> ImportInventoryItem {
    ImportInventoryItem {
        category: category.into(),
        source: source.into(),
        target: target.into(),
        outcome,
        count,
        detail: detail.into(),
    }
}

/// Every category firstmate stores locally, and where it went.
fn inventory(
    plan: &Plan,
    projects: &[ImportedProject],
    w: &Written,
    req: &ImportFirstmate,
    agent: &AgentConfig,
) -> Vec<ImportInventoryItem> {
    use ImportOutcome::*;
    let sum = |f: fn(&ImportedProject) -> &ImportCount| -> (usize, usize) {
        projects
            .iter()
            .map(|p| (f(p).imported, f(p).present))
            .fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
    };
    let (ti, tp) = sum(|p| &p.tasks);
    let (hi, hp) = sum(|p| &p.holds);
    let pending = projects.iter().any(|p| p.outcome == Pending);
    let state_of = |imported: usize, present: usize| {
        if pending && imported + present == 0 {
            Pending
        } else if imported == 0 && present > 0 {
            Present
        } else {
            Imported
        }
    };
    let logs = plan.tasks.iter().filter(|t| !t.status.is_empty()).count();
    let reports = plan
        .tasks
        .iter()
        .filter(|t| t.report.is_some() || t.brief.is_some())
        .count();
    let in_flight = plan
        .tasks
        .iter()
        .filter(|t| t.item.section == firstmate::backlog::Section::InFlight)
        .count();
    let mem = |n: usize| match w.memory {
        Some((wr, pr)) if wr + pr > 0 => state_of(wr, pr),
        _ if n == 0 => NotImported,
        _ => NotImported,
    };
    let mut v = vec![
        item(
            "projects",
            "data/projects.md, projects/*/.git/config",
            "a Quark Project per firstmate project: name, goal, origin repo, delivery (direct-PR is direct, others gated)",
            if projects.iter().all(|p| p.outcome == NotImported) && !projects.is_empty() {
                NotImported
            } else if pending {
                Pending
            } else {
                Imported
            },
            plan.projects.len(),
            "Quark's per-Project state (dispatch, Beads, workspace) needs a Project per firstmate project; change a coordinator in Settings",
        ),
        item(
            "coordinator",
            "config/crew-harness",
            &format!("each new Project's coordinator: {} at high effort", agent.harness),
            match &plan.crew_harness {
                Some(_) if projects.iter().all(|p| p.outcome == NotImported) && !projects.is_empty() => NotImported,
                Some(h) if firstmate::dispatch::quark_harness(h).as_deref() == Some(agent.harness.as_str()) => Imported,
                _ => NotImported,
            },
            usize::from(plan.crew_harness.is_some()),
            "firstmate's crew harness runs the coordinator when it can coordinate; otherwise claude-code does, and the notes say why. Existing Projects keep theirs",
        ),
        item(
            "dispatch",
            "config/crew-dispatch.json",
            "each Project's dispatch.yaml (Project repo, own commit)",
            match &plan.dispatch {
                Some(_) => projects
                    .iter()
                    .map(|p| p.dispatch)
                    .find(|o| *o != NotImported)
                    .unwrap_or(NotImported),
                None => NotImported,
            },
            plan.dispatch.as_ref().map_or(0, |d| d.rules),
            "harness ids mapped to Quark's; why, floor, provider, pricing and select kept; the classifier is not carried over; a dispatch.yaml you edited is kept",
        ),
        item(
            "dispatch",
            "config/crew-dispatch-*.json, config/crew-dispatch.json.bak-*",
            "",
            NotImported,
            plan.skipped.variant_dispatch,
            "alternative rule sets and backups firstmate swapped between; only the active crew-dispatch.json is the rule set",
        ),
        item(
            "accounts",
            "(none)",
            "",
            NotImported,
            0,
            "firstmate keeps no accounts of its own; harness logins live in each CLI's config directory. Add them under Settings > Accounts",
        ),
        item(
            "quota",
            "config/kiro-quota.json",
            "<quark home>/quota/kiro.json",
            w.kiro,
            usize::from(plan.quota.kiro_overage_allowed.is_some()),
            "Kiro's overage_allowed, which has the same meaning",
        ),
        item(
            "quota",
            "state/bob-quota-anchor.json",
            "<quark home>/quota/bob.json (monthly_coins, only when not 1500)",
            w.bob,
            usize::from(plan.quota.bob_budget_coins.is_some()),
            "the usage anchor itself is not carried over: it is bound to a charge month and expires in a day",
        ),
        item(
            "tasks",
            if req.include_archive {
                "data/backlog.md, data/done-archive.md"
            } else {
                "data/backlog.md"
            },
            "Beads in the item's Project (labels firstmate and firstmate:<id>; done items closed; blocked-by links kept)",
            state_of(ti, tp),
            ti + tp,
            "items with no routable Project are listed as unrouted and not imported",
        ),
        item(
            "holds",
            "held and captain-kind items in data/backlog.md",
            "decision beads (closed with the captain's answer when one is recorded)",
            state_of(hi, hp),
            hi + hp,
            "they are not Quark decisions: they were never asked in Quark, so closing the bead is how one is answered",
        ),
        item(
            "status",
            "state/<id>.status, state/<id>.meta",
            "a comment on the item's bead; harness, model and effort in its description",
            state_of(ti + hi, tp + hp),
            logs,
            "no events are written to the event log, and in-flight items become open beads, not running workers: their panes and worktrees cannot be adopted",
        ),
        item(
            "reports",
            "data/<id>/report.md, data/<id>/brief.md",
            "comments on the item's bead",
            state_of(ti + hi, tp + hp),
            reports,
            "",
        ),
        item(
            "memory",
            "data/captain.md",
            "user-level memory entries (one per bullet)",
            mem(plan.captain.len()),
            plan.captain.len(),
            "",
        ),
        item(
            "memory",
            "data/learnings.md",
            "user-level memory entries (one per section)",
            mem(plan.learnings.len()),
            plan.learnings.len(),
            "",
        ),
    ];
    let _ = in_flight;
    for (cat, src, n, why) in [
        (
            "runtime",
            "config/backend",
            usize::from(plan.skipped.backend.is_some()),
            "firstmate's terminal backend (herdr, tmux...); Quark runs workers in its own PTY supervisor",
        ),
        (
            "runtime",
            "config/startup-memory-budget and other config/* settings",
            0,
            "firstmate session tuning with no Quark equivalent",
        ),
        (
            "runtime",
            "data/secondmates.md",
            usize::from(plan.skipped.secondmates),
            "secondmate homes are firstmate's; Quark's sub-coordinators are set up per host",
        ),
        (
            "runtime",
            "state/ watcher, lease, inbox, busy-state and turn-end files",
            plan.skipped.watcher_files,
            "firstmate's supervision runtime, tied to its panes and processes",
        ),
        (
            "runtime",
            "state/*.check.sh, state/*.check-trust",
            plan.skipped.check_scripts,
            "scripts the firstmate watcher runs; never read as anything but a name, never executed",
        ),
        (
            "status",
            "state/<id>.status with no backlog item",
            plan.skipped.status_logs_without_item,
            "nothing to attach them to",
        ),
        (
            "tasks",
            "data/<id>/ of items that were not imported, and loose files in data/",
            plan.skipped.task_dirs,
            "scratch from past tasks (probes, exports, session logs)",
        ),
        (
            "secrets",
            ".env",
            usize::from(plan.skipped.env_file),
            "never read. Store a classifier key with `security add-generic-password` and Bob's BOBSHELL_API_KEY in quarkd's environment",
        ),
    ] {
        v.push(item(cat, src, "", NotImported, n, why));
    }
    v
}

/// This module's endpoints.
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(import),
    components(schemas(ImportOutcome, ImportCount, ImportInventoryItem, ImportedProject))
)]
pub(super) struct Api;

/// This module's routes, merged into the `/v1` router.
pub(super) fn router() -> axum::Router<AppState> {
    axum::Router::new().route("/v1/imports/firstmate", axum::routing::post(import))
}
