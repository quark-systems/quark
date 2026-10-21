//! Seeding a Project from a firstmate home (`crate::firstmate_import`).
//!
//! `POST /v1/imports/firstmate` reads the home and plans the import; unless
//! it is a dry run it creates the Project as `POST /v1/projects` does and
//! answers at once. The rest lands in the background once the Project is
//! provisioned and its Beads database is ready: dispatch rules,
//! instructions, memory, library files, standing approval, user-level
//! memory and quota settings, then the backlog as beads, and last the
//! report at `library/firstmate/IMPORT.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use quark_systems::{
    AgentConfig, AgentRole, BeadsState, ErrorBody, FirstmateImport, ImportFirstmate, ImportOutcome,
    MemoryEntry, Project, ProjectStatus, UpdateProject, UserMemoryEntry,
};

use super::{db, ApiError, AppState};
use crate::firstmate_import::{self, plan::LIBRARY, Seed, FALLBACK_HARNESS};
use crate::{classifier, crew_dispatch, memory, project_repo};

/// How long the import waits for the Project to provision.
const PROVISION_WAIT: Duration = Duration::from_secs(60 * 60);
/// How long it waits for the Project's Beads database.
const BEADS_WAIT: Duration = Duration::from_secs(10 * 60);
const POLL: Duration = Duration::from_secs(2);
/// Who imported entries are recorded as accepted by.
const IMPORTER: &str = "firstmate import";

/// Import a firstmate home into a new Project.
///
/// Reads the home at `path` (firstmate's `FM_HOME`: `data/`, `config/`,
/// `state/`, `projects/`) without changing it, and plans the Project: each
/// project with a clone under `projects/` is a repo, cloned from its
/// `origin`; ship modes set delivery and `+yolo` on every project standing
/// approval; the crew harness runs the coordinator. With `dry_run` only the
/// plan is answered. Otherwise the Project is created and provisions in the
/// background, and once it is ready the import commits `dispatch.yaml` from
/// `config/crew-dispatch.json`, its notes to `instructions.md`, learnings to
/// `memory/` and firstmate's registry files, briefs, reports and status logs
/// to `library/firstmate/`; adds captain preferences to user-level memory;
/// copies `config/bob-quota.json` when Quark has none; and creates a bead
/// per backlog task with its status, brief, waits and captain holds.
/// `library/firstmate/IMPORT.md` records what was imported.
#[utoipa::path(
    post,
    path = "/v1/imports/firstmate",
    tag = "projects",
    request_body = ImportFirstmate,
    responses(
        (status = 200, description = "Dry run: the plan; nothing was created", body = FirstmateImport),
        (status = 201, description = "The Project was created and is provisioning; the import lands once it is ready", body = FirstmateImport),
        (status = 400, description = "Not a firstmate home, nothing to import, or the Project would be invalid", body = ErrorBody)
    )
)]
pub async fn import_firstmate(
    State(state): State<AppState>,
    Json(input): Json<ImportFirstmate>,
) -> Result<(StatusCode, Json<FirstmateImport>), ApiError> {
    let root = expand(&input.path)?;
    let harnesses = state.harnesses.clone();
    let planned = input.clone();
    let mut seed = tokio::task::spawn_blocking(move || {
        let home = firstmate_import::read(&root)?;
        let resolve = |name: &str| harnesses.resolve(name).map(|h| h.id().to_string());
        firstmate_import::plan(&home, &planned, &resolve, &crate::now_rfc3339()[..10])
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .map_err(ApiError::invalid)?;

    // A harness that cannot coordinate still runs the crew through the
    // dispatch rules; the coordinator falls back.
    if let Some(agent) = seed.plan.project.agent_config.clone() {
        let check = state.harnesses.validate(&agent, AgentRole::Coordinator);
        if !check.valid && agent.harness != FALLBACK_HARNESS {
            let why: Vec<_> = check.errors.into_iter().map(|e| e.message).collect();
            seed.plan.warnings.push(format!(
                "{} cannot run the coordinator ({}); it runs on {FALLBACK_HARNESS}",
                agent.harness,
                why.join("; ")
            ));
            seed.plan.project.agent_config = Some(AgentConfig {
                harness: FALLBACK_HARNESS.into(),
                model: None,
                effort: None,
                pool: None,
            });
        }
    }

    if input.dry_run {
        return Ok((
            StatusCode::OK,
            Json(FirstmateImport {
                plan: seed.plan,
                project: None,
            }),
        ));
    }
    let (_, Json(project)) =
        super::routes::create_project(State(state.clone()), Json(seed.plan.project.clone()))
            .await?;
    tokio::spawn(apply(state, project.id.clone(), seed.clone()));
    Ok((
        StatusCode::CREATED,
        Json(FirstmateImport {
            plan: seed.plan,
            project: Some(project),
        }),
    ))
}

/// `~/` expanded; the path must be absolute.
fn expand(path: &str) -> Result<PathBuf, ApiError> {
    let path = path.trim();
    let expanded = match path.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(rest))
            .ok_or_else(|| ApiError::invalid("HOME is not set to expand ~/"))?,
        None => PathBuf::from(path),
    };
    if !expanded.is_absolute() {
        return Err(ApiError::invalid(format!("path {path:?} must be absolute")));
    }
    Ok(expanded)
}

/// What the background part did, for the report.
#[derive(Default)]
struct Done {
    lines: Vec<String>,
    problems: Vec<String>,
}

impl Done {
    fn ok(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }

    fn failed(&mut self, what: &str, e: impl std::fmt::Display) {
        tracing::warn!(%what, error = %e, "firstmate import step failed");
        self.problems.push(format!("{what}: {e}"));
    }
}

async fn apply(state: AppState, project_id: String, seed: Seed) {
    let project = match wait_provisioned(&state, &project_id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(project = %project_id, error = %e, "firstmate import stopped");
            return;
        }
    };
    let mut done = Done::default();
    let (Some(bare), Some(root)) = (
        project.project_repo_path.clone(),
        project.workspace_path.clone(),
    ) else {
        tracing::warn!(project = %project_id, "firstmate import stopped: no Project repo");
        return;
    };
    let (bare, checkout) = (PathBuf::from(bare), PathBuf::from(root).join("project"));

    {
        let _one = super::memory::COMMITTING.lock().await;
        let (b, c, s) = (bare.clone(), checkout.clone(), seed.clone());
        let user_config = state.layout.user_config();
        match tokio::task::spawn_blocking(move || commit_repo(&b, &c, &s, &user_config)).await {
            Ok(lines) => {
                for line in lines {
                    match line {
                        Ok(l) => done.ok(l),
                        Err(e) => done.problems.push(e),
                    }
                }
            }
            Err(e) => done.failed("writing the Project repo", e),
        }
    }

    if seed.plan.standing_approval {
        match super::routes::set_standing_approval(&state, &project, true).await {
            Ok(()) => {
                let id = project_id.clone();
                let on = UpdateProject {
                    standing_approval: Some(true),
                    ..UpdateProject::default()
                };
                match db(&state, move |s| s.update_project(&id, on)).await {
                    Ok(_) => done.ok("Standing approval is on, as +yolo was on every project."),
                    Err(e) => done.failed("turning standing approval on", e.message()),
                }
            }
            Err(e) => done.failed("turning standing approval on", e.message()),
        }
    }

    let (dir, quota, s, p) = (
        state.layout.user_memory(),
        state.layout.home.join("quota").join("bob.json"),
        seed.clone(),
        project.clone(),
    );
    match tokio::task::spawn_blocking(move || user_files(&dir, &quota, &s, &p)).await {
        Ok(lines) => {
            for line in lines {
                match line {
                    Ok(l) => done.ok(l),
                    Err(e) => done.problems.push(e),
                }
            }
        }
        Err(e) => done.failed("writing user-level files", e),
    }

    if !seed.issues.is_empty() {
        import_beads(&state, &project_id, &seed, &mut done).await;
    }

    let report = report(&seed, &project, &done);
    let _one = super::memory::COMMITTING.lock().await;
    let files = vec![(format!("{LIBRARY}/IMPORT.md"), report)];
    let commit = tokio::task::spawn_blocking(move || {
        project_repo::commit_files(&bare, &checkout, &files, "Record the firstmate import")
    })
    .await;
    match commit {
        Ok(Ok(_)) => tracing::info!(project = %project_id, "firstmate import done"),
        Ok(Err(e)) => tracing::warn!(project = %project_id, error = %e, "writing IMPORT.md failed"),
        Err(e) => tracing::warn!(project = %project_id, error = %e, "writing IMPORT.md failed"),
    }
    crate::beads::memories_changed(&state.store, &project_id);
}

async fn wait_provisioned(state: &AppState, project_id: &str) -> Result<Project, String> {
    let start = std::time::Instant::now();
    loop {
        let id = project_id.to_string();
        let project = db(state, move |s| s.get_project(&id))
            .await
            .map_err(|e| e.message().to_string())?;
        match project.status {
            ProjectStatus::Ready => return Ok(project),
            ProjectStatus::Failed => {
                return Err(format!(
                    "provisioning failed: {}",
                    project.status_detail.unwrap_or_default()
                ))
            }
            ProjectStatus::Provisioning if start.elapsed() > PROVISION_WAIT => {
                return Err("provisioning did not finish in an hour".into())
            }
            ProjectStatus::Provisioning => tokio::time::sleep(POLL).await,
        }
    }
}

/// A step's failure, in words.
struct Msg(String);

impl From<project_repo::RepoError> for Msg {
    fn from(e: project_repo::RepoError) -> Self {
        Msg(e.to_string())
    }
}

/// The Project repo part: dispatch rules, instructions, then memory and
/// library files in one commit. One result line per step.
fn commit_repo(
    bare: &Path,
    checkout: &Path,
    seed: &Seed,
    user_config: &Path,
) -> Vec<Result<String, String>> {
    let mut out = Vec::new();
    if let Some(draft) = &seed.dispatch {
        let written = project_repo::commit_file(
            bare,
            checkout,
            crew_dispatch::FILE,
            |current| -> Result<String, Msg> {
                let body =
                    crew_dispatch::render(draft, current.map(|(_, text)| text)).map_err(Msg)?;
                let user = classifier::user_default_at(user_config).map_err(Msg)?;
                crew_dispatch::compile(&body, user.as_ref()).map_err(Msg)?;
                Ok(body)
            },
            "Import dispatch rules from firstmate",
        );
        out.push(
            written
                .map(|_| {
                    format!(
                        "dispatch.yaml has firstmate's {} rule{} and default.",
                        draft.rules.len(),
                        if draft.rules.len() == 1 { "" } else { "s" }
                    )
                })
                .map_err(|e| format!("dispatch.yaml: {}", e.0)),
        );
    }
    let notes = project_repo::commit_file(
        bare,
        checkout,
        "instructions.md",
        |current| -> Result<String, Msg> {
            let mut body = current.map(|(_, t)| t.to_string()).unwrap_or_default();
            if !body.is_empty() && !body.ends_with('\n') {
                body.push('\n');
            }
            body.push_str(&seed.instructions);
            Ok(body)
        },
        "Add firstmate's ship modes to the instructions",
    );
    out.push(
        notes
            .map(|_| "instructions.md has each repo's firstmate ship mode.".to_string())
            .map_err(|e| format!("instructions.md: {}", e.0)),
    );

    let now = crate::now_rfc3339();
    let mut files = seed.library.clone();
    let mut names: Vec<String> = Vec::new();
    for text in &seed.memory {
        let name = memory::file_name(&now, text, |n| {
            names.iter().any(|t| t == n) || checkout.join("memory").join(n).exists()
        });
        names.push(name.clone());
        let entry = MemoryEntry {
            id: String::new(),
            project_id: String::new(),
            path: String::new(),
            text: text.clone(),
            evidence: Default::default(),
            source: None,
            date: Some(now.clone()),
            accepted_at: Some(now.clone()),
            accepted_by: Some(IMPORTER.into()),
            proposal_id: None,
            commit: None,
            beads_key: None,
        };
        files.push((format!("memory/{name}"), memory::render(&entry)));
    }
    let (library, entries) = (seed.library.len(), seed.memory.len());
    out.push(
        project_repo::commit_files(bare, checkout, &files, "Import firstmate's records")
            .map(|_| {
                format!(
                    "{library} firstmate file{} are in {LIBRARY}/ and {entries} learning{} in memory/.",
                    if library == 1 { "" } else { "s" },
                    if entries == 1 { "" } else { "s" }
                )
            })
            .map_err(|e| format!("library and memory: {e}")),
    );
    out
}

/// User-level memory and quota settings, which every Project shares: only
/// what is not there already is added.
fn user_files(
    dir: &Path,
    bob_quota: &Path,
    seed: &Seed,
    project: &Project,
) -> Vec<Result<String, String>> {
    let mut out = Vec::new();
    if !seed.user_memory.is_empty() {
        let added = (|| -> Result<usize, String> {
            let existing: Vec<String> = memory::list_user(dir)?
                .into_iter()
                .map(|e| e.text.trim().to_string())
                .collect();
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            let now = crate::now_rfc3339();
            let mut added = 0;
            for text in &seed.user_memory {
                if existing.iter().any(|e| e == text.trim()) {
                    continue;
                }
                let entry = UserMemoryEntry {
                    id: String::new(),
                    path: String::new(),
                    text: text.clone(),
                    evidence: Default::default(),
                    source: None,
                    date: Some(now.clone()),
                    project_id: Some(project.id.clone()),
                    project_name: Some(project.name.clone()),
                    entry_id: None,
                    commit: None,
                    promoted_at: Some(now.clone()),
                    promoted_by: Some(IMPORTER.into()),
                };
                let name = memory::file_name(&now, text, |n| dir.join(n).exists());
                let path = dir.join(name);
                std::fs::write(&path, memory::render_user(&entry))
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                added += 1;
            }
            Ok(added)
        })();
        out.push(
            added
                .map(|n| {
                    format!(
                        "{n} captain preference{} added to user-level memory ({} already there).",
                        if n == 1 { "" } else { "s" },
                        seed.user_memory.len() - n
                    )
                })
                .map_err(|e| format!("user-level memory: {e}")),
        );
    }
    if let Some(json) = &seed.bob_quota {
        out.push(if bob_quota.exists() {
            Ok(format!(
                "{} already exists; firstmate's Bob quota settings were not copied.",
                bob_quota.display()
            ))
        } else if serde_json::from_str::<serde_json::Value>(json).is_err() {
            Err("config/bob-quota.json is not JSON; not copied".into())
        } else {
            bob_quota
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(bob_quota, json))
                .map(|()| format!("Bob quota settings are in {}.", bob_quota.display()))
                .map_err(|e| format!("{}: {e}", bob_quota.display()))
        });
    }
    out
}

async fn import_beads(state: &AppState, project_id: &str, seed: &Seed, done: &mut Done) {
    let home = state.layout.home.clone();
    let start = std::time::Instant::now();
    loop {
        let status = state.beads.status(&home, project_id).await;
        match status.state {
            BeadsState::Ready => break,
            BeadsState::Failed | BeadsState::Unavailable => {
                done.failed(
                    "beads",
                    format!(
                        "the Beads database is not available ({}); the backlog is only in {LIBRARY}/backlog.md",
                        status.detail.unwrap_or_default()
                    ),
                );
                return;
            }
            _ if start.elapsed() > BEADS_WAIT => {
                done.failed("beads", "the Beads database was not ready in ten minutes");
                return;
            }
            _ => tokio::time::sleep(POLL).await,
        }
    }
    let mut ids = BTreeMap::new();
    for issue in &seed.issues {
        match state.beads.import_issue(&home, project_id, issue).await {
            Ok(id) => {
                ids.insert(issue.key.clone(), id);
            }
            Err(e) => done.failed(&format!("bead for task {}", issue.key), e),
        }
    }
    let mut waits = 0;
    for issue in &seed.issues {
        let Some(id) = ids.get(&issue.key) else {
            continue;
        };
        for on in issue.blocked_by.iter().filter_map(|k| ids.get(k)) {
            match state.beads.import_wait(&home, project_id, id, on).await {
                Ok(()) => waits += 1,
                Err(e) => done.failed(&format!("wait of task {}", issue.key), e),
            }
        }
    }
    let gates = seed
        .issues
        .iter()
        .filter(|i| i.hold.is_some() && ids.contains_key(&i.key))
        .count();
    done.ok(format!(
        "{} of {} backlog task{} are beads, with {waits} wait{} and {gates} captain hold{}: {}.",
        ids.len(),
        seed.issues.len(),
        if seed.issues.len() == 1 { "" } else { "s" },
        if waits == 1 { "" } else { "s" },
        if gates == 1 { "" } else { "s" },
        ids.iter()
            .map(|(k, v)| format!("{k} → {v}"))
            .collect::<Vec<_>>()
            .join(", ")
    ));
}

fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

/// `library/firstmate/IMPORT.md`.
fn report(seed: &Seed, project: &Project, done: &Done) -> String {
    let plan = &seed.plan;
    let mut s = format!(
        "# Firstmate import\n\nImported from `{}` on {} into the Project {}.\n\n",
        plan.path,
        &crate::now_rfc3339()[..10],
        project.name
    );
    s.push_str("| Firstmate data | Found | Outcome | In Quark |\n|---|---|---|---|\n");
    for item in &plan.items {
        s.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            cell(&if item.files.is_empty() {
                item.what.clone()
            } else {
                format!("{} (`{}`)", item.what, item.files.join("`, `"))
            }),
            item.count,
            match item.outcome {
                ImportOutcome::Imported => "imported",
                ImportOutcome::Skipped => "skipped",
            },
            cell(&item.note)
        ));
    }
    s.push_str("\n## What was written\n\n");
    for line in &done.lines {
        s.push_str(&format!("- {line}\n"));
    }
    if !done.problems.is_empty() {
        s.push_str("\n## What failed\n\n");
        for p in &done.problems {
            s.push_str(&format!("- {p}\n"));
        }
    }
    if !plan.warnings.is_empty() {
        s.push_str("\n## Notes\n\n");
        for w in &plan.warnings {
            s.push_str(&format!("- {w}\n"));
        }
    }
    s
}

/// This module's endpoints, merged into the served document by
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(import_firstmate))]
pub(super) struct Api;

/// This module's routes, merged into the `/v1` router.
pub(super) fn router() -> axum::Router<AppState> {
    axum::Router::new().route(
        "/v1/imports/firstmate",
        axum::routing::post(import_firstmate),
    )
}
