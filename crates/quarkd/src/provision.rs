//! Project creation (journey J2).
//!
//! `POST /v1/projects` with repos records the Project as `provisioning` and
//! runs [`provision`] in the background. The steps, in order:
//!
//! 1. clone every repo into the command-center workspace;
//! 2. seed the Project workspace from those clones (a local secondmate of the
//!    command center, ADR-6);
//! 3. write the Project repo (`project.yaml`, `dispatch.yaml`,
//!    `instructions.md`, `memory/`) and check it out inside the workspace;
//! 4. start the Project coordinator with the Project's agent config.
//!
//! Each step emits a `project.updated` event with what is happening, and is
//! recorded as an adapter call. A failure stops provisioning with status
//! `failed` and the step and error in `status_detail`; every step is safe to
//! run again, so `POST /v1/projects/{id}:provision` retries from the start.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use quark_systems::{
    CreateProject, DeliveryPolicy, DispatchPreset, Project, ProjectStatus, RepoSource,
};

use crate::engine::{EngineAdapter, SourceRepo, WorkspacePlan, WorkspaceRef};
use crate::project_repo;
use crate::store::Store;

/// Where Quark keeps workspaces and Project repos under its home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub home: PathBuf,
}

impl Layout {
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }

    /// The command-center workspace (a firstmate primary).
    pub fn command_workspace(&self) -> PathBuf {
        self.home.join("workspaces/command")
    }

    pub fn workspace(&self, project_id: &str) -> PathBuf {
        self.home.join("workspaces").join(project_id)
    }

    /// The bare Project repo.
    pub fn project_repo(&self, project_id: &str) -> PathBuf {
        self.home.join("projects").join(format!("{project_id}.git"))
    }
}

const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// Validate a create request and fill in defaults: repo names derived from
/// URLs, preset `single`, delivery `gated`. A request without repos is only
/// recorded, as before.
pub fn normalize(mut input: CreateProject) -> Result<CreateProject, String> {
    input.name = input.name.trim().to_string();
    if input.name.is_empty() {
        return Err("name must not be empty".into());
    }
    if input.repos.is_empty() {
        return Ok(input);
    }
    if input.workspace_path.is_some() {
        return Err(
            "give either repos to provision a workspace or workspace_path to attach one, not both"
                .into(),
        );
    }
    let agent = input
        .agent_config
        .as_mut()
        .ok_or("agent_config is required to provision a Project")?;
    agent.harness = agent.harness.trim().to_string();
    if !token(&agent.harness, false) {
        return Err(format!(
            "agent_config.harness {:?} must match [a-z0-9-]+",
            agent.harness
        ));
    }
    if let Some(m) = &agent.model {
        if !token(m, true) {
            return Err(format!("agent_config.model {m:?} is not a model id"));
        }
    }
    if let Some(e) = &agent.effort {
        if !EFFORTS.contains(&e.as_str()) {
            return Err(format!(
                "agent_config.effort {e:?} must be one of {}",
                EFFORTS.join(", ")
            ));
        }
    }
    let mut seen = Vec::new();
    for repo in &mut input.repos {
        repo.url = repo.url.trim().to_string();
        if repo.url.is_empty()
            || repo.url.starts_with('-')
            || repo
                .url
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(format!("repo url {:?} is not a clone URL", repo.url));
        }
        let name = match repo.name.as_deref().map(str::trim) {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => name_from_url(&repo.url)
                .ok_or_else(|| format!("cannot derive a name from {:?}; give one", repo.url))?,
        };
        if !repo_name(&name) {
            return Err(format!(
                "repo name {name:?} must match [A-Za-z0-9._-]+ without a leading dot or dash"
            ));
        }
        if seen.contains(&name) {
            return Err(format!("repo name {name:?} is used twice"));
        }
        seen.push(name.clone());
        repo.name = Some(name);
    }
    input.dispatch_preset.get_or_insert(DispatchPreset::Single);
    input.delivery.get_or_insert(DeliveryPolicy::Gated);
    Ok(input)
}

/// The last path segment of a clone URL, without `.git`.
pub fn name_from_url(url: &str) -> Option<String> {
    let trimmed = url.trim_end_matches('/');
    let last = trimmed.rsplit(['/', ':']).next()?;
    let name = last.strip_suffix(".git").unwrap_or(last);
    repo_name(name).then(|| name.to_string())
}

fn repo_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && !s.starts_with(['.', '-'])
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn token(s: &str, model: bool) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && !s.starts_with('-')
        && s.bytes().all(|b| {
            if model {
                b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'/' | b'+' | b'-')
            } else {
                b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'
            }
        })
}

/// Run every provisioning step for `project_id`. Never panics or returns an
/// error: the outcome is the Project's status.
pub async fn provision(
    store: Arc<Store>,
    engine: Arc<dyn EngineAdapter>,
    layout: Layout,
    project_id: String,
) {
    let p = Provisioner {
        store,
        engine,
        layout,
        project_id,
    };
    match p.run().await {
        Ok(project) => {
            tracing::info!(project = %project.id, workspace = ?project.workspace_path, "Project ready")
        }
        Err(detail) => {
            tracing::warn!(project = %p.project_id, %detail, "Project provisioning failed");
            p.status(ProjectStatus::Failed, Some(&detail), None, None)
                .await;
        }
    }
}

struct Provisioner {
    store: Arc<Store>,
    engine: Arc<dyn EngineAdapter>,
    layout: Layout,
    project_id: String,
}

impl Provisioner {
    async fn run(&self) -> Result<Project, String> {
        let project = self
            .db(|s, id| s.get_project(id))
            .await
            .map_err(|e| format!("loading the Project: {e}"))?;
        let agent = project
            .agent_config
            .clone()
            .ok_or("the Project has no agent config")?;
        let delivery = project.delivery.unwrap_or(DeliveryPolicy::Gated);
        let sources = sources(&project.repos)?;
        let command = self.layout.command_workspace();
        let root = self.layout.workspace(&project.id);
        let bare = self.layout.project_repo(&project.id);

        std::fs::create_dir_all(&command)
            .map_err(|e| format!("creating {}: {e}", command.display()))?;

        for source in &sources {
            self.step(
                &format!("Cloning {}", source.name),
                "add_source",
                self.engine.add_source(&command, source, delivery),
            )
            .await?;
        }

        let plan = WorkspacePlan {
            project_id: project.id.clone(),
            name: project.name.clone(),
            goal: project.goal.clone(),
            sources: sources.clone(),
            root: root.clone(),
        };
        let root = self
            .step(
                "Seeding the Project workspace",
                "seed_workspace",
                self.engine.seed_workspace(&command, &plan),
            )
            .await?;

        let checkout = root.join("project");
        let (b, c, snapshot) = (bare.clone(), checkout.clone(), project.clone());
        self.step(
            "Writing the Project repo",
            "project_repo_init",
            async move {
                tokio::task::spawn_blocking(move || project_repo::init(&b, &c, &snapshot))
                    .await
                    .map_err(|e| e.to_string())?
                    .map_err(|e| e.to_string())
            },
        )
        .await?;

        let ws = WorkspaceRef {
            project_id: project.id.clone(),
            root: root.clone(),
        };
        self.step(
            "Starting the coordinator",
            "start_coordinator",
            self.engine.start_coordinator(&command, &ws, &agent),
        )
        .await?;

        Ok(self
            .status(ProjectStatus::Ready, None, Some(&root), Some(&bare))
            .await
            .ok_or("could not record the ready Project")?)
    }

    /// Announce `label`, run `fut`, record it as adapter call `operation`, and
    /// turn a failure into `"<label>: <error>"`.
    async fn step<T, E: std::fmt::Display>(
        &self,
        label: &str,
        operation: &'static str,
        fut: impl std::future::Future<Output = Result<T, E>>,
    ) -> Result<T, String> {
        self.status(ProjectStatus::Provisioning, Some(label), None, None)
            .await;
        let started = Instant::now();
        let res = fut.await;
        let elapsed = started.elapsed().as_millis() as u64;
        let detail = res.as_ref().err().map(|e| e.to_string());
        let (store, id) = (self.store.clone(), self.project_id.clone());
        let d = detail.clone();
        let _ = tokio::task::spawn_blocking(move || {
            store.record_adapter_call(Some(&id), operation, d.is_none(), elapsed, d.as_deref())
        })
        .await;
        res.map_err(|e| format!("{label}: {e}"))
    }

    async fn status(
        &self,
        status: ProjectStatus,
        detail: Option<&str>,
        workspace: Option<&Path>,
        repo: Option<&Path>,
    ) -> Option<Project> {
        let detail = detail.map(str::to_string);
        let workspace = workspace.map(|p| p.to_string_lossy().into_owned());
        let repo = repo.map(|p| p.to_string_lossy().into_owned());
        let res = self
            .db(move |s, id| {
                s.set_project_status(
                    id,
                    status,
                    detail.as_deref(),
                    workspace.as_deref(),
                    repo.as_deref(),
                )
            })
            .await;
        match res {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::error!(project = %self.project_id, error = %e, "could not record Project status");
                None
            }
        }
    }

    async fn db<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Store, &str) -> crate::store::Result<T> + Send + 'static,
    ) -> crate::store::Result<T> {
        let (store, id) = (self.store.clone(), self.project_id.clone());
        tokio::task::spawn_blocking(move || f(&store, &id))
            .await
            .expect("store task panicked")
    }
}

fn sources(repos: &[RepoSource]) -> Result<Vec<SourceRepo>, String> {
    repos
        .iter()
        .map(|r| {
            Ok(SourceRepo {
                name: r
                    .name
                    .clone()
                    .ok_or_else(|| format!("repo {} has no name", r.url))?,
                url: r.url.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use quark_systems::AgentConfig;

    fn example_agent() -> AgentConfig {
        AgentConfig {
            harness: "claude-code".into(),
            model: None,
            effort: None,
        }
    }

    fn create(repos: &[(&str, Option<&str>)]) -> CreateProject {
        CreateProject {
            name: " Quark ".into(),
            goal: None,
            workspace_path: None,
            repos: repos
                .iter()
                .map(|(u, n)| RepoSource {
                    url: u.to_string(),
                    name: n.map(str::to_string),
                })
                .collect(),
            agent_config: Some(example_agent()),
            dispatch_preset: None,
            delivery: None,
        }
    }

    #[test]
    fn derives_names_and_defaults() {
        let n = normalize(create(&[
            ("https://github.com/quark-systems/quark.git", None),
            ("git@github.com:quark-systems/firstmate.git", None),
            ("/srv/repos/tool/", Some("tooling")),
        ]))
        .unwrap();
        let names: Vec<_> = n.repos.iter().map(|r| r.name.clone().unwrap()).collect();
        assert_eq!(names, ["quark", "firstmate", "tooling"]);
        assert_eq!(n.name, "Quark");
        assert_eq!(n.dispatch_preset, Some(DispatchPreset::Single));
        assert_eq!(n.delivery, Some(DeliveryPolicy::Gated));
    }

    #[test]
    fn refuses_bad_requests() {
        assert!(normalize(create(&[("https://h/a.git", None), ("https://g/a", None)])).is_err());
        assert!(normalize(create(&[("-oProxy", None)])).is_err());
        assert!(normalize(create(&[("https://h/r", Some(".x"))])).is_err());
        let mut c = create(&[("https://h/r", None)]);
        c.agent_config = None;
        assert!(normalize(c).is_err());
        let mut c = create(&[("https://h/r", None)]);
        c.workspace_path = Some("/w".into());
        assert!(normalize(c).is_err());
        let mut c = create(&[("https://h/r", None)]);
        c.agent_config.as_mut().unwrap().effort = Some("ultra".into());
        assert!(normalize(c).is_err());
        let mut c = create(&[("https://h/r", None)]);
        c.agent_config.as_mut().unwrap().harness = "Claude Code".into();
        assert!(normalize(c).is_err());
        // Without repos the request is only recorded.
        assert!(normalize(create(&[])).is_ok());
    }
}
