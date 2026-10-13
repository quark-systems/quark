//! The native coordinator's routes (slice 6): its tools over MCP and its
//! harness's turn-end hook, nested under [`PREFIX`].
//!
//! - `POST /{project}/{token}/mcp`: MCP streamable HTTP, one JSON-RPC
//!   message per POST, answered by [`CoordinatorMcp`]. `GET` is `405`.
//! - `POST /{project}/{token}/hooks/{event}`: the harness's turn-end hook
//!   (`turn_end`, or Claude Code's `Stop`). It closes the open turn with the
//!   turn's token use: the body's `usage` when the hook sends one, else read
//!   from the coordinator's own session log. An `Idempotency-Key` header
//!   makes a retried hook count once.
//!
//! The token is minted once per Project and kept in `<home>/coordinators/`
//! (mode `0600`), so only the coordinator quarkd configured can reach its
//! tools. A wrong token is `404`, the same as an unknown Project.
//!
//! [`install_claude`] writes those URLs into a Claude Code coordinator's
//! workspace: the MCP server in `.mcp.json` and the `Stop` hook in
//! `.claude/settings.local.json`.

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
use quark_coordinator::{Coordinator, Usage};
use quark_core::ProjectId;
use quark_transcript::{locate, read_turns, SessionFormat, SessionRoots, UsageCursor};
use serde_json::{json, Value};

use crate::store::Store;

/// Where the routes are nested.
pub const PREFIX: &str = "/v1/coordinator";
/// The MCP server's name in a coordinator's harness config.
pub const SERVER_NAME: &str = "quark-coordinator";
/// Hook events that end a turn.
const TURN_END: &[&str] = &["turn_end", "Stop"];

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
}

/// The routes, to nest under [`PREFIX`].
pub fn router(
    coordinator: Arc<Coordinator>,
    tokens: Tokens,
    store: Arc<Store>,
    roots: SessionRoots,
) -> Router {
    let routes = Routes {
        mcp: CoordinatorMcp::new(coordinator.clone()),
        coordinator,
        tokens,
        store,
        roots,
        cursors: Mutex::new(HashMap::new()),
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
    let usage = match body.get("usage") {
        Some(u) => match serde_json::from_value::<Usage>(u.clone()) {
            Ok(u) => u,
            Err(e) => return bad(format!("usage: {e}")),
        },
        None => r.logged_usage(&project).await,
    };
    let id = ProjectId::new(project);
    match r
        .coordinator
        .turn_ended(&id, usage, source.as_deref())
        .await
    {
        Ok(turn) => Json(json!({ "turn": turn })).into_response(),
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

impl Routes {
    /// The token use logged by the coordinator since its last turn end,
    /// from its newest session log in the Project workspace. Zero when
    /// there is no log to read. The first read of a log counts only its
    /// last turn, not the whole history.
    async fn logged_usage(&self, project: &str) -> Usage {
        let store = self.store.clone();
        let id = project.to_string();
        let Ok(Ok(p)) = tokio::task::spawn_blocking(move || store.get_project(&id)).await else {
            return Usage::default();
        };
        let Some(root) = p.workspace_path.map(PathBuf::from) else {
            return Usage::default();
        };
        let roots = self.roots.clone();
        let Ok(Some((path, format))) =
            tokio::task::spawn_blocking(move || newest_log(&roots, &root)).await
        else {
            return Usage::default();
        };
        let known = self.cursors.lock().unwrap().get(&path).cloned();
        let first = known.is_none();
        let cursor = known.unwrap_or_default();
        let p = path.clone();
        let read = tokio::task::spawn_blocking(move || read_turns(&p, &cursor, format, true)).await;
        let Ok(Ok((turns, next))) = read else {
            return Usage::default();
        };
        self.cursors.lock().unwrap().insert(path, next);
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
        usage
    }
}

fn newest_log(roots: &SessionRoots, cwd: &Path) -> Option<(PathBuf, SessionFormat)> {
    SessionFormat::ALL
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
        "curl -fsS -m 10 -X POST -H 'Content-Type: application/json' --data-binary @- '{}/turn_end' >/dev/null 2>&1 || true",
        urls.hooks
    );
    changed |= edit_json(&root.join(".claude/settings.local.json"), |v| {
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

/// Apply `edit` to the JSON object at `path` (an empty object when the file
/// is missing), writing it back only when it changed.
fn edit_json(path: &Path, edit: impl FnOnce(&mut Value)) -> std::io::Result<bool> {
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
    use quark_coordinator::{Config, Mode, NoBrain, NoHands};
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
                Mode::Shadow,
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
        let call = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"tell_user","arguments":{"text":"hi"}}}"#;
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
        let stop = s["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0]["hooks"][0]["command"], "mine");
        assert!(stop[1]
            .to_string()
            .contains("/v1/coordinator/p1/tok/hooks/turn_end"));

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
