//! The native coordinator's routes (slice 6): its tools over MCP and its
//! harness's turn-end hook, nested under [`PREFIX`].
//!
//! - `POST /{project}/{token}/mcp`: MCP streamable HTTP, one JSON-RPC
//!   message per POST, answered by [`CoordinatorMcp`]. `GET` is `405`.
//! - `POST /{project}/{token}/hooks/{event}`: the harness's turn-end hook
//!   (`turn_end`, or Claude Code's `Stop`). It closes the open turn with the
//!   turn's token use: the body's `usage` when the hook sends one, else read
//!   from the coordinator's own session log. An `Idempotency-Key` header
//!   makes a retried hook count once. On Claude Code's `Stop`, a reply that
//!   asks the user something with no `ask_user` call that turn is sent back
//!   once (`{"decision": "block"}`, [`questions::NUDGE`]) so the coordinator
//!   files the questions as decisions; the turn closes when it stops again.
//!
//! The token is minted once per Project and kept in `<home>/coordinators/`
//! (mode `0600`), so only the coordinator quarkd configured can reach its
//! tools. A wrong token is `404`, the same as an unknown Project.
//!
//! [`install`] points a coordinator's harness at those URLs:
//! - Claude Code ([`install_claude`]): the MCP server in `.mcp.json` and the
//!   `Stop` hook in `.claude/settings.local.json`.
//! - Codex ([`codex_overrides`]): `-c` overrides on its command line, the
//!   MCP server under `mcp_servers` and a `notify` program POSTing each
//!   `agent-turn-complete` to `turn_end`. Nothing is written to `~/.codex`
//!   or the workspace, and no Codex hook is used (they ask for trust first).
//! - Pi ([`install_pi`]): Pi has no MCP client, so an extension in
//!   [`PI_EXTENSION`] registers each MCP tool as a Pi tool, forwards its
//!   calls, and POSTs `turn_end` on every `agent_end`.
//! - IBM Bob ([`install_bob`]): the workspace's own Bob settings: the MCP
//!   server in `.bob/mcp.json`, a `Stop` hook POSTing `turn_end` and a
//!   `SessionStart` hook priming Beads in `.bob/settings.json`, and the
//!   [`BOB_MODE`] custom mode the coordinator runs in, whose rules carry its
//!   prompt (Bob has no system-prompt flag).
//! - Antigravity CLI ([`crate::coordinator_agy::install`]): the workspace's
//!   `.agents/`: the MCP server in `mcp_config.json`, and a named hook in
//!   `hooks.json` whose `PreInvocation` asks `hooks/agy_prompt` for the
//!   prompt (agy has no system-prompt flag, so quarkd adds it to the
//!   conversation) and whose `Stop` POSTs `hooks/agy_stop`, which can send
//!   the turn back as Claude Code's `Stop` can.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use quark_coordinator::mcp::CoordinatorMcp;
use quark_coordinator::questions;
use quark_coordinator::{Coordinator, Usage};
use quark_core::ProjectId;
use quark_transcript::{locate, read_turns, SessionFormat, SessionRoots, UsageCursor};
use serde_json::{json, Value};

use crate::chat::CoordinatorInput;
use crate::compaction::{self, Compactor};
use crate::store::Store;

/// Where the routes are nested.
pub const PREFIX: &str = "/v1/coordinator";
/// The MCP server's name in a coordinator's harness config.
pub const SERVER_NAME: &str = "quark-coordinator";
/// Hook events that end a turn.
const TURN_END: &[&str] = &["turn_end", "Stop", crate::coordinator_agy::STOP_EVENT];

/// Each Project coordinator's token, one file per Project.
#[derive(Debug, Clone)]
pub struct Tokens {
    dir: PathBuf,
}

impl Tokens {
    pub fn new(home: &Path) -> Self {
        Self {
            dir: home.join("coordinators"),
        }
    }

    fn path(&self, project: &str) -> Option<PathBuf> {
        // Project ids are path-safe; refuse anything that is not.
        let safe = !project.is_empty()
            && project
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        safe.then(|| self.dir.join(format!("{project}.token")))
    }

    /// The Project's token, minted on first use.
    pub fn get_or_create(&self, project: &str) -> std::io::Result<String> {
        let path = self.path(project).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad Project id")
        })?;
        if let Ok(t) = std::fs::read_to_string(&path) {
            if !t.trim().is_empty() {
                return Ok(t.trim().to_string());
            }
        }
        std::fs::create_dir_all(&self.dir)?;
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let tmp = path.with_extension("tmp");
        {
            use std::io::Write;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
            let mut f = opts.open(&tmp)?;
            f.write_all(token.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &path)?;
        Ok(token)
    }

    /// Whether `token` is the Project's. Never mints one.
    pub fn check(&self, project: &str, token: &str) -> bool {
        let Some(want) = self
            .path(project)
            .and_then(|p| std::fs::read_to_string(p).ok())
        else {
            return false;
        };
        let want = want.trim();
        // Same length, then every byte compared, so timing says nothing.
        !want.is_empty()
            && want.len() == token.len()
            && want
                .bytes()
                .zip(token.bytes())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0
    }
}

/// The coordinator's URLs for one Project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinatorUrls {
    pub mcp: String,
    /// Hook commands POST to `<hooks>/<event>`.
    pub hooks: String,
}

impl CoordinatorUrls {
    /// `base` is quarkd's own address, such as `http://127.0.0.1:7777`.
    pub fn new(base: &str, project: &str, token: &str) -> Self {
        let root = format!("{}{PREFIX}/{project}/{token}", base.trim_end_matches('/'));
        Self {
            mcp: format!("{root}/mcp"),
            hooks: format!("{root}/hooks"),
        }
    }
}

struct Routes {
    coordinator: Arc<Coordinator>,
    mcp: CoordinatorMcp,
    tokens: Tokens,
    store: Arc<Store>,
    roots: SessionRoots,
    /// Where each coordinator session log was last read for turn usage.
    cursors: Mutex<HashMap<PathBuf, UsageCursor>>,
    /// Projects whose open turn was sent back with [`questions::NUDGE`]
    /// from a `turn_end`, which has no `stop_hook_active` of its own.
    nudged: Mutex<std::collections::HashSet<String>>,
    /// The start of each Project's agy coordinator whose first turn was
    /// already sent back with [`crate::coordinator_agy::TOOLS_NOTE`].
    agy_launches: Mutex<HashMap<String, String>>,
    /// What each agy coordinator transcript says about its prompt.
    agy_prompts: Mutex<HashMap<PathBuf, crate::coordinator_agy::PromptState>>,
    wrap_up: Option<WrapUp>,
}

/// What compacting a coordinator takes: a wrap up, then `/compact`.
pub struct WrapUp {
    pub chat: Arc<dyn CoordinatorInput>,
    pub events: quark_eventlog::SqliteEventLog,
    pub compactor: Arc<Compactor>,
}

/// The routes, to nest under [`PREFIX`].
pub fn router(
    coordinator: Arc<Coordinator>,
    tokens: Tokens,
    store: Arc<Store>,
    roots: SessionRoots,
) -> Router {
    routes(coordinator, tokens, store, roots, None)
}

/// The routes, compacting a coordinator when its context fills.
pub fn router_wrapping_up(
    coordinator: Arc<Coordinator>,
    tokens: Tokens,
    store: Arc<Store>,
    roots: SessionRoots,
    wrap_up: WrapUp,
) -> Router {
    routes(coordinator, tokens, store, roots, Some(wrap_up))
}

fn routes(
    coordinator: Arc<Coordinator>,
    tokens: Tokens,
    store: Arc<Store>,
    roots: SessionRoots,
    wrap_up: Option<WrapUp>,
) -> Router {
    let routes = Routes {
        mcp: CoordinatorMcp::new(coordinator.clone()),
        coordinator,
        tokens,
        store,
        roots,
        cursors: Mutex::new(HashMap::new()),
        nudged: Mutex::new(Default::default()),
        agy_prompts: Mutex::new(HashMap::new()),
        agy_launches: Mutex::new(HashMap::new()),
        wrap_up,
    };
    Router::new()
        .route("/{project}/{token}/mcp", post(mcp))
        .route("/{project}/{token}/hooks/{event}", post(hook))
        .with_state(Arc::new(routes))
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response()
}

fn bad(msg: String) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": msg }))).into_response()
}

async fn mcp(
    State(r): State<Arc<Routes>>,
    UrlPath((project, token)): UrlPath<(String, String)>,
    body: Bytes,
) -> Response {
    if !r.tokens.check(&project, &token) {
        return not_found();
    }
    let message: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"jsonrpc": "2.0", "id": null,
                    "error": {"code": -32700, "message": e.to_string()}})),
            )
                .into_response()
        }
    };
    match r.mcp.handle(&ProjectId::new(project), message).await {
        Some(response) => Json(response).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

async fn hook(
    State(r): State<Arc<Routes>>,
    UrlPath((project, token, event)): UrlPath<(String, String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !r.tokens.check(&project, &token) {
        return not_found();
    }
    if event == crate::coordinator_agy::PROMPT_EVENT {
        let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        return Json(r.agy_prompt(&project, &body).await).into_response();
    }
    if !TURN_END.contains(&event.as_str()) {
        return bad(format!("hook {event} is not a turn end"));
    }
    let body: Value = if body.iter().all(u8::is_ascii_whitespace) {
        Value::Null
    } else {
        match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => return bad(format!("hook body: {e}")),
        }
    };
    let source = match headers.get("idempotency-key") {
        None => None,
        Some(v) => match v.to_str() {
            Ok(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
            _ => return bad("Idempotency-Key must be text".into()),
        },
    };
    // A reply that asks the user something without `ask_user` is sent back
    // once, so the coordinator files the questions as decisions. The turn
    // stays open until it ends again. Claude Code's `Stop` takes the answer
    // as its decision; the Pi extension sends `reason` as a follow-up;
    // Codex's `notify` and Bob's `Stop` read no answer, so the nudge is
    // typed in.
    let claude_stop = event == "Stop";
    if claude_stop && r.unfiled_questions(&project, &body).await {
        tracing::info!(%project, "coordinator asked in text; nudged it to ask_user");
        return Json(json!({ "decision": "block", "reason": questions::NUDGE })).into_response();
    }
    // agy's `Stop` sends a turn back with `continue`. Its own
    // `continuationActive` stays false on the stop that follows, so a turn
    // already sent back is remembered here instead.
    let agy_stop = event == crate::coordinator_agy::STOP_EVENT;
    // A turn the person or quarkd interrupted, or that failed, is never
    // sent back.
    let agy_ended_itself = matches!(
        body.get("terminationReason").and_then(Value::as_str),
        None | Some("NO_TOOL_CALL" | "TERMINAL_STEP_TYPE" | "INJECTED_RESPONSE")
    );
    if agy_stop && agy_ended_itself {
        let launch = headers
            .get(crate::coordinator_agy::LAUNCH_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty());
        let fresh = launch.is_some_and(|l| {
            let mut seen = r.agy_launches.lock().unwrap();
            seen.insert(project.clone(), l.to_string()).as_deref() != Some(l)
        });
        if fresh {
            r.nudged.lock().unwrap().insert(project.clone());
            tracing::info!(%project, "sent the agy coordinator's first turn back once its tools connected");
            return Json(json!({
                "decision": "continue",
                "reason": crate::coordinator_agy::TOOLS_NOTE,
            }))
            .into_response();
        }
    }
    if agy_stop
        && agy_ended_itself
        && !r.nudged.lock().unwrap().remove(&project)
        && r.unfiled_questions(&project, &body).await
    {
        r.nudged.lock().unwrap().insert(project.clone());
        tracing::info!(%project, "coordinator asked in text; nudged it to ask_user");
        return Json(json!({ "decision": "continue", "reason": questions::NUDGE })).into_response();
    }
    if !claude_stop && !agy_stop && !r.nudged.lock().unwrap().remove(&project) {
        if let Some(response) = r.nudge_turn_end(&project, &body).await {
            return response;
        }
    }
    let (usage, context) = match body.get("usage") {
        Some(u) => match serde_json::from_value::<Usage>(u.clone()) {
            Ok(u) => (u, None),
            Err(e) => return bad(format!("usage: {e}")),
        },
        None => r.logged_usage(&project).await,
    };
    let id = ProjectId::new(project.clone());
    match r
        .coordinator
        .turn_ended(&id, usage, source.as_deref())
        .await
    {
        Ok(turn) => {
            // A switch waiting on this coordinator's wrap up may go on.
            let (store, pid) = (r.store.clone(), project.clone());
            if let Ok(Err(e)) =
                tokio::task::spawn_blocking(move || store.switch_turn_ended(&pid)).await
            {
                tracing::warn!(%project, error = %e, "could not mark a switch's wrap up");
            }
            if let Some(w) = &r.wrap_up {
                // A message queued while the turn ran goes in now, just
                // after the hook returns.
                let (store, chat, pid) = (r.store.clone(), w.chat.clone(), project.clone());
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    crate::outbox::deliver_next(store, chat, pid).await;
                });
                compaction::after_turn(
                    w.compactor.clone(),
                    r.store.clone(),
                    w.events.clone(),
                    w.chat.clone(),
                    project,
                    context,
                );
            }
            // Claude Code and agy read a `Stop` hook's output as its
            // decision; an empty object lets the session stop.
            if claude_stop || agy_stop {
                return Json(json!({})).into_response();
            }
            Json(json!({ "turn": turn })).into_response()
        }
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

impl Routes {
    /// Send a `turn_end` that asked in text back, as [`hook`] describes.
    /// `None` when the turn should close: no unfiled question, or no way to
    /// reach the coordinator's harness.
    async fn nudge_turn_end(&self, project: &str, body: &Value) -> Option<Response> {
        if !self.unfiled_questions(project, body).await {
            return None;
        }
        let store = self.store.clone();
        let id = project.to_string();
        let p = tokio::task::spawn_blocking(move || store.get_project(&id))
            .await
            .ok()?
            .ok()?;
        let harness = p
            .agent_config
            .as_ref()
            .map(|a| a.harness.as_str())
            .unwrap_or(crate::engine::native_project::COORDINATOR_HARNESS);
        match Wiring::for_harness(harness)? {
            Wiring::Pi => {}
            // Bob's `Stop` hook cannot send the turn back either. Kiro's
            // can, but only once per prompt: the turn it sends back would
            // end without another `Stop`, so the nudge goes in as a prompt.
            Wiring::Codex | Wiring::Bob | Wiring::Kiro => {
                let chat = self.wrap_up.as_ref()?.chat.clone();
                let ws = crate::engine::WorkspaceRef {
                    project_id: p.id.clone(),
                    root: PathBuf::from(p.workspace_path?),
                };
                let pid = project.to_string();
                tokio::spawn(async move {
                    if let Err(e) = chat.send(&ws, questions::NUDGE).await {
                        tracing::warn!(project = %pid, error = %e, "could not nudge the coordinator to ask_user");
                    }
                });
            }
            Wiring::Claude | Wiring::Antigravity => return None,
        }
        self.nudged.lock().unwrap().insert(project.to_string());
        tracing::info!(%project, "coordinator asked in text; nudged it to ask_user");
        Some(Json(json!({ "decision": "block", "reason": questions::NUDGE })).into_response())
    }

    /// Whether the turn a hook ends asked the user something in text with
    /// no `ask_user` call since the last turn end.
    /// Never on a stop that a nudge already caused (`stop_hook_active`).
    async fn unfiled_questions(&self, project: &str, body: &Value) -> bool {
        if body.get("stop_hook_active").and_then(Value::as_bool) == Some(true) {
            return false;
        }
        // Claude Code and the Pi extension send `last_assistant_message`;
        // Codex's notify, `last-assistant-message`.
        let said = body
            .get("last_assistant_message")
            .or_else(|| body.get("last-assistant-message"))
            .and_then(Value::as_str);
        // agy's payload names its transcript in camelCase.
        let agy_log = body.get("transcriptPath").and_then(Value::as_str);
        let text = match said {
            Some(t) => t.to_string(),
            None if agy_log.is_some() => {
                let path = PathBuf::from(agy_log.unwrap_or_default());
                tokio::task::spawn_blocking(move || {
                    last_entry_reply(&path, SessionFormat::Antigravity)
                })
                .await
                .ok()
                .flatten()
                .unwrap_or_default()
            }
            None => match body.get("transcript_path").and_then(Value::as_str) {
                Some(path) => {
                    let path = PathBuf::from(path);
                    tokio::task::spawn_blocking(move || last_reply(&path))
                        .await
                        .ok()
                        .flatten()
                        .unwrap_or_default()
                }
                // Kiro's `Stop` payload has neither: its session log has
                // the reply.
                None => match self.kiro_reply(project).await {
                    Some(t) => t,
                    None => return false,
                },
            },
        };
        if questions::questions(&text).is_empty() {
            return false;
        }
        !self
            .coordinator
            .asked_this_turn(&ProjectId::new(project.to_string()))
            .await
    }

    /// What an agy coordinator's `PreInvocation` hook prints: its prompt,
    /// as [`crate::coordinator_agy::injection`], when the conversation does
    /// not hold the current one (a new conversation, a compaction since it
    /// was added, or a prompt file that changed); otherwise `{}`.
    async fn agy_prompt(&self, project: &str, body: &Value) -> Value {
        use crate::coordinator_agy::{digest, injection, PromptState};
        use crate::engine::native_project::{PROMPT_FILE, STATE_DIR};
        let store = self.store.clone();
        let id = project.to_string();
        let Ok(Ok(p)) = tokio::task::spawn_blocking(move || store.get_project(&id)).await else {
            return json!({});
        };
        let Some(root) = p.workspace_path.map(PathBuf::from) else {
            return json!({});
        };
        let Ok(text) = std::fs::read_to_string(root.join(STATE_DIR).join(PROMPT_FILE)) else {
            tracing::warn!(%project, "the agy coordinator's prompt file is missing");
            return json!({});
        };
        let want = digest(&text);
        // The transcript agy names, else the newest for the workspace.
        let log = body
            .get("transcriptPath")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .or_else(|| locate(SessionFormat::Antigravity, &root, &self.roots));
        if let Some(log) = log {
            let mut state = self
                .agy_prompts
                .lock()
                .unwrap()
                .get(&log)
                .cloned()
                .unwrap_or_default();
            let path = log.clone();
            let read = tokio::task::spawn_blocking(move || {
                // A conversation's first call comes before agy writes it.
                if path.is_file() {
                    state.scan(&path)?;
                }
                Ok::<_, std::io::Error>(state)
            })
            .await;
            let mut state = match read {
                Ok(Ok(s)) => s,
                _ => PromptState::default(),
            };
            let holds = state.holds.as_deref() == Some(want.as_str());
            if !holds {
                // Added now; the transcript shows it from the next call on.
                state.holds = Some(want.clone());
            }
            self.agy_prompts.lock().unwrap().insert(log, state);
            if holds {
                return json!({});
            }
        }
        tracing::info!(%project, bytes = text.len(), "added the prompt to the agy coordinator's conversation");
        injection(&text, &want)
    }

    /// The last thing the Project's coordinator said, from its newest Kiro
    /// CLI session log, when it runs on Kiro. `None` otherwise.
    async fn kiro_reply(&self, project: &str) -> Option<String> {
        let store = self.store.clone();
        let id = project.to_string();
        let p = tokio::task::spawn_blocking(move || store.get_project(&id))
            .await
            .ok()?
            .ok()?;
        let harness = p.agent_config.as_ref()?.harness.clone();
        if Wiring::for_harness(crate::engine::engine_harness(&harness)) != Some(Wiring::Kiro) {
            return None;
        }
        let root = PathBuf::from(p.workspace_path?);
        let roots = self.roots.clone();
        tokio::task::spawn_blocking(move || {
            let log = locate(SessionFormat::Kiro, &root, &roots)?;
            last_entry_reply(&log, SessionFormat::Kiro)
        })
        .await
        .ok()
        .flatten()
    }

    /// The token use logged by the coordinator since its last turn end,
    /// from its newest session log in the Project workspace, and the
    /// context its last turn ended on. Zero when there is no log to read.
    /// The first read of a log counts only its last turn, not the whole
    /// history.
    async fn logged_usage(&self, project: &str) -> (Usage, Option<u64>) {
        let store = self.store.clone();
        let id = project.to_string();
        let Ok(Ok(p)) = tokio::task::spawn_blocking(move || store.get_project(&id)).await else {
            return (Usage::default(), None);
        };
        let Some(root) = p.workspace_path.map(PathBuf::from) else {
            return (Usage::default(), None);
        };
        // Bob writes no session log; its database has the context (and no
        // per-turn token counts).
        if p.agent_config
            .as_ref()
            .is_some_and(|a| crate::engine::engine_harness(&a.harness) == "bob")
        {
            let db = crate::transcripts::default_bob_db();
            let context = tokio::task::spawn_blocking(move || {
                quark_dispatch::local::bob::conversation(&db, &root).and_then(|c| c.context_tokens)
            })
            .await
            .ok()
            .flatten();
            return (Usage::default(), context);
        }
        let roots = self.roots.clone();
        let Ok(Some((path, format))) =
            tokio::task::spawn_blocking(move || newest_log(&roots, &root)).await
        else {
            return (Usage::default(), None);
        };
        let known = self.cursors.lock().unwrap().get(&path).cloned();
        let first = known.is_none();
        let cursor = known.unwrap_or_default();
        let p = path.clone();
        let read = tokio::task::spawn_blocking(move || read_turns(&p, &cursor, format, true)).await;
        let Ok(Ok((turns, next))) = read else {
            return (Usage::default(), None);
        };
        self.cursors.lock().unwrap().insert(path, next);
        let context = turns.last().and_then(|t| t.context);
        let counted = if first {
            &turns[turns.len().saturating_sub(1)..]
        } else {
            &turns[..]
        };
        let mut usage = Usage::default();
        for m in counted.iter().flat_map(|t| &t.models) {
            usage.add(&Usage {
                input: m.input,
                output: m.output,
                cache_read: m.cache_read,
                calls: m.calls,
            });
        }
        (usage, context)
    }
}

/// The text of the last assistant message in a Claude Code session log,
/// read from its tail. `None` when the file is not a readable `.jsonl`.
fn last_reply(path: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL: u64 = 256 * 1024;
    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
        return None;
    }
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    text.lines().rev().find_map(|line| {
        let v: Value = serde_json::from_str(line).ok()?;
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            return None;
        }
        let content = v.get("message")?.get("content")?;
        let said = match content {
            Value::String(s) => s.clone(),
            Value::Array(blocks) => blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        (!said.trim().is_empty()).then_some(said)
    })
}

/// The text of the last assistant reply in a session log of `format`,
/// read from its tail. A reply that streamed in several entries after the
/// last prompt is joined.
fn last_entry_reply(path: &Path, format: SessionFormat) -> Option<String> {
    use quark_transcript::TranscriptRole;
    use std::io::{Read, Seek, SeekFrom};
    const TAIL: u64 = 256 * 1024;
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let mut said: Vec<String> = Vec::new();
    for line in text.lines() {
        for e in format.parse_line(line) {
            match e.role {
                TranscriptRole::User => said.clear(),
                TranscriptRole::Assistant if !e.text.trim().is_empty() => said.push(e.text),
                _ => {}
            }
        }
    }
    // Only the reply that ended the turn: the text after the turn's last
    // tool call is what the person reads.
    said.pop()
}

fn newest_log(roots: &SessionRoots, cwd: &Path) -> Option<(PathBuf, SessionFormat)> {
    SessionFormat::COORDINATOR
        .iter()
        .filter_map(|&f| locate(f, cwd, roots).map(|p| (p, f)))
        .max_by_key(|(p, _)| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
        })
}

/// Marks Quark's `bd prime` hook, so a later pass replaces its own.
const PRIME_MARK: &str = "# quark:bd-prime";

/// Injects the Project's Beads memories at session start. `BEADS_DIR` is in
/// the coordinator's environment; without `bd`, or without a database yet,
/// it prints nothing.
const PRIME_HOOK: &str = "bd prime --memories-only 2>/dev/null || true # quark:bd-prime";

/// Configure the Claude Code coordinator running in `root` to reach quarkd:
/// [`SERVER_NAME`] as an approved HTTP MCP server in `.mcp.json`, and a
/// `Stop` hook POSTing to the turn-end URL in `.claude/settings.local.json`,
/// beside a `SessionStart` hook that injects the Project's Beads memories.
/// Everything else in both files is kept, and a previous Quark hook is
/// replaced, so it is safe to run on every pass. Returns whether a file
/// changed.
pub fn install_claude(root: &Path, urls: &CoordinatorUrls) -> std::io::Result<bool> {
    let mut changed = edit_json(&root.join(".mcp.json"), |v| {
        v["mcpServers"][SERVER_NAME] = json!({ "type": "http", "url": urls.mcp });
    })?;
    let command = format!(
        "curl -fsS -m 10 -X POST -H 'Content-Type: application/json' --data-binary @- '{}/Stop' 2>/dev/null || true",
        urls.hooks
    );
    changed |= edit_json(&root.join(".claude/settings.local.json"), |v| {
        // A suggested next prompt would sit in the coordinator's input as if
        // someone had typed it, and nobody types there but the engine.
        v["promptSuggestionEnabled"] = json!(false);
        // Claude Code's SendFeedback tool queues a bug report for review with
        // its own /feedback command, which Quark's chat does not have, so the
        // coordinator would point the user at a command they cannot run.
        v["feedbackDrafts"] = json!("off");
        let approved = &mut v["enabledMcpjsonServers"];
        if !approved.is_array() {
            *approved = json!([]);
        }
        let list = approved.as_array_mut().expect("array");
        if !list.iter().any(|s| s == SERVER_NAME) {
            list.push(json!(SERVER_NAME));
        }
        let stop = &mut v["hooks"]["Stop"];
        if !stop.is_array() {
            *stop = json!([]);
        }
        let groups = stop.as_array_mut().expect("array");
        groups.retain(|g| !g.to_string().contains(&format!("{PREFIX}/")));
        groups.push(json!({ "hooks": [{ "type": "command", "command": command }] }));
        // Beads' own session-start hook, as `bd setup` would add it: the
        // Project's memories (standing rules among them) reach the
        // coordinator when its session starts, resumes or is compacted. Only
        // the memories: how a coordinator uses Beads is in its prompt.
        let start = &mut v["hooks"]["SessionStart"];
        if !start.is_array() {
            *start = json!([]);
        }
        let groups = start.as_array_mut().expect("array");
        groups.retain(|g| !g.to_string().contains(PRIME_MARK));
        groups.push(json!({ "hooks": [{ "type": "command", "command": PRIME_HOOK }] }));
    })?;
    Ok(changed)
}

/// How quarkd wires a coordinator harness to its tools and turn end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wiring {
    Claude,
    Codex,
    Pi,
    Bob,
    Kiro,
    Antigravity,
}

impl Wiring {
    /// The wiring for a harness manifest id; `None` for a harness that
    /// cannot be a coordinator yet.
    pub fn for_harness(id: &str) -> Option<Wiring> {
        match id {
            "claude-code" | "claude" => Some(Wiring::Claude),
            "codex" => Some(Wiring::Codex),
            "pi" | "pi-signed" => Some(Wiring::Pi),
            "bob" => Some(Wiring::Bob),
            "kiro" => Some(Wiring::Kiro),
            "antigravity" | "agy" => Some(Wiring::Antigravity),
            _ => None,
        }
    }
}

/// The harnesses a native coordinator runs on, by manifest id.
pub const COORDINATOR_HARNESSES: &[&str] = &[
    "claude-code",
    "codex",
    "pi",
    "pi-signed",
    "bob",
    "kiro",
    "antigravity",
];

/// Point the coordinator running `harness` in `root` at quarkd. Codex takes
/// its wiring on the command line ([`codex_overrides`]), so nothing is
/// written for it. Returns whether a file changed.
pub fn install(harness: &str, root: &Path, urls: &CoordinatorUrls) -> std::io::Result<bool> {
    match Wiring::for_harness(harness) {
        Some(Wiring::Claude) => install_claude(root, urls),
        Some(Wiring::Pi) => install_pi(root, urls),
        Some(Wiring::Bob) => install_bob(root, urls),
        Some(Wiring::Kiro) => install_kiro(root, urls),
        Some(Wiring::Antigravity) => crate::coordinator_agy::install(root, urls),
        Some(Wiring::Codex) | None => Ok(false),
    }
}

/// The Bob custom mode a coordinator runs in (`bob chat --mode`).
pub const BOB_MODE: &str = "quark-coordinator";
/// The coordinator's prompt as Bob reads it: a rules file of
/// [`BOB_MODE`], relative to the workspace root. Bob adds a mode's rules
/// to its system prompt, beside its own.
pub const BOB_PROMPT: &str = ".bob/rules-quark-coordinator/coordinator.md";
/// Bob's workspace custom modes, relative to the workspace root.
const BOB_MODES: &str = ".bob/custom_modes.yaml";

/// Configure the IBM Bob coordinator running in `root` to reach quarkd,
/// through the workspace's own Bob settings, so `~/.bob` is left alone:
/// - `.bob/mcp.json`: [`SERVER_NAME`] as a streamable HTTP server whose
///   tools need no approval (`alwaysAllow`) and belong to [`BOB_MODE`]
///   alone (`groups`). The mode leaves out Bob's `mcp` group, so the
///   user's global MCP servers, whose tools are all in it, stay out of the
///   coordinator's tools and context.
/// - `.bob/settings.json`: a `Stop` hook POSTing Bob's payload (which
///   carries `last_assistant_message`) to `turn_end`, and a `SessionStart`
///   hook whose output, the Project's Beads memories, Bob adds to the
///   model's context. Bob's hook schema is strict (an entry holds only
///   `matcher` and `hooks`; a command only `type`, `command`, `timeout`,
///   `disabled`), and one bad entry makes it ignore every hook in the file.
/// - `.bob/custom_modes.yaml`: [`BOB_MODE`], whose rules ([`BOB_PROMPT`],
///   written with the prompt) carry the coordinator's instructions.
///
/// Everything else in those files is kept, and Quark's earlier entries are
/// replaced. Returns whether a file changed.
pub fn install_bob(root: &Path, urls: &CoordinatorUrls) -> std::io::Result<bool> {
    let tools: Vec<&str> = quark_coordinator::tools::catalog()
        .iter()
        .map(|t| t.name)
        .collect();
    let mut changed = edit_json(&root.join(".bob/mcp.json"), |v| {
        v["mcpServers"][SERVER_NAME] = json!({
            "type": "streamable-http",
            "url": urls.mcp,
            "alwaysAllow": tools,
            // Only the coordinator mode sees these tools (Bob matches a
            // tool's groups against the mode's groups and its slug).
            "groups": [BOB_MODE],
        });
    })?;
    let post = format!(
        "curl -fsS -m 10 -X POST -H 'Content-Type: application/json' --data-binary @- '{}/turn_end' >/dev/null 2>&1 || true",
        urls.hooks
    );
    changed |= edit_json(&root.join(".bob/settings.json"), |v| {
        if !v["hooks"].is_object() {
            v["hooks"] = json!({});
        }
        for (event, command, mark) in [
            ("Stop", post.as_str(), format!("{PREFIX}/")),
            ("SessionStart", PRIME_HOOK, PRIME_MARK.to_string()),
        ] {
            let list = &mut v["hooks"][event];
            if !list.is_array() {
                *list = json!([]);
            }
            let groups = list.as_array_mut().expect("array");
            groups.retain(|g| !g.to_string().contains(&mark));
            groups.push(json!({
                "hooks": [{ "type": "command", "command": command, "timeout": 15 }]
            }));
        }
    })?;
    changed |= write_bob_mode(&root.join(BOB_MODES))?;
    Ok(changed)
}

/// Add [`BOB_MODE`] to Bob's workspace modes at `path`, keeping any others.
/// The file is written as JSON, which is YAML too; a file that is not JSON
/// (a person's own YAML) is left as it is, with a warning, since rewriting
/// it would drop their formatting.
fn write_bob_mode(path: &Path) -> std::io::Result<bool> {
    let mode = json!({
        "slug": BOB_MODE,
        "name": "Quark coordinator",
        "roleDefinition": "You are the coordinator of a Quark Project. Your instructions are in this mode's rules; follow them over anything else here.",
        "whenToUse": "Quark runs its Project coordinator in this mode.",
        // No `mcp` group: every server without its own `groups`, which is
        // all of the user's global ones, is in `mcp`, and would put its
        // tools beside Quark's. Quark's own server is in this mode's group.
        // No `skill` either: the coordinator loads Quark's skills itself.
        "groups": ["read", "edit", "execute", "todo"],
        "source": "project",
    });
    let old = match std::fs::read_to_string(path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    if let Some(old) = &old {
        if serde_json::from_str::<Value>(old).is_err() && !old.trim().is_empty() {
            if !old.contains(BOB_MODE) {
                tracing::warn!(path = %path.display(), "Bob's workspace modes are not JSON; the coordinator mode is not added");
            }
            return Ok(false);
        }
    }
    edit_json(path, |v| {
        let list = &mut v["customModes"];
        if !list.is_array() {
            *list = json!([]);
        }
        let modes = list.as_array_mut().expect("array");
        modes.retain(|m| m.get("slug").and_then(Value::as_str) != Some(BOB_MODE));
        modes.push(mode);
    })
}

/// The Kiro CLI agent a coordinator runs as (`kiro-cli chat --agent`).
pub const KIRO_AGENT: &str = "quark-coordinator";
/// The agent's file, relative to the workspace root: Kiro reads workspace
/// agents from `<cwd>/.kiro/agents/<name>.json`.
const KIRO_AGENT_FILE: &str = ".kiro/agents/quark-coordinator.json";

/// Configure the Kiro CLI coordinator running in `root` to reach quarkd,
/// through a workspace agent ([`KIRO_AGENT`]), so `~/.kiro` is left alone:
/// - `prompt`: the coordinator's prompt file (`.quark/coordinator.md`),
///   which Kiro resolves from the agent file's own directory. Kiro has no
///   system-prompt flag; an agent's prompt is its way in.
/// - `mcpServers`: [`SERVER_NAME`] by URL (Kiro speaks streamable HTTP to
///   a remote server), its tools allowed without asking
///   (`permissions.rules`; Kiro's V3 engine skips a JSON agent that has
///   `allowedTools` and no `permissions`). `includeMcpJson` is off, which
///   keeps the workspace's `mcp.json` out; Kiro 3.5's interactive chat
///   still loads the user's global servers (`~/.kiro/settings/mcp.json`).
/// - `hooks.stop`: POSTs Kiro's `Stop` payload to `turn_end`. Its output is
///   dropped, since Kiro would take a printed block decision as "keep
///   going" and allows only one per prompt.
///
/// The Project's memories are in the prompt, as for Codex and Pi: Kiro
/// does not run its session-start hook again on resume. Kiro's global hooks
/// and steering (`~/.kiro/hooks`, `~/.kiro/steering`) still load; an agent
/// cannot leave them out. Returns whether the file changed.
pub fn install_kiro(root: &Path, urls: &CoordinatorUrls) -> std::io::Result<bool> {
    let post = format!(
        "curl -fsS -m 10 -X POST -H 'Content-Type: application/json' --data-binary @- '{}/turn_end' >/dev/null 2>&1 || true",
        urls.hooks
    );
    edit_json(&root.join(KIRO_AGENT_FILE), |v| {
        *v = json!({
            "name": KIRO_AGENT,
            "description": "The coordinator of a Quark Project. Quark starts and wires it; edits here are replaced.",
            // From `.kiro/agents/`, the workspace's `.quark/coordinator.md`.
            "prompt": "file://../../.quark/coordinator.md",
            "tools": "*",
            "includeMcpJson": false,
            "permissions": { "rules": [{
                "capability": "mcp",
                "match": [format!("{SERVER_NAME}/*")],
                "effect": "allow",
            }]},
            "hooks": { "stop": [{ "command": post }] },
        });
        v["mcpServers"][SERVER_NAME] = json!({ "url": urls.mcp });
    })
}

/// The Pi coordinator's extension, relative to the workspace root.
pub const PI_EXTENSION: &str = ".quark/pi/quark-coordinator.ts";
const PI_TEMPLATE: &str = include_str!("coordinator_pi.ts");

/// Write the Pi extension bridging the coordinator's MCP tools and turn-end
/// hook into `root`, at [`PI_EXTENSION`]. Returns whether it changed.
pub fn install_pi(root: &Path, urls: &CoordinatorUrls) -> std::io::Result<bool> {
    let text = PI_TEMPLATE
        .replace("__QUARK_MCP_URL__", &json!(urls.mcp).to_string())
        .replace("__QUARK_HOOKS_URL__", &json!(urls.hooks).to_string());
    let path = root.join(PI_EXTENSION);
    if std::fs::read_to_string(&path).is_ok_and(|old| old == text) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("quark-tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

/// Codex's `-c` overrides wiring a coordinator to quarkd: [`SERVER_NAME`]
/// as a streamable HTTP MCP server, and a `notify` program that POSTs the
/// JSON Codex appends to it (`agent-turn-complete`) to `turn_end`. Each
/// value is TOML; a JSON string or array of strings is one.
pub fn codex_overrides(urls: &CoordinatorUrls) -> Vec<String> {
    let post = format!(
        "curl -fsS -m 10 -X POST -H 'Content-Type: application/json' --data-binary \"$1\" '{}/turn_end' >/dev/null 2>&1 || true",
        urls.hooks
    );
    let notify = json!(["/bin/sh", "-c", post, "quark-notify"]);
    vec![
        "-c".into(),
        format!("mcp_servers.{SERVER_NAME}.url={}", json!(urls.mcp)),
        "-c".into(),
        format!("notify={notify}"),
    ]
}

/// Apply `edit` to the JSON object at `path` (an empty object when the file
/// is missing), writing it back only when it changed.
pub(crate) fn edit_json(path: &Path, edit: impl FnOnce(&mut Value)) -> std::io::Result<bool> {
    let old = match std::fs::read_to_string(path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let mut v: Value = match &old {
        Some(s) => serde_json::from_str(s).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{}: {e}", path.display()),
            )
        })?,
        None => json!({}),
    };
    if !v.is_object() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} is not a JSON object", path.display()),
        ));
    }
    let before = v.clone();
    edit(&mut v);
    if old.is_some() && v == before {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(&v)? + "\n";
    let tmp = path.with_extension("quark-tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use quark_coordinator::{Config, NoBrain, NoHands};
    use quark_core::{EventLog, Seq};
    use quark_systems::CreateProject;
    use tower::ServiceExt;

    use super::*;

    async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
        let res = app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    fn post(uri: &str, body: &str) -> Request<Body> {
        Request::post(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[test]
    fn tokens_are_minted_once_and_checked() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = Tokens::new(dir.path());
        assert!(!tokens.check("p1", "x"));
        let t = tokens.get_or_create("p1").unwrap();
        assert_eq!(t.len(), 64);
        assert_eq!(tokens.get_or_create("p1").unwrap(), t);
        assert!(tokens.check("p1", &t));
        assert!(!tokens.check("p1", &t[1..]));
        assert!(!tokens.check("p2", &t));
        assert!(tokens.get_or_create("../p1").is_err());
        assert!(!tokens.check("../p1", &t));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("coordinators/p1.token"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[tokio::test]
    async fn routes_check_the_token_and_close_turns() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open_in_memory().unwrap());
        let project = store
            .create_project(CreateProject {
                name: "Quark".into(),
                goal: None,
                workspace_path: Some(dir.path().join("ws").display().to_string()),
                repos: vec![],
                agent_config: None,
                dispatch_preset: None,
                delivery: None,
            })
            .unwrap();
        let log = quark_eventlog::SqliteEventLog::open(dir.path().join("events.db")).unwrap();
        let coordinator = Arc::new(
            Coordinator::open(
                Arc::new(log.clone()),
                quark_core::HostId::from("h"),
                Config::default(),
                Arc::new(NoBrain),
                Arc::new(NoHands),
            )
            .await
            .unwrap(),
        );
        let tokens = Tokens::new(dir.path());
        let token = tokens.get_or_create(&project.id).unwrap();
        let app = router(coordinator, tokens, store, SessionRoots::default());
        let base = format!("/{}/{token}", project.id);

        let list = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let (s, b) = send(&app, post(&format!("{base}/mcp"), list)).await;
        assert_eq!(s, StatusCode::OK);
        assert!(b["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "ask_user"));
        let (s, _) = send(&app, post(&format!("/{}/nope/mcp", project.id), list)).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        // Shadow mode refuses to act, as a tool result.
        let call = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"remember","arguments":{"fact":"hi"}}}"#;
        let (_, b) = send(&app, post(&format!("{base}/mcp"), call)).await;
        assert_eq!(b["result"]["isError"], true);

        let hook = |key: &str, body: &str| {
            Request::post(format!("{base}/hooks/Stop"))
                .header("idempotency-key", key)
                .body(Body::from(body.to_string()))
                .unwrap()
        };
        let usage = r#"{"usage": {"input": 100, "output": 20, "calls": 2}}"#;
        assert_eq!(send(&app, hook("a", usage)).await.0, StatusCode::OK);
        assert_eq!(send(&app, hook("a", usage)).await.0, StatusCode::OK);
        assert_eq!(send(&app, hook("b", "")).await.0, StatusCode::OK);
        let (s, _) = send(&app, post(&format!("{base}/hooks/PreCompact"), "")).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let ended: Vec<Value> = log
            .read(Seq::ZERO, 100)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.kind.as_str() == "coordinator.turn_ended")
            .map(|e| e.payload)
            .collect();
        assert_eq!(ended.len(), 2, "a retried hook counts once");
        assert_eq!(ended[0]["usage"]["input"], 100);
    }

    /// Hands that carry out every call.
    struct YesHands;

    #[async_trait::async_trait]
    impl quark_coordinator::Hands for YesHands {
        async fn run(
            &self,
            _: &ProjectId,
            _: &quark_coordinator::ToolCall,
        ) -> quark_core::Result<String> {
            Ok("done".into())
        }
    }

    /// Records what the engine types to a coordinator.
    #[derive(Default)]
    struct Typed(Mutex<Vec<String>>);

    #[async_trait::async_trait]
    impl CoordinatorInput for Typed {
        async fn send(
            &self,
            _ws: &crate::engine::WorkspaceRef,
            text: &str,
        ) -> Result<crate::chat::Delivery, crate::chat::ChatError> {
            self.0.lock().unwrap().push(text.to_string());
            Ok(crate::chat::Delivery::Confirmed)
        }

        async fn interrupt(
            &self,
            _ws: &crate::engine::WorkspaceRef,
        ) -> Result<(), crate::chat::ChatError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn codex_and_pi_turns_that_ask_in_text_are_sent_back_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open_in_memory().unwrap());
        let mut ids = HashMap::new();
        for harness in ["codex", "pi"] {
            let p = store
                .create_project(CreateProject {
                    name: harness.into(),
                    goal: None,
                    workspace_path: Some(dir.path().join(harness).display().to_string()),
                    repos: vec![],
                    agent_config: Some(quark_systems::AgentConfig {
                        harness: harness.into(),
                        model: None,
                        effort: None,
                        pool: None,
                    }),
                    dispatch_preset: None,
                    delivery: None,
                })
                .unwrap();
            ids.insert(harness, p.id);
        }
        let log = quark_eventlog::SqliteEventLog::open(dir.path().join("events.db")).unwrap();
        let coordinator = Arc::new(
            Coordinator::open(
                Arc::new(log.clone()),
                quark_core::HostId::from("h"),
                Config::default(),
                Arc::new(NoBrain),
                Arc::new(YesHands),
            )
            .await
            .unwrap(),
        );
        let tokens = Tokens::new(dir.path());
        let typed = Arc::new(Typed::default());
        let off = quark_systems::CompactionSettings {
            auto: false,
            at_percent: 70,
        };
        let app = router_wrapping_up(
            coordinator,
            tokens.clone(),
            store,
            SessionRoots::default(),
            WrapUp {
                chat: typed.clone(),
                events: log.clone(),
                compactor: Arc::new(Compactor::new(off, SessionRoots::default())),
            },
        );
        let ended = || async {
            log.read(Seq::ZERO, 100)
                .await
                .unwrap()
                .iter()
                .filter(|e| e.kind.as_str() == "coordinator.turn_ended")
                .count()
        };
        let asking = "Plan is ready. Should I use Redis or keep it in memory?";

        // Pi: the extension sends the reply back as a follow-up.
        let pi = &ids["pi"];
        let url = format!("/{pi}/{}/hooks/turn_end", tokens.get_or_create(pi).unwrap());
        let body = json!({"last_assistant_message": asking}).to_string();
        let (_, b) = send(&app, post(&url, &body)).await;
        assert_eq!(b["decision"], "block");
        assert_eq!(b["reason"], questions::NUDGE);
        assert_eq!(ended().await, 0, "the turn stays open");
        // The next turn end closes it, whatever it said.
        let (_, b) = send(&app, post(&url, &body)).await;
        assert!(b.get("turn").is_some(), "{b}");
        assert_eq!(ended().await, 1);

        // Codex: its notify reads no answer, so the nudge is typed in.
        let codex = &ids["codex"];
        let url = format!(
            "/{codex}/{}/hooks/turn_end",
            tokens.get_or_create(codex).unwrap()
        );
        let body =
            json!({"type": "agent-turn-complete", "last-assistant-message": asking}).to_string();
        let (_, b) = send(&app, post(&url, &body)).await;
        assert_eq!(b["decision"], "block");
        for _ in 0..50 {
            if !typed.0.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(*typed.0.lock().unwrap(), [questions::NUDGE.to_string()]);
        assert_eq!(ended().await, 1);
        let (_, b) = send(&app, post(&url, &body)).await;
        assert!(b.get("turn").is_some(), "{b}");
        assert_eq!(ended().await, 2);
        // A reply without a question just closes the turn.
        let done = json!({"last-assistant-message": "Started t1."}).to_string();
        let (_, b) = send(&app, post(&url, &done)).await;
        assert!(b.get("turn").is_some(), "{b}");
        assert_eq!(typed.0.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_reply_that_asks_in_text_is_sent_back_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open_in_memory().unwrap());
        let log = quark_eventlog::SqliteEventLog::open(dir.path().join("events.db")).unwrap();
        let coordinator = Arc::new(
            Coordinator::open(
                Arc::new(log.clone()),
                quark_core::HostId::from("h"),
                Config::default(),
                Arc::new(NoBrain),
                Arc::new(YesHands),
            )
            .await
            .unwrap(),
        );
        let tokens = Tokens::new(dir.path());
        let token = tokens.get_or_create("p1").unwrap();
        let app = router(coordinator.clone(), tokens, store, SessionRoots::default());
        let stop = |body: Value| post(&format!("/p1/{token}/hooks/Stop"), &body.to_string());
        let ended = || async {
            log.read(Seq::ZERO, 100)
                .await
                .unwrap()
                .iter()
                .filter(|e| e.kind.as_str() == "coordinator.turn_ended")
                .count()
        };
        let asking = "Plan is ready. Should I use Redis or keep it in memory?";

        let (s, b) = send(&app, stop(json!({"last_assistant_message": asking}))).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(b["decision"], "block");
        assert_eq!(b["reason"], questions::NUDGE);
        assert_eq!(ended().await, 0, "the turn stays open");

        // The stop the nudge caused ends the turn, whatever it said.
        let (_, b) = send(
            &app,
            stop(json!({"last_assistant_message": asking, "stop_hook_active": true})),
        )
        .await;
        assert_eq!(b, json!({}));
        assert_eq!(ended().await, 1);

        // Read from the session log when the hook does not carry the reply.
        let path = dir.path().join("session.jsonl");
        let line = |kind: &str, text: &str| {
            json!({"type": kind, "message": {"content": [{"type": "text", "text": text}]}})
                .to_string()
        };
        std::fs::write(
            &path,
            [
                line("user", "Go?"),
                line("assistant", asking),
                "{\"type\":\"system\"}".into(),
            ]
            .join("\n"),
        )
        .unwrap();
        let (_, b) = send(&app, stop(json!({"transcript_path": path}))).await;
        assert_eq!(b["decision"], "block");

        // A question filed with ask_user this turn needs no nudge.
        let ask = quark_coordinator::ToolCall::parse(
            "ask_user",
            &json!({
                "key": "cache", "question": "Redis or memory?",
                "context": "Sessions need a store.", "options": ["Redis", "Memory"],
                "recommended": "Memory", "recommended_why": "one process"
            }),
        )
        .unwrap();
        coordinator.call(&ProjectId::new("p1"), ask).await.unwrap();
        let (_, b) = send(&app, stop(json!({"last_assistant_message": asking}))).await;
        assert_eq!(b, json!({}));
        assert_eq!(ended().await, 2);

        // Nor does a reply without a question, and the next turn starts clean.
        let (_, b) = send(&app, stop(json!({"last_assistant_message": "Started t1."}))).await;
        assert_eq!(b, json!({}));
        let (_, b) = send(&app, stop(json!({"last_assistant_message": asking}))).await;
        assert_eq!(b["decision"], "block");
    }

    #[tokio::test]
    async fn an_agy_coordinator_gets_its_prompt_and_its_text_questions_sent_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open_in_memory().unwrap());
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(ws.join(".quark")).unwrap();
        let prompt = format!("# Coordinator\n{}", "Act through tools.\n".repeat(3000));
        std::fs::write(ws.join(".quark/coordinator.md"), &prompt).unwrap();
        store
            .create_project(CreateProject {
                name: "agy".into(),
                goal: None,
                workspace_path: Some(ws.display().to_string()),
                repos: vec![],
                agent_config: Some(quark_systems::AgentConfig {
                    harness: "antigravity".into(),
                    model: None,
                    effort: None,
                    pool: None,
                }),
                dispatch_preset: None,
                delivery: None,
            })
            .unwrap();
        let pid = store.list_projects().unwrap()[0].id.clone();
        let log = quark_eventlog::SqliteEventLog::open(dir.path().join("events.db")).unwrap();
        let coordinator = Arc::new(
            Coordinator::open(
                Arc::new(log.clone()),
                quark_core::HostId::from("h"),
                Config::default(),
                Arc::new(NoBrain),
                Arc::new(YesHands),
            )
            .await
            .unwrap(),
        );
        let tokens = Tokens::new(dir.path());
        let token = tokens.get_or_create(&pid).unwrap();
        let app = router(coordinator, tokens, store, SessionRoots::default());
        let transcript = dir.path().join("transcript_full.jsonl");
        let payload = json!({"conversationId": "c1", "transcriptPath": transcript,
            "hookEventName": "PreInvocation"});
        let ask_prompt = || {
            post(
                &format!("/{pid}/{token}/hooks/agy_prompt"),
                &payload.to_string(),
            )
        };

        // The first call of a conversation: agy has written nothing yet.
        let (s, b) = send(&app, ask_prompt()).await;
        assert_eq!(s, StatusCode::OK);
        let steps = b["injectSteps"].as_array().unwrap();
        assert!(steps.len() > 1, "a long prompt goes in parts");
        let parts: Vec<&str> = steps
            .iter()
            .map(|s| s["systemMessage"]["systemMessage"].as_str().unwrap())
            .collect();
        assert!(parts.iter().all(|p| p.len() < 32 * 1024));
        // The next call, before agy wrote it, adds nothing again.
        let (_, b) = send(&app, ask_prompt()).await;
        assert_eq!(b, json!({}));

        // agy wrote it; a compaction then drops it, so it goes in again.
        let line = |v: Value| format!("{v}\n");
        let mut text =
            line(json!({"type": "USER_INPUT", "content": "<USER_REQUEST>\nhi\n</USER_REQUEST>"}));
        for p in &parts {
            text += &line(json!({"type": "SYSTEM_MESSAGE", "source": "SYSTEM_SDK", "content": p}));
        }
        std::fs::write(&transcript, &text).unwrap();
        let (_, b) = send(&app, ask_prompt()).await;
        assert_eq!(b, json!({}));
        text += &line(json!({"type": "CHECKPOINT", "source": "SYSTEM",
            "content": "# Resuming from a compaction\n\nsummary"}));
        std::fs::write(&transcript, &text).unwrap();
        let (_, b) = send(&app, ask_prompt()).await;
        assert!(b["injectSteps"].is_array());

        // A reply that asks in text is sent back once, with agy's decision.
        let asking = "Plan is ready. Should I use Redis or keep it in memory?";
        text += &line(json!({"type": "PLANNER_RESPONSE", "source": "MODEL", "content": asking}));
        std::fs::write(&transcript, &text).unwrap();
        let stop_payload = json!({"transcriptPath": transcript, "continuationActive": false,
            "terminationReason": "NO_TOOL_CALL"});
        let stop = |launch: &str| {
            let mut req = post(
                &format!("/{pid}/{token}/hooks/agy_stop"),
                &stop_payload.to_string(),
            );
            req.headers_mut()
                .insert("x-quark-launch", launch.parse().unwrap());
            req
        };
        // The first stop of a start is sent back once: agy lists Quark's
        // tools only from then on. It counts as the turn's one send-back.
        let (_, b) = send(&app, stop("1")).await;
        assert_eq!(b["decision"], "continue");
        assert_eq!(b["reason"], crate::coordinator_agy::TOOLS_NOTE);
        let (_, b) = send(&app, stop("1")).await;
        assert_eq!(b, json!({}));
        let (_, b) = send(&app, stop("1")).await;
        assert_eq!(b["decision"], "continue");
        assert_eq!(b["reason"], questions::NUDGE);
        // agy's next stop still says `continuationActive: false`; it ends the turn.
        let (_, b) = send(&app, stop("1")).await;
        assert_eq!(b, json!({}));
        // An interrupted turn is never sent back, even on a new start.
        let cancelled = json!({"transcriptPath": transcript, "terminationReason": "USER_CANCELED"});
        let mut req = post(
            &format!("/{pid}/{token}/hooks/agy_stop"),
            &cancelled.to_string(),
        );
        req.headers_mut()
            .insert("x-quark-launch", "2".parse().unwrap());
        let (_, b) = send(&app, req).await;
        assert_eq!(b, json!({}));
        let ended = log
            .read(Seq::ZERO, 100)
            .await
            .unwrap()
            .iter()
            .filter(|e| e.kind.as_str() == "coordinator.turn_ended")
            .count();
        assert_eq!(ended, 3);
    }

    #[tokio::test]
    async fn a_kiro_turn_that_asks_in_text_is_read_from_its_log_and_nudged() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open_in_memory().unwrap());
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let p = store
            .create_project(CreateProject {
                name: "kiro".into(),
                goal: None,
                workspace_path: Some(ws.display().to_string()),
                repos: vec![],
                agent_config: Some(quark_systems::AgentConfig {
                    harness: "kiro".into(),
                    model: None,
                    effort: None,
                    pool: None,
                }),
                dispatch_preset: None,
                delivery: None,
            })
            .unwrap();
        // Kiro's V3 session store: the reply is only in the log.
        let kiro = dir.path().join("kiro-home");
        let sess = kiro.join("sessions/abc/sess_1");
        std::fs::create_dir_all(&sess).unwrap();
        std::fs::write(
            sess.join("session.json"),
            json!({"id": "sess_1", "workspacePaths": [ws.display().to_string()]}).to_string(),
        )
        .unwrap();
        let log_line = |v: Value| format!("{v}\n");
        let asking = "Plan is ready. Should I use Redis or keep it in memory?";
        std::fs::write(
            sess.join("messages.jsonl"),
            [
                log_line(json!({"id": "u", "payload": {"type": "user", "content": "plan it"}})),
                log_line(
                    json!({"id": "a", "payload": {"type": "assistant", "content": "Reading."}}),
                ),
                log_line(json!({"id": "t", "payload": {"type": "tool_call", "toolName": "shell"}})),
                log_line(json!({"id": "b", "payload": {"type": "assistant", "content": asking}})),
            ]
            .concat(),
        )
        .unwrap();
        let roots = SessionRoots {
            kiro: vec![kiro],
            ..SessionRoots::default()
        };
        let log = quark_eventlog::SqliteEventLog::open(dir.path().join("events.db")).unwrap();
        let coordinator = Arc::new(
            Coordinator::open(
                Arc::new(log.clone()),
                quark_core::HostId::from("h"),
                Config::default(),
                Arc::new(NoBrain),
                Arc::new(YesHands),
            )
            .await
            .unwrap(),
        );
        let tokens = Tokens::new(dir.path());
        let typed = Arc::new(Typed::default());
        let off = quark_systems::CompactionSettings {
            auto: false,
            at_percent: 70,
        };
        let app = router_wrapping_up(
            coordinator,
            tokens.clone(),
            store,
            roots.clone(),
            WrapUp {
                chat: typed.clone(),
                events: log.clone(),
                compactor: Arc::new(Compactor::new(off, roots)),
            },
        );
        let url = format!(
            "/{}/{}/hooks/turn_end",
            p.id,
            tokens.get_or_create(&p.id).unwrap()
        );
        // Kiro's `Stop` payload carries no reply.
        let body =
            json!({"session_id": "sess_1", "hook_event_name": "Stop", "cwd": ws}).to_string();
        let (_, b) = send(&app, post(&url, &body)).await;
        assert_eq!(b["decision"], "block");
        for _ in 0..50 {
            if !typed.0.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(*typed.0.lock().unwrap(), [questions::NUDGE.to_string()]);
        // The turn the nudge started closes at its own `Stop`.
        let (_, b) = send(&app, post(&url, &body)).await;
        assert!(b.get("turn").is_some(), "{b}");
        // A last reply without a question just closes the turn.
        std::fs::OpenOptions::new()
            .append(true)
            .open(sess.join("messages.jsonl"))
            .and_then(|mut f| {
                use std::io::Write;
                f.write_all(
                    log_line(json!({"id": "c", "payload": {"type": "assistant", "content": "Started t1."}}))
                        .as_bytes(),
                )
            })
            .unwrap();
        let (_, b) = send(&app, post(&url, &body)).await;
        assert!(b.get("turn").is_some(), "{b}");
        assert_eq!(typed.0.lock().unwrap().len(), 1);
    }

    #[test]
    fn installs_a_kiro_coordinator_agent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let urls = CoordinatorUrls::new("http://127.0.0.1:7777", "p1", "tok");
        assert!(install("kiro", root, &urls).unwrap());
        assert!(!install("kiro", root, &urls).unwrap(), "idempotent");
        let agent: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join(".kiro/agents/quark-coordinator.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(agent["name"], KIRO_AGENT);
        assert_eq!(agent["prompt"], "file://../../.quark/coordinator.md");
        assert_eq!(agent["includeMcpJson"], false);
        assert!(
            agent.get("allowedTools").is_none(),
            "V3 skips such an agent"
        );
        assert_eq!(
            agent["mcpServers"][SERVER_NAME]["url"],
            "http://127.0.0.1:7777/v1/coordinator/p1/tok/mcp"
        );
        assert_eq!(
            agent["permissions"]["rules"][0],
            json!({"capability": "mcp", "match": ["quark-coordinator/*"], "effect": "allow"})
        );
        let stop = agent["hooks"]["stop"][0]["command"].as_str().unwrap();
        assert!(
            stop.contains("/v1/coordinator/p1/tok/hooks/turn_end"),
            "{stop}"
        );
        assert!(
            stop.contains(">/dev/null"),
            "Kiro must not read a decision: {stop}"
        );
        // A new token is written in.
        let urls = CoordinatorUrls::new("http://127.0.0.1:7777", "p1", "tok2");
        assert!(install("kiro", root, &urls).unwrap());
    }

    #[test]
    fn installs_the_pi_bridge_and_leaves_codex_to_its_command_line() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let urls = CoordinatorUrls::new("http://127.0.0.1:7777", "p1", "tok");
        assert!(install("pi", root, &urls).unwrap());
        assert!(!install("pi-signed", root, &urls).unwrap(), "idempotent");
        let ext = std::fs::read_to_string(root.join(PI_EXTENSION)).unwrap();
        assert!(ext.contains(
            r#"const MCP_URL: string = "http://127.0.0.1:7777/v1/coordinator/p1/tok/mcp";"#
        ));
        assert!(ext.contains(
            r#"const HOOKS_URL: string = "http://127.0.0.1:7777/v1/coordinator/p1/tok/hooks";"#
        ));
        assert!(!ext.contains("__QUARK_"));
        let urls = CoordinatorUrls::new("http://127.0.0.1:7777", "p1", "tok2");
        assert!(install("pi", root, &urls).unwrap());

        let other = tempfile::tempdir().unwrap();
        assert!(!install("codex", other.path(), &urls).unwrap());
        assert_eq!(std::fs::read_dir(other.path()).unwrap().count(), 0);
        assert_eq!(Wiring::for_harness("claude"), Some(Wiring::Claude));
        for h in COORDINATOR_HARNESSES {
            assert!(Wiring::for_harness(h).is_some(), "{h}");
        }
    }

    #[test]
    fn installs_into_a_bob_workspace_and_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".bob")).unwrap();
        std::fs::write(
            root.join(".bob/settings.json"),
            r#"{"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "mine"}]}]}, "other": 1}"#,
        )
        .unwrap();
        std::fs::write(
            root.join(".bob/custom_modes.yaml"),
            r#"{"customModes": [{"slug": "theirs", "name": "Theirs"}]}"#,
        )
        .unwrap();
        let urls = CoordinatorUrls::new("http://127.0.0.1:7777", "p1", "tok");
        assert!(install("bob", root, &urls).unwrap());
        assert!(!install("bob", root, &urls).unwrap(), "idempotent");
        let read = |f: &str| -> Value {
            serde_json::from_str(&std::fs::read_to_string(root.join(f)).unwrap()).unwrap()
        };

        let mcp = read(".bob/mcp.json");
        let server = &mcp["mcpServers"][SERVER_NAME];
        assert_eq!(server["type"], "streamable-http");
        assert_eq!(server["url"], urls.mcp);
        let allowed = server["alwaysAllow"].as_array().unwrap();
        assert!(allowed.iter().any(|t| t == "ask_user"), "{allowed:?}");
        assert_eq!(server["groups"], json!([BOB_MODE]));

        let s = read(".bob/settings.json");
        assert_eq!(s["other"], 1);
        let stop = s["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0]["hooks"][0]["command"], "mine");
        assert!(stop[1]
            .to_string()
            .contains("/v1/coordinator/p1/tok/hooks/turn_end"));
        let start = s["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(start.len(), 1);
        assert_eq!(start[0]["hooks"][0]["command"], PRIME_HOOK);
        // Bob ignores every hook in a file with one key outside its schema.
        for group in stop.iter().chain(start) {
            for k in group.as_object().unwrap().keys() {
                assert!(["matcher", "hooks"].contains(&k.as_str()), "{k}");
            }
            for h in group["hooks"].as_array().unwrap() {
                for k in h.as_object().unwrap().keys() {
                    assert!(
                        ["type", "command", "timeout", "disabled"].contains(&k.as_str()),
                        "{k}"
                    );
                }
            }
        }

        let modes = read(".bob/custom_modes.yaml");
        let slugs: Vec<&str> = modes["customModes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|m| m["slug"].as_str())
            .collect();
        assert_eq!(slugs, ["theirs", BOB_MODE]);
        // Global MCP servers' tools (group `mcp`) stay out of the mode.
        let groups = &modes["customModes"][1]["groups"];
        assert!(
            !groups.as_array().unwrap().iter().any(|g| g == "mcp"),
            "{groups}"
        );

        // A new token replaces Quark's hook and server rather than adding more.
        let urls = CoordinatorUrls::new("http://127.0.0.1:7777", "p1", "tok2");
        assert!(install("bob", root, &urls).unwrap());
        let stop = read(".bob/settings.json")["hooks"]["Stop"].clone();
        assert_eq!(stop.as_array().unwrap().len(), 2);
        assert!(stop[1].to_string().contains("tok2"));

        // A person's own YAML modes are left alone.
        let other = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(other.path().join(".bob")).unwrap();
        let yaml = "customModes:\n  - slug: mine\n";
        std::fs::write(other.path().join(".bob/custom_modes.yaml"), yaml).unwrap();
        install("bob", other.path(), &urls).unwrap();
        assert_eq!(
            std::fs::read_to_string(other.path().join(".bob/custom_modes.yaml")).unwrap(),
            yaml
        );
    }

    #[test]
    fn installs_into_a_claude_workspace_and_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        std::fs::write(
            root.join(".claude/settings.local.json"),
            r#"{"permissions": {"allow": ["Bash(ls)"]}, "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "mine"}]}]}}"#,
        )
        .unwrap();
        let urls = CoordinatorUrls::new("http://127.0.0.1:7777", "p1", "tok");
        assert_eq!(urls.mcp, "http://127.0.0.1:7777/v1/coordinator/p1/tok/mcp");
        assert!(install_claude(root, &urls).unwrap());
        assert!(!install_claude(root, &urls).unwrap(), "idempotent");

        let mcp: Value =
            serde_json::from_str(&std::fs::read_to_string(root.join(".mcp.json")).unwrap())
                .unwrap();
        assert_eq!(mcp["mcpServers"][SERVER_NAME]["url"], urls.mcp);
        let read = || -> Value {
            serde_json::from_str(
                &std::fs::read_to_string(root.join(".claude/settings.local.json")).unwrap(),
            )
            .unwrap()
        };
        let s = read();
        assert_eq!(s["permissions"]["allow"][0], "Bash(ls)");
        assert_eq!(s["enabledMcpjsonServers"], json!([SERVER_NAME]));
        assert_eq!(s["promptSuggestionEnabled"], json!(false));
        assert_eq!(s["feedbackDrafts"], json!("off"));
        let stop = s["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0]["hooks"][0]["command"], "mine");
        assert!(stop[1]
            .to_string()
            .contains("/v1/coordinator/p1/tok/hooks/Stop"));

        // A new token replaces the old hook rather than adding another.
        let urls = CoordinatorUrls::new("http://127.0.0.1:7777", "p1", "tok2");
        assert!(install_claude(root, &urls).unwrap());
        let stop = read()["hooks"]["Stop"].as_array().unwrap().clone();
        assert_eq!(stop.len(), 2);
        assert!(stop[1].to_string().contains("tok2"));
        // Beads' memories are injected at session start, by one hook however often this runs.
        let start = read()["hooks"]["SessionStart"].as_array().unwrap().clone();
        assert_eq!(start.len(), 1);
        assert_eq!(start[0]["hooks"][0]["command"], PRIME_HOOK);
        assert!(PRIME_HOOK.contains(PRIME_MARK));
    }
}
