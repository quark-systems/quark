//! The judgment-only coordinator (slice 6), in shadow beside the bash
//! engine or native once slices 1 to 5 are.
//!
//! Slice 6 switches on after slices 1 to 5, so firstmate's coordinator still
//! runs every Project. Unless [`ENV`] is `0`, quarkd runs the native
//! coordinator on a timer, for each Project:
//!
//! - builds its layered prompt (built-in, persona, the Project repo's
//!   instructions and memory, code repo instructions, skills) and records it
//!   when it changes;
//! - in shadow, reads firstmate's coordinator transcript into
//!   `coordinator.baseline` turns, the token baseline the native coordinator
//!   is measured against, and records which events would have woken the
//!   native coordinator (`coordinator.would_wake`);
//! - native, points a Claude Code coordinator at its tools and turn-end hook
//!   ([`crate::coordinator_http::install_claude`]), wakes it through the
//!   [`Brain`] and runs its calls through the [`Hands`]
//!   ([`crate::coordinator_hands`]).
//!
//! See `docs/engine/coordinator.md`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use quark_coordinator::{
    Brain, Config, Coordinator, Hands, LayeredPrompt, Mode, NoBrain, NoHands, Sources,
};
use quark_core::{EventLog, ProjectId};
use quark_persona::FilePersonas;
use quark_transcript::{locate, SessionFormat, SessionRoots};

use crate::coordinator_http::{install_claude, CoordinatorUrls, Tokens};
use crate::provision::Layout;
use crate::store::Store;

/// Set to `0` to turn the slice 6 coordinator off.
pub const ENV: &str = "QUARK_NATIVE_COORDINATOR";

pub fn enabled() -> bool {
    !std::env::var(ENV).is_ok_and(|v| v == "0")
}

/// How the coordinator is wired.
pub enum Wiring {
    /// Beside firstmate: records, never acts.
    Shadow,
    /// Wakes the coordinator and runs its tools. `base_url` is quarkd's own
    /// address, for the coordinator's MCP server and turn-end hook.
    Native {
        brain: Arc<dyn Brain>,
        hands: Arc<dyn Hands>,
        base_url: String,
    },
}

/// The slice 6 coordinator and the Projects it serves.
pub struct NativeCoordinator {
    coordinator: Arc<Coordinator>,
    store: Arc<Store>,
    layout: Layout,
    roots: SessionRoots,
    tokens: Tokens,
    base_url: Option<String>,
}

impl NativeCoordinator {
    pub async fn open(
        store: Arc<Store>,
        log: Arc<dyn EventLog>,
        layout: Layout,
        roots: SessionRoots,
        wiring: Wiring,
    ) -> anyhow::Result<Self> {
        let (mode, brain, hands, base_url): (Mode, Arc<dyn Brain>, Arc<dyn Hands>, _) = match wiring
        {
            Wiring::Shadow => (Mode::Shadow, Arc::new(NoBrain), Arc::new(NoHands), None),
            Wiring::Native {
                brain,
                hands,
                base_url,
            } => (Mode::Native, brain, hands, Some(base_url)),
        };
        let coordinator = Coordinator::open(
            log,
            crate::event_ingest::host(),
            mode,
            Config::default(),
            brain,
            hands,
        )
        .await?;
        Ok(Self {
            coordinator: Arc::new(coordinator),
            store,
            tokens: Tokens::new(&layout.home),
            layout,
            roots,
            base_url,
        })
    }

    /// The coordinator, for its routes.
    pub fn coordinator(&self) -> Arc<Coordinator> {
        self.coordinator.clone()
    }

    /// Each Project coordinator's token, for its routes.
    pub fn tokens(&self) -> Tokens {
        self.tokens.clone()
    }

    /// Refresh each Project's prompt, then its baseline (shadow) or its
    /// tool and hook config (native), then evaluate.
    pub async fn pass(&self) -> anyhow::Result<()> {
        let store = self.store.clone();
        let projects = tokio::task::spawn_blocking(move || store.list_projects()).await??;
        for project in projects {
            let Some(workspace) = project.workspace_path.clone() else {
                continue;
            };
            let id = ProjectId::new(project.id.clone());
            let workspace = PathBuf::from(workspace);
            let layout = self.layout.clone();
            let ws = workspace.clone();
            let pid = project.id.clone();
            let built =
                tokio::task::spawn_blocking(move || build_prompt(&layout, &pid, &ws)).await?;
            match built {
                Ok((prompt, persona)) => {
                    if let Err(e) = self.coordinator.set_prompt(&id, prompt, &persona).await {
                        tracing::warn!(project = %id, error = %e, "slice 6 prompt not recorded");
                    }
                }
                Err(e) => tracing::warn!(project = %id, error = %e, "slice 6 prompt not built"),
            }
            match &self.base_url {
                None => {
                    if let Some(path) = locate(SessionFormat::Claude, &workspace, &self.roots) {
                        if let Err(e) = self.coordinator.read_baseline(&id, &path).await {
                            tracing::warn!(project = %id, error = %e, "slice 6 baseline not read");
                        }
                    }
                }
                Some(base) => {
                    let harness = project.agent_config.as_ref().map(|a| a.harness.as_str());
                    if let Err(e) = self.install(base, &project.id, &workspace, harness) {
                        tracing::warn!(project = %id, error = %e, "coordinator tools not installed");
                    }
                }
            }
        }
        let r = self
            .coordinator
            .tick(time::OffsetDateTime::now_utc())
            .await?;
        if r.turns > 0 {
            tracing::debug!(
                items = r.items,
                turns = r.turns,
                mode = ?self.coordinator.mode(),
                "slice 6 opened coordinator turns"
            );
        }
        Ok(())
    }

    /// Point the Project's coordinator at its tools and turn-end hook. Only
    /// Claude Code coordinators are configured so far.
    fn install(
        &self,
        base: &str,
        project: &str,
        workspace: &Path,
        harness: Option<&str>,
    ) -> std::io::Result<bool> {
        if harness.is_some_and(|h| !h.starts_with("claude")) {
            return Ok(false);
        }
        let token = self.tokens.get_or_create(project)?;
        install_claude(workspace, &CoordinatorUrls::new(base, project, &token))
    }

    pub async fn run(self, interval: Duration) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if let Err(e) = self.pass().await {
                tracing::error!(error = %format!("{e:#}"), "slice 6 pass failed");
            }
        }
    }
}

/// The Project's layered prompt from its workspace, with the id of the
/// persona pack it speaks in.
pub(crate) fn build_prompt(
    layout: &Layout,
    project: &str,
    workspace: &Path,
) -> quark_core::Result<(LayeredPrompt, String)> {
    let pack = FilePersonas::new(layout.home.clone())
        .resolve(project)?
        .pack;
    let persona = pack.id.clone();
    let sources = sources(workspace, pack, layout.user_memory());
    Ok((LayeredPrompt::build(&sources), persona))
}

/// A Project's prompt sources in its workspace: the Project repo checkout
/// (`project/`), then each code repo (under `projects/` in a firstmate
/// workspace, `repos/` in a native one), the Project's memory and the
/// user's.
fn sources(workspace: &Path, persona: quark_core::PersonaPack, user_memory: PathBuf) -> Sources {
    let project_repo = workspace.join("project");
    let mut repos = vec![("project".to_string(), project_repo.clone())];
    for dir in ["projects", crate::engine::native_project::REPOS_DIR] {
        let Ok(rd) = std::fs::read_dir(workspace.join(dir)) else {
            continue;
        };
        let mut code: Vec<(String, PathBuf)> = rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join(".git").exists())
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .collect();
        code.sort();
        repos.extend(code);
    }
    Sources {
        persona,
        repos,
        memory: vec![project_repo.join("memory"), user_memory],
        skills: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use quark_core::Seq;
    use quark_systems::CreateProject;

    use super::*;

    #[tokio::test]
    async fn a_pass_records_the_prompt_and_baseline_and_acts_on_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(ws.join("project/memory")).unwrap();
        std::fs::write(ws.join("project/instructions.md"), "Ship small PRs.\n").unwrap();
        std::fs::write(ws.join("project/memory/0001.md"), "Rebase first.\n").unwrap();
        std::fs::create_dir_all(ws.join("projects/quark/.git")).unwrap();
        std::fs::write(ws.join("projects/quark/AGENTS.md"), "Run cargo test.\n").unwrap();
        // firstmate's coordinator transcript, where Claude Code keeps it.
        let claude = dir.path().join("claude");
        let slug: String = ws
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let logs = claude.join("projects").join(slug);
        std::fs::create_dir_all(&logs).unwrap();
        let user = |id: &str, text: &str| {
            format!(
                "{}\n",
                serde_json::json!({"type": "user", "uuid": id, "timestamp": "t",
                    "message": {"role": "user", "content": text}})
            )
        };
        std::fs::write(
            logs.join("s.jsonl"),
            [
                user("u1", "<task-notification>x</task-notification>"),
                user("u2", "hi"),
            ]
            .concat(),
        )
        .unwrap();

        let store = Arc::new(Store::open_in_memory().unwrap());
        let project = store
            .create_project(CreateProject {
                name: "Quark".into(),
                goal: None,
                workspace_path: Some(ws.display().to_string()),
                repos: vec![],
                agent_config: None,
                dispatch_preset: None,
                delivery: None,
            })
            .unwrap();
        let log = quark_eventlog::SqliteEventLog::open(dir.path().join("events.db")).unwrap();
        let roots = SessionRoots {
            claude: vec![claude],
            ..Default::default()
        };
        let shadow = NativeCoordinator::open(
            store,
            Arc::new(log.clone()),
            Layout::new(dir.path().join("home")),
            roots,
            Wiring::Shadow,
        )
        .await
        .unwrap();
        shadow.pass().await.unwrap();
        shadow.pass().await.unwrap();
        let kinds: Vec<String> = log
            .read(Seq::ZERO, 100)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.project.as_str() == project.id)
            .map(|e| e.kind.0)
            .collect();
        assert_eq!(
            kinds,
            [
                "coordinator.prompt",
                "coordinator.baseline",
                "coordinator.transcript_read"
            ]
        );
        let prompt = shadow
            .coordinator
            .prompt(&ProjectId::new(project.id))
            .unwrap();
        for want in [
            "Ship small PRs.",
            "Run cargo test.",
            "Rebase first.",
            "captain",
        ] {
            assert!(prompt.contains(want), "{want} missing from the prompt");
        }
    }
}
