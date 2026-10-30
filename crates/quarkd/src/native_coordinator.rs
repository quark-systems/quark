//! The judgment-only coordinator (slice 6).
//!
//! With the native engine, quarkd runs the coordinator on a timer, for each
//! Project:
//!
//! - builds its layered prompt (built-in, persona, the Project repo's
//!   instructions and memory, code repo instructions, skills) and records it
//!   when it changes;
//! - points a Claude Code coordinator at its tools and turn-end hook
//!   ([`crate::coordinator_http::install`]), wakes it through the
//!   [`Brain`] and runs its calls through the [`Hands`]
//!   ([`crate::coordinator_hands`]);
//! - closes the window of a coordinator that was launched with an older
//!   prompt, once it is between turns. A harness reads its system
//!   prompt at launch only, so a persona, instruction or memory change
//!   reaches a running coordinator when coordinator recovery
//!   ([`crate::recovery`]) starts it again in its conversation.
//!
//! See `docs/engine/coordinator.md`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use quark_coordinator::{Brain, Config, Coordinator, Hands, LayeredPrompt, Sources};
use quark_core::{EventLog, ProjectId};
use quark_persona::FilePersonas;
use quark_transcript::{locate, SessionFormat, SessionRoots};

use crate::coordinator_http::{install, CoordinatorUrls, Tokens};
use crate::engine::native_project::{prompt_text, PROMPT_FILE, STATE_DIR, WINDOW_PREFIX};
use crate::provision::Layout;
use crate::sessions::Sessions;
use crate::store::Store;

/// How long a coordinator's conversation must have been still before it is
/// started again on a new prompt: a turn the user began has no wake to show
/// it is open.
const QUIET: Duration = Duration::from_secs(60);

/// How the coordinator is woken and acts. `base_url` is quarkd's own
/// address, for the coordinator's MCP server and turn-end hook.
pub struct Wiring {
    pub brain: Arc<dyn Brain>,
    pub hands: Arc<dyn Hands>,
    pub base_url: String,
}

/// The slice 6 coordinator and the Projects it serves.
pub struct NativeCoordinator {
    coordinator: Arc<Coordinator>,
    store: Arc<Store>,
    layout: Layout,
    roots: SessionRoots,
    tokens: Tokens,
    base_url: String,
    sessions: Option<Sessions>,
}

impl NativeCoordinator {
    pub async fn open(
        store: Arc<Store>,
        log: Arc<dyn EventLog>,
        layout: Layout,
        roots: SessionRoots,
        wiring: Wiring,
    ) -> anyhow::Result<Self> {
        let Wiring {
            brain,
            hands,
            base_url,
        } = wiring;
        let coordinator =
            Coordinator::open(log, crate::host(), Config::default(), brain, hands).await?;
        Ok(Self {
            coordinator: Arc::new(coordinator),
            store,
            tokens: Tokens::new(&layout.home),
            layout,
            roots,
            base_url,
            sessions: None,
        })
    }

    /// Where coordinators run, so one on an older prompt can be started
    /// again.
    pub fn with_sessions(mut self, sessions: Sessions) -> Self {
        self.sessions = Some(sessions);
        self
    }

    /// The coordinator, for its routes.
    pub fn coordinator(&self) -> Arc<Coordinator> {
        self.coordinator.clone()
    }

    /// Each Project coordinator's token, for its routes.
    pub fn tokens(&self) -> Tokens {
        self.tokens.clone()
    }

    /// Refresh each Project's prompt and its tool and hook config, then
    /// evaluate.
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
                    self.retire_stale(&id, &workspace, &prompt_text(&prompt))
                        .await;
                    if let Err(e) = self.coordinator.set_prompt(&id, prompt, &persona).await {
                        tracing::warn!(project = %id, error = %e, "slice 6 prompt not recorded");
                    }
                }
                Err(e) => tracing::warn!(project = %id, error = %e, "slice 6 prompt not built"),
            }
            let harness = project.agent_config.as_ref().map(|a| a.harness.as_str());
            if let Err(e) = self.install(&project.id, &workspace, harness) {
                tracing::warn!(project = %id, error = %e, "coordinator tools not installed");
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
                "slice 6 opened coordinator turns"
            );
        }
        Ok(())
    }

    /// Close the window of a coordinator that was launched with a prompt
    /// other than `current`, when it is between turns. Coordinator recovery
    /// starts it again, in its conversation, on `current`.
    async fn retire_stale(&self, id: &ProjectId, workspace: &Path, current: &str) {
        let Some(sessions) = &self.sessions else {
            return;
        };
        // Written at each launch; missing before the first one.
        let Ok(launched) = std::fs::read_to_string(workspace.join(STATE_DIR).join(PROMPT_FILE))
        else {
            return;
        };
        if launched == current {
            return;
        }
        if self.coordinator.open_turn(id).await.is_some()
            || !self.coordinator.pending(id).await.is_empty()
        {
            return;
        }
        let transcript = locate(SessionFormat::Claude, workspace, &self.roots);
        if !still(transcript.as_deref(), SystemTime::now()) {
            return;
        }
        let Ok(server) = sessions.server() else {
            return;
        };
        let window = format!("{WINDOW_PREFIX}{id}");
        let Ok(panes) = server.list_panes().await else {
            return;
        };
        let Some(target) = panes
            .into_iter()
            .map(|p| p.target)
            .find(|t| t.split_once(':').is_some_and(|(_, w)| w == window))
        else {
            // Not running: its next launch reads the current prompt.
            return;
        };
        match server.run(&["kill-window", "-t", &target]).await {
            Ok(_) => {
                tracing::info!(project = %id, "coordinator prompt changed; starting it again")
            }
            Err(e) => {
                tracing::warn!(project = %id, error = %e, "could not close a coordinator on an old prompt")
            }
        }
    }

    /// Point the Project's coordinator at its tools and turn-end hook.
    fn install(
        &self,
        project: &str,
        workspace: &Path,
        harness: Option<&str>,
    ) -> std::io::Result<bool> {
        let harness = harness.unwrap_or(crate::engine::native_project::COORDINATOR_HARNESS);
        if crate::coordinator_http::Wiring::for_harness(harness).is_none() {
            return Ok(false);
        }
        let token = self.tokens.get_or_create(project)?;
        install(
            harness,
            workspace,
            &CoordinatorUrls::new(&self.base_url, project, &token),
        )
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

/// Whether the conversation at `transcript` has been still for [`QUIET`].
/// One that cannot be read counts as still.
fn still(transcript: Option<&Path>, now: SystemTime) -> bool {
    let Some(modified) = transcript
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
    else {
        return true;
    };
    now.duration_since(modified).is_ok_and(|d| d >= QUIET)
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
/// (`project/`), then each code repo (under `repos/`, or `projects/` in a
/// workspace firstmate made), the Project's memory and the user's.
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
    async fn a_pass_records_the_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(ws.join("project/memory")).unwrap();
        std::fs::write(ws.join("project/instructions.md"), "Ship small PRs.\n").unwrap();
        std::fs::write(ws.join("project/memory/0001.md"), "Rebase first.\n").unwrap();
        std::fs::create_dir_all(ws.join("projects/quark/.git")).unwrap();
        std::fs::write(ws.join("projects/quark/AGENTS.md"), "Run cargo test.\n").unwrap();
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
        let coordinator = NativeCoordinator::open(
            store,
            Arc::new(log.clone()),
            Layout::new(dir.path().join("home")),
            SessionRoots::default(),
            Wiring {
                brain: Arc::new(quark_coordinator::NoBrain),
                hands: Arc::new(quark_coordinator::NoHands),
                base_url: "http://127.0.0.1:1".into(),
            },
        )
        .await
        .unwrap();
        coordinator.pass().await.unwrap();
        coordinator.pass().await.unwrap();
        let kinds: Vec<String> = log
            .read(Seq::ZERO, 100)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.project.as_str() == project.id)
            .map(|e| e.kind.0)
            .collect();
        assert_eq!(kinds, ["coordinator.prompt"]);
        let prompt = coordinator
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

    #[test]
    fn a_conversation_is_still_once_it_has_been_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, "{}").unwrap();
        let now = SystemTime::now();
        // Just written: a turn may be under way.
        assert!(!still(Some(&path), now));
        assert!(still(Some(&path), now + QUIET + Duration::from_secs(1)));
        // Nothing to read is nothing to interrupt.
        assert!(still(None, now));
        assert!(still(Some(&dir.path().join("missing")), now));
    }
}
