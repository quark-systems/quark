//! Native Project setup and coordinator start, for the native engine.
//!
//! [`ProjectSetup`] creates a Project workspace and runs its coordinator
//! with no firstmate home:
//!
//! - **Adding a repo** clones it into the command-center workspace, at
//!   `<command>/repos/<name>`, the copy every Project seeds from.
//! - **Seeding** claims the Project workspace for its Project
//!   (`.quark/workspace.json`) and clones each source into
//!   `<workspace>/repos/<name>`, from the command center's copy when there
//!   is one, with `origin` pointing at the source URL.
//! - **Starting the coordinator** builds its layered prompt (the built-in
//!   prompt, the Project's persona, the Project and code repo instructions,
//!   memory) into `.quark/coordinator.md`, points the harness at the
//!   coordinator's MCP route and turn-end hook
//!   ([`crate::coordinator_http::install_claude`]), and starts the harness in
//!   a window of quarkd's tmux server, named `coordinator-<Project id>` and
//!   run in the workspace. The prompt reaches the agent as its system prompt.
//!   With `resume`, the agent continues its latest conversation in the
//!   workspace.
//! - **Coordinator terminals** come from one record per started
//!   coordinator in `<command>/coordinators/`, so the app's coordinator
//!   terminal and chat, and coordinator recovery, find each window after a
//!   daemon restart.
//!
//! Every step is safe to run again: a clone already there is checked, not
//! repeated, and a coordinator whose window is up is not started twice.
//!
//! Only Claude Code coordinators are started so far, since its MCP server
//! and turn-end hook are the ones wired; another harness is refused.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use quark_core::harness::HarnessManifest;
use quark_harness::{LaunchVars, ManifestRegistry};
use quark_systems::AgentConfig;
use quark_transcript::{locate, SessionFormat, SessionRoots};
use serde::{Deserialize, Serialize};

use super::{EngineError, SourceRepo, WorkspacePlan, WorkspaceRef};
use crate::coordinator_http::{install_claude, CoordinatorUrls, Tokens};
use crate::provision::Layout;
use crate::sessions::{SessionError, Sessions, WindowSpec};

/// Where code repos are cloned, in the command center and in each Project
/// workspace.
pub const REPOS_DIR: &str = "repos";
/// Quark's own files inside a Project workspace.
pub const STATE_DIR: &str = ".quark";
/// The claim on a Project workspace, inside [`STATE_DIR`].
const WORKSPACE_FILE: &str = "workspace.json";
/// The coordinator's rendered prompt, inside [`STATE_DIR`].
pub const PROMPT_FILE: &str = "coordinator.md";
/// One record per started coordinator, in the command-center workspace.
const RECORDS_DIR: &str = "coordinators";
/// A coordinator's window is this prefix and its Project id.
pub const WINDOW_PREFIX: &str = "coordinator-";
/// The only harness the native coordinator runs on so far.
pub const COORDINATOR_HARNESS: &str = "claude-code";
/// Most bytes of prompt passed as the system prompt: one argument must stay
/// under the kernel's 128 KiB limit.
const MAX_SYSTEM_PROMPT: usize = 100 * 1024;
/// Account variables carried into the coordinator's window: Claude Code's,
/// the one harness it runs on.
pub const ACCOUNT_ENVS: &[&str] = &["CLAUDE_CONFIG_DIR"];
/// The variable naming the Project to the coordinator.
const PROJECT_ENV: &str = "QUARK_PROJECT";

/// The claim written into a seeded workspace.
#[derive(Debug, Serialize, Deserialize)]
struct Claim {
    project_id: String,
}

/// A started coordinator, as recorded in the command center.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoordinatorRecord {
    pub project_id: String,
    /// The tmux `session:window` target it runs in.
    pub target: String,
    pub workspace: PathBuf,
    pub harness: String,
}

/// Creates Project workspaces and starts their coordinators natively.
pub struct ProjectSetup {
    sessions: Sessions,
    layout: Layout,
    manifests: Arc<ManifestRegistry>,
    tokens: Tokens,
    base_url: String,
    roots: SessionRoots,
    /// Coordinator starts take turns, so two never race for one window.
    starts: tokio::sync::Mutex<()>,
}

impl ProjectSetup {
    /// `base_url` is quarkd's own address, for the coordinator's MCP route
    /// and turn-end hook. Harness manifests come from `<home>/harnesses`
    /// over the built-ins.
    pub fn new(sessions: Sessions, layout: Layout, base_url: impl Into<String>) -> Self {
        let (manifests, _) = ManifestRegistry::load(&layout.home.join("harnesses"));
        Self {
            sessions,
            tokens: Tokens::new(&layout.home),
            layout,
            manifests: Arc::new(manifests),
            base_url: base_url.into(),
            roots: SessionRoots::from_env(),
            starts: tokio::sync::Mutex::new(()),
        }
    }

    pub fn with_manifests(mut self, manifests: Arc<ManifestRegistry>) -> Self {
        self.manifests = manifests;
        self
    }

    /// Where coordinator conversations are looked for, before `resume`.
    pub fn with_roots(mut self, roots: SessionRoots) -> Self {
        self.roots = roots;
        self
    }

    /// Clone `source` into the command center. Idempotent for the same name
    /// and URL; another URL under the same name is refused.
    pub async fn add_source(&self, command: &Path, source: &SourceRepo) -> Result<(), EngineError> {
        check_name(&source.name)?;
        let dest = command.join(REPOS_DIR).join(&source.name);
        if dest.join(".git").exists() {
            return check_origin(&dest, source).await;
        }
        clone(&source.url, &dest, None).await
    }

    /// Claim `plan.root` for the Project and clone every source into it.
    pub async fn seed_workspace(
        &self,
        command: &Path,
        plan: &WorkspacePlan,
    ) -> Result<PathBuf, EngineError> {
        check_id(&plan.project_id)?;
        for source in &plan.sources {
            check_name(&source.name)?;
        }
        claim(&plan.root, &plan.project_id)?;
        for source in &plan.sources {
            let dest = plan.root.join(REPOS_DIR).join(&source.name);
            if dest.join(".git").exists() {
                check_origin(&dest, source).await?;
                continue;
            }
            let local = command.join(REPOS_DIR).join(&source.name);
            if local.join(".git").exists() {
                clone(local.as_os_str(), &dest, Some(&source.url)).await?;
            } else {
                clone(&source.url, &dest, None).await?;
            }
        }
        Ok(plan.root.clone())
    }

    /// Start the Project's coordinator with `agent` under `account_env`,
    /// continuing its latest conversation with `resume`. A coordinator whose
    /// window is up is left alone.
    pub async fn start_coordinator(
        &self,
        command: &Path,
        ws: &WorkspaceRef,
        agent: &AgentConfig,
        account_env: &[(String, String)],
        resume: bool,
    ) -> Result<(), EngineError> {
        check_id(&ws.project_id)?;
        let manifest = self.manifest(agent)?;
        if !ws.root.is_dir() {
            return Err(EngineError::WorkspaceNotFound(ws.root.clone()));
        }
        let _turn = self.starts.lock().await;
        let window = format!("{WINDOW_PREFIX}{}", ws.project_id);
        let record = |target: String| CoordinatorRecord {
            project_id: ws.project_id.clone(),
            target,
            workspace: ws.root.clone(),
            harness: manifest.id.clone(),
        };
        if let Some(target) = self.running(&window).await? {
            // Started before (a provisioning retry): keep it, and its record.
            return write_record(command, &record(target));
        }
        let roots = roots_for(&self.roots, account_env);
        if resume && locate(SessionFormat::Claude, &ws.root, &roots).is_none() {
            return Err(EngineError::Invalid(format!(
                "the coordinator of {} has no earlier conversation to resume",
                ws.project_id
            )));
        }
        let prompt_file = self.write_prompt(ws).await?;
        let token = self.tokens.get_or_create(&ws.project_id)?;
        install_claude(
            &ws.root,
            &CoordinatorUrls::new(&self.base_url, &ws.project_id, &token),
        )?;
        let spec = WindowSpec {
            name: window,
            argv: launch_argv(manifest, agent, &ws.root, &prompt_file, resume)?,
            cwd: Some(ws.root.clone()),
            env: launch_env(manifest, account_env, &ws.project_id),
        };
        let target = self
            .sessions
            .start_window(&spec)
            .await
            .map_err(session_error)?;
        write_record(command, &record(target))
    }

    /// Each started coordinator's window target, by Project id.
    pub fn coordinator_terminals(
        &self,
        command: &Path,
    ) -> Result<HashMap<String, String>, EngineError> {
        Ok(read_records(command)?
            .into_iter()
            .map(|r| (r.project_id, r.target))
            .collect())
    }

    fn manifest(&self, agent: &AgentConfig) -> Result<&HarnessManifest, EngineError> {
        let m = self.manifests.resolve(&agent.harness).ok_or_else(|| {
            EngineError::Invalid(format!("no harness {} is known", agent.harness))
        })?;
        if m.id != COORDINATOR_HARNESS {
            return Err(EngineError::Invalid(format!(
                "the native coordinator runs on {COORDINATOR_HARNESS} only so far \
                 (its tools and turn-end hook are wired for it alone), not {}",
                m.id
            )));
        }
        Ok(m)
    }

    /// The target of `window` when the server has it.
    async fn running(&self, window: &str) -> Result<Option<String>, EngineError> {
        let server = self.sessions.server().map_err(session_error)?;
        let panes = server
            .list_panes()
            .await
            .map_err(|e| EngineError::Command(e.to_string()))?;
        Ok(panes
            .into_iter()
            .map(|p| p.target)
            .find(|t| t.split_once(':').is_some_and(|(_, w)| w == window)))
    }

    /// Render the coordinator's layered prompt into the workspace.
    async fn write_prompt(&self, ws: &WorkspaceRef) -> Result<PathBuf, EngineError> {
        let (layout, pid, root) = (self.layout.clone(), ws.project_id.clone(), ws.root.clone());
        tokio::task::spawn_blocking(move || {
            let (prompt, _) = crate::native_coordinator::build_prompt(&layout, &pid, &root)
                .map_err(|e| EngineError::Invalid(format!("the coordinator's prompt: {e}")))?;
            let text = fit(prompt.render(), MAX_SYSTEM_PROMPT);
            let path = root.join(STATE_DIR).join(PROMPT_FILE);
            write_atomic(&path, text.as_bytes())?;
            Ok(path)
        })
        .await
        .map_err(|e| EngineError::Command(e.to_string()))?
    }
}

/// The harness's own command for `agent`, run through `sh` so the prompt
/// is read from `prompt_file` as the system prompt: a tmux command is too
/// small to carry it.
fn launch_argv(
    m: &HarnessManifest,
    agent: &AgentConfig,
    root: &Path,
    prompt_file: &Path,
    resume: bool,
) -> Result<Vec<String>, EngineError> {
    if let Some(effort) = &agent.effort {
        if !m.efforts.iter().any(|e| e == effort) {
            return Err(EngineError::Invalid(format!(
                "harness {} has no effort {effort}",
                m.id
            )));
        }
    }
    let model = match m.models.selection.as_str() {
        "automatic" | "none" => None,
        _ => agent.model.clone().or_else(|| m.models.default.clone()),
    };
    let vars = LaunchVars {
        model,
        effort: agent.effort.clone(),
        prompt: None,
        prompt_file: None,
        cwd: Some(root.to_string_lossy().into_owned()),
    };
    let mut argv = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        r#"p=$1; shift; exec "$@" --append-system-prompt "$(cat "$p")""#.to_string(),
        "quark-coordinator".to_string(),
        prompt_file.to_string_lossy().into_owned(),
    ];
    argv.extend(quark_harness::argv(m, &vars));
    if resume {
        argv.push("--continue".into());
    }
    Ok(argv)
}

fn launch_env(
    m: &HarnessManifest,
    account_env: &[(String, String)],
    project: &str,
) -> Vec<(String, String)> {
    let mut env: BTreeMap<String, String> = m.launch.env.clone();
    env.extend(account_env.iter().cloned());
    env.insert(PROJECT_ENV.into(), project.into());
    env.into_iter().collect()
}

/// `roots`, with the account's Claude config directory first when the
/// account names one.
fn roots_for(roots: &SessionRoots, account_env: &[(String, String)]) -> SessionRoots {
    let mut roots = roots.clone();
    if let Some((_, dir)) = account_env.iter().find(|(k, _)| k == "CLAUDE_CONFIG_DIR") {
        roots.claude.insert(0, PathBuf::from(dir));
    }
    roots
}

/// `text` cut at `max` bytes on a character boundary, with a note.
fn fit(text: String, max: usize) -> String {
    if text.len() <= max {
        return text;
    }
    let note = "\n\n(The prompt was cut here to fit.)\n";
    let mut end = max - note.len();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{note}", &text[..end])
}

fn session_error(e: SessionError) -> EngineError {
    EngineError::Command(format!("terminal sessions: {e}"))
}

/// Repo names are path components: `[A-Za-z0-9._-]+`, without a leading
/// dot or dash.
fn check_name(name: &str) -> Result<(), EngineError> {
    let ok = !name.is_empty()
        && !name.starts_with(['.', '-'])
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(EngineError::Invalid(format!("repo name {name:?}")))
    }
}

/// Project ids name files and tmux windows: `[A-Za-z0-9_-]+`.
fn check_id(id: &str) -> Result<(), EngineError> {
    let ok = !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(EngineError::Invalid(format!("Project id {id:?}")))
    }
}

/// Claim `root` for `project_id`. It must not exist yet, be empty, or be
/// claimed by the same Project already.
fn claim(root: &Path, project_id: &str) -> Result<(), EngineError> {
    let file = root.join(STATE_DIR).join(WORKSPACE_FILE);
    match std::fs::read(&file) {
        Ok(bytes) => {
            let claim: Claim = serde_json::from_slice(&bytes)
                .map_err(|e| EngineError::Parse(format!("{}: {e}", file.display())))?;
            if claim.project_id == project_id {
                return Ok(());
            }
            return Err(EngineError::Invalid(format!(
                "{} is the workspace of Project {}",
                root.display(),
                claim.project_id
            )));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    if root.exists() && std::fs::read_dir(root)?.next().is_some() {
        return Err(EngineError::Invalid(format!(
            "{} already exists and is not a Quark workspace",
            root.display()
        )));
    }
    let claim = Claim {
        project_id: project_id.to_string(),
    };
    let bytes = serde_json::to_vec_pretty(&claim).map_err(|e| EngineError::Parse(e.to_string()))?;
    write_atomic(&file, &bytes)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), EngineError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("quark-tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn write_record(command: &Path, record: &CoordinatorRecord) -> Result<(), EngineError> {
    let path = command
        .join(RECORDS_DIR)
        .join(format!("{}.json", record.project_id));
    let bytes = serde_json::to_vec_pretty(record).map_err(|e| EngineError::Parse(e.to_string()))?;
    if std::fs::read(&path).is_ok_and(|old| old == bytes) {
        return Ok(());
    }
    write_atomic(&path, &bytes)
}

/// Every coordinator record in the command center. An unreadable record is
/// skipped, so one bad file does not hide the others.
fn read_records(command: &Path) -> Result<Vec<CoordinatorRecord>, EngineError> {
    let dir = command.join(RECORDS_DIR);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut records = Vec::new();
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension() != Some(OsStr::new("json")) {
            continue;
        }
        match std::fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
        {
            Ok(r) => records.push(r),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "coordinator record skipped")
            }
        }
    }
    records.sort_by(|a: &CoordinatorRecord, b| a.project_id.cmp(&b.project_id));
    Ok(records)
}

async fn git<S: AsRef<OsStr>>(dir: Option<&Path>, args: &[S]) -> Result<String, EngineError> {
    let mut cmd = tokio::process::Command::new("git");
    if let Some(d) = dir {
        cmd.arg("-C").arg(d);
    }
    let out = cmd
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
    }
    let what: Vec<_> = args
        .iter()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .collect();
    Err(EngineError::Command(format!(
        "git {}: {}",
        what.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    )))
}

/// Clone `from` into `dest` through a temporary sibling, so a failed clone
/// leaves nothing behind for the retry. With `origin`, the clone's `origin`
/// is set to it.
async fn clone(
    from: impl AsRef<OsStr>,
    dest: &Path,
    origin: Option<&str>,
) -> Result<(), EngineError> {
    let parent = dest
        .parent()
        .ok_or_else(|| EngineError::Invalid(format!("{} has no parent", dest.display())))?;
    std::fs::create_dir_all(parent)?;
    let name = dest.file_name().unwrap_or_default().to_string_lossy();
    let tmp = parent.join(format!(".{name}.cloning"));
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp)?;
    }
    let args = [
        OsStr::new("clone"),
        OsStr::new("--quiet"),
        OsStr::new("--"),
        from.as_ref(),
        tmp.as_os_str(),
    ];
    if let Err(e) = git(None, &args).await {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    if let Some(url) = origin {
        git(Some(&tmp), &["remote", "set-url", "origin", url]).await?;
    }
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

/// A clone already at `dest` must be of `source`.
async fn check_origin(dest: &Path, source: &SourceRepo) -> Result<(), EngineError> {
    let origin = git(Some(dest), &["remote", "get-url", "origin"]).await?;
    if origin == source.url {
        Ok(())
    } else {
        Err(EngineError::Invalid(format!(
            "repo {} is already cloned from {origin}, not {}",
            source.name, source.url
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::store::Store;

    fn run(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    /// A repo with one commit and an `AGENTS.md`, to clone from.
    fn upstream(dir: &Path, name: &str) -> String {
        let repo = dir.join("upstream").join(name);
        std::fs::create_dir_all(&repo).unwrap();
        run(&repo, &["init", "--quiet", "-b", "main"]);
        std::fs::write(
            repo.join("AGENTS.md"),
            format!("Work on {name} carefully.\n"),
        )
        .unwrap();
        run(&repo, &["add", "."]);
        run(
            &repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "--quiet",
                "-m",
                "init",
            ],
        );
        repo.to_string_lossy().into_owned()
    }

    fn source(name: &str, url: &str) -> SourceRepo {
        SourceRepo {
            name: name.into(),
            url: url.into(),
        }
    }

    fn setup(dir: &Path, sessions: Sessions) -> ProjectSetup {
        ProjectSetup::new(
            sessions,
            Layout::new(dir.join("home")),
            "http://127.0.0.1:7777",
        )
        .with_roots(SessionRoots {
            claude: vec![dir.join("claude")],
            ..Default::default()
        })
    }

    fn agent(harness: &str) -> AgentConfig {
        AgentConfig {
            harness: harness.into(),
            model: Some("opus".into()),
            effort: Some("high".into()),
            pool: None,
        }
    }

    fn tmux_installed() -> bool {
        std::process::Command::new("tmux")
            .arg("-V")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    #[tokio::test]
    async fn adds_and_seeds_repos_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let setup = setup(dir.path(), Sessions::disabled("no tmux in this test"));
        let command = dir.path().join("command");
        let quark = upstream(dir.path(), "quark");
        let tools = upstream(dir.path(), "tools");

        setup
            .add_source(&command, &source("quark", &quark))
            .await
            .unwrap();
        assert!(command.join("repos/quark/AGENTS.md").is_file());
        setup
            .add_source(&command, &source("quark", &quark))
            .await
            .unwrap();
        let err = setup
            .add_source(&command, &source("quark", &tools))
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::Invalid(_)), "{err}");
        assert!(setup
            .add_source(&command, &source("../x", &quark))
            .await
            .is_err());

        let root = dir.path().join("workspaces/p1");
        let plan = WorkspacePlan {
            project_id: "p1".into(),
            name: "P".into(),
            goal: None,
            // `tools` was never added: it is cloned from its URL.
            sources: vec![source("quark", &quark), source("tools", &tools)],
            root: root.clone(),
            user_memory: None,
        };
        assert_eq!(setup.seed_workspace(&command, &plan).await.unwrap(), root);
        for name in ["quark", "tools"] {
            assert!(root.join("repos").join(name).join("AGENTS.md").is_file());
        }
        // Seeded from the command center's copy, but its origin is the URL.
        let origin = git(
            Some(&root.join("repos/quark")),
            &["remote", "get-url", "origin"],
        )
        .await
        .unwrap();
        assert_eq!(origin, quark);
        assert!(!root.join("projects").exists() && !root.join("data").exists());

        // Again for the same Project; refused for another, or a foreign dir.
        assert_eq!(setup.seed_workspace(&command, &plan).await.unwrap(), root);
        let other = WorkspacePlan {
            project_id: "p2".into(),
            ..plan.clone()
        };
        assert!(setup.seed_workspace(&command, &other).await.is_err());
        let foreign = dir.path().join("foreign");
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(foreign.join("x"), "x").unwrap();
        let foreign_plan = WorkspacePlan {
            root: foreign,
            ..plan
        };
        assert!(setup.seed_workspace(&command, &foreign_plan).await.is_err());
    }

    #[tokio::test]
    async fn refuses_coordinators_it_cannot_wire() {
        let dir = tempfile::tempdir().unwrap();
        let setup = setup(dir.path(), Sessions::disabled("no tmux in this test"));
        let command = dir.path().join("command");
        let ws = WorkspaceRef {
            project_id: "p1".into(),
            root: dir.path().join("ws"),
        };
        std::fs::create_dir_all(&ws.root).unwrap();
        for harness in ["codex", "nope"] {
            let err = setup
                .start_coordinator(&command, &ws, &agent(harness), &[], false)
                .await
                .unwrap_err();
            assert!(matches!(err, EngineError::Invalid(_)), "{err}");
        }
        let missing = WorkspaceRef {
            root: dir.path().join("missing"),
            ..ws
        };
        let err = setup
            .start_coordinator(&command, &missing, &agent("claude"), &[], false)
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::WorkspaceNotFound(_)), "{err}");
        assert!(setup.coordinator_terminals(&command).unwrap().is_empty());
    }

    #[test]
    fn launches_through_sh_with_the_prompt_file() {
        let reg = ManifestRegistry::builtin();
        let m = reg.get(COORDINATOR_HARNESS).unwrap();
        let argv = launch_argv(
            m,
            &agent("claude-code"),
            Path::new("/w"),
            Path::new("/w/.quark/coordinator.md"),
            true,
        )
        .unwrap();
        assert_eq!(&argv[..2], ["/bin/sh", "-c"]);
        assert_eq!(argv[4], "/w/.quark/coordinator.md");
        assert_eq!(
            &argv[5..],
            [
                "claude",
                "--dangerously-skip-permissions",
                "--model",
                "opus",
                "--effort",
                "high",
                "--continue"
            ]
        );
        let mut bad = agent("claude-code");
        bad.effort = Some("ultra".into());
        assert!(launch_argv(m, &bad, Path::new("/w"), Path::new("/p"), false).is_err());

        let env = launch_env(m, &[("CLAUDE_CONFIG_DIR".into(), "/a".into())], "p1");
        assert!(env.contains(&("CLAUDE_CONFIG_DIR".into(), "/a".into())));
        assert!(env.contains(&(PROJECT_ENV.into(), "p1".into())));

        let cut = fit("é".repeat(100), 60);
        assert!(cut.len() <= 60 && cut.ends_with("fit.)\n"));
    }

    /// A Claude Code stand-in that records its arguments, then waits.
    fn fake_claude(dir: &Path) -> Arc<ManifestRegistry> {
        let script = dir.join("fake-claude");
        let out = dir.join("args");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n--\\n' \"$a\"; done >> '{0}'\necho QUARK-FAKE-END >> '{0}'\nexec sleep 600\n",
                out.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut m = (**ManifestRegistry::builtin()
            .get(COORDINATOR_HARNESS)
            .unwrap())
        .clone();
        m.launch.argv = vec![script.to_string_lossy().into_owned()];
        Arc::new(ManifestRegistry::with([m]))
    }

    async fn args(dir: &Path, count: usize) -> String {
        for _ in 0..100 {
            let text = std::fs::read_to_string(dir.join("args")).unwrap_or_default();
            if text.matches("QUARK-FAKE-END\n").count() >= count {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the coordinator never started");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn starts_records_and_resumes_the_coordinator() {
        if !tmux_installed() {
            eprintln!("tmux is not installed; skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open_in_memory().unwrap());
        let sessions = Sessions::new("tmux", dir.path().join("run"), store).unwrap();
        let server = sessions.server().unwrap().clone();
        let setup = setup(dir.path(), sessions).with_manifests(fake_claude(dir.path()));
        let command = dir.path().join("command");
        let ws = WorkspaceRef {
            project_id: "p1".into(),
            root: dir.path().join("ws"),
        };
        std::fs::create_dir_all(ws.root.join("project")).unwrap();
        std::fs::write(ws.root.join("project/instructions.md"), "Ship small PRs.\n").unwrap();
        std::fs::create_dir_all(ws.root.join("repos/quark/.git")).unwrap();
        std::fs::write(ws.root.join("repos/quark/AGENTS.md"), "Run cargo test.\n").unwrap();

        // Nothing to resume yet.
        assert!(setup
            .start_coordinator(&command, &ws, &agent("claude"), &[], true)
            .await
            .is_err());

        let env = [("BEADS_X".to_string(), "1".to_string())];
        setup
            .start_coordinator(&command, &ws, &agent("claude"), &env, false)
            .await
            .unwrap();
        let first = args(dir.path(), 1).await;
        for want in ["--model\n--\nopus", "Ship small PRs.", "Run cargo test."] {
            assert!(first.contains(want), "{want} missing from {first}");
        }
        assert!(!first.contains("--continue"));
        let prompt = std::fs::read_to_string(ws.root.join(".quark/coordinator.md")).unwrap();
        assert!(prompt.contains("Run cargo test."));
        let mcp = std::fs::read_to_string(ws.root.join(".mcp.json")).unwrap();
        assert!(mcp.contains("/v1/coordinator/p1/"), "{mcp}");
        let terminals = setup.coordinator_terminals(&command).unwrap();
        assert_eq!(
            terminals.get("p1").map(String::as_str),
            Some("quark:coordinator-p1")
        );

        // Running already: not started twice.
        setup
            .start_coordinator(&command, &ws, &agent("claude"), &env, false)
            .await
            .unwrap();
        let windows = |panes: Vec<crate::sessions::PaneInfo>| {
            panes
                .into_iter()
                .filter(|p| p.target == "quark:coordinator-p1")
                .count()
        };
        assert_eq!(windows(server.list_panes().await.unwrap()), 1);

        // Its window gone and a conversation logged, it resumes.
        server
            .run(&["kill-window", "-t", "quark:coordinator-p1"])
            .await
            .unwrap();
        let slug: String = ws
            .root
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let logs = dir.path().join("claude/projects").join(slug);
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("s.jsonl"), "{}\n").unwrap();
        setup
            .start_coordinator(&command, &ws, &agent("claude"), &env, true)
            .await
            .unwrap();
        let second = args(dir.path(), 2).await;
        let resumed = &second[first.len()..];
        assert!(resumed.contains("\n--\n--continue\n--\n"), "{resumed}");
        assert_eq!(windows(server.list_panes().await.unwrap()), 1);
        server.kill().await;
    }
}
