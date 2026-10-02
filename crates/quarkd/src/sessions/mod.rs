//! Terminal sessions: worker and coordinator panes streamed from tmux.
//!
//! Each Project workspace runs its own tmux server on a private socket,
//! `<quark home>/run/tmux/<project id>`. Firstmate creates task windows in
//! the server its supervisor runs in, so a coordinator started with
//! [`Sessions::start_window`] puts every worker it spawns on that server.
//!
//! For every session on a workspace server the daemon attaches one tmux
//! control-mode client ([`control`]), which delivers every pane's output as
//! raw bytes. Panes are mapped to terminals:
//!
//! - a window whose `@quark_role` is `coordinator` is the Project's
//!   coordinator terminal, with the Project id as its terminal id;
//! - a window whose `session:window` target matches a task's engine endpoint
//!   is that task's worker terminal, with the task id as its terminal id.
//!
//! Output from mapped panes becomes `worker.output` events. The first event
//! for a pane is a snapshot of its screen, taken in stream order, and a new
//! snapshot follows whenever output had to be dropped (tmux paused a pane
//! the daemon fell behind on) or old output was pruned. After a daemon
//! restart the servers are still running, so terminals reattach where they
//! were and start again from a snapshot.

pub mod control;
pub mod server;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use base64::Engine as _;
use quark_systems::{Event, Terminal, TerminalChunkKind, TerminalOutput, TerminalRole};
use tokio::sync::{oneshot, Notify};

use crate::store::{self, Store};
pub use control::ControlError;
use control::{ControlClient, Handler, Reply, Waiter};
pub use server::{PaneInfo, Server, ServerError, WindowSpec};

/// Output kept per terminal in the event log.
pub const RETAIN_BYTES: u64 = 2 * 1024 * 1024;
/// Largest `worker.output` chunk the writer builds by merging.
const MAX_CHUNK: usize = 256 * 1024;
/// tmux pauses a pane this many seconds behind and the daemon resyncs it.
const PAUSE_AFTER_SECS: u32 = 5;
/// Bytes typed per `send-keys` command.
const INPUT_CHUNK: usize = 512;
/// Layout notifications arrive in bursts; rescan once per burst.
const RESCAN_DEBOUNCE: Duration = Duration::from_millis(30);

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("terminal sessions are unavailable: {0}")]
    Unavailable(String),
    #[error("terminal not found")]
    NotFound,
    #[error("input sequence {got} is not after {last}")]
    StaleInput { got: u64, last: u64 },
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Server(#[from] ServerError),
    #[error(transparent)]
    Control(#[from] ControlError),
    #[error(transparent)]
    Store(#[from] store::StoreError),
}

/// A task whose engine endpoint is a tmux `session:window` target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskTarget {
    pub task_id: String,
    pub target: String,
}

/// The daemon's terminal sessions across all workspaces. Cheap to clone.
#[derive(Clone)]
pub struct Sessions {
    shared: Option<Arc<Shared>>,
    reason: Arc<str>,
}

struct Shared {
    tmux: PathBuf,
    run_dir: PathBuf,
    sink: std::sync::mpsc::Sender<Chunk>,
    workspaces: Mutex<HashMap<String, Arc<Workspace>>>,
    runtime: tokio::runtime::Handle,
}

impl Sessions {
    /// Sessions backed by the `tmux` binary, with sockets under `run_dir`.
    /// Output is written to `store` by a dedicated thread. Must be called
    /// from within a Tokio runtime.
    pub fn new(tmux: impl Into<PathBuf>, run_dir: impl Into<PathBuf>, store: Arc<Store>) -> Self {
        let (sink, rx) = std::sync::mpsc::channel();
        let shared = Arc::new(Shared {
            tmux: tmux.into(),
            run_dir: run_dir.into(),
            sink,
            workspaces: Mutex::new(HashMap::new()),
            runtime: tokio::runtime::Handle::current(),
        });
        let weak = Arc::downgrade(&shared);
        std::thread::Builder::new()
            .name("terminal-output".into())
            .spawn(move || write_output(rx, store, weak))
            .expect("spawn terminal output writer");
        Self {
            shared: Some(shared),
            reason: Arc::from(""),
        }
    }

    /// Sessions that report `reason` for every call, e.g. without tmux.
    pub fn disabled(reason: impl Into<String>) -> Self {
        Self {
            shared: None,
            reason: Arc::from(reason.into()),
        }
    }

    /// The tmux binary to use: `tmux` from `PATH` unless given, checked by
    /// running `tmux -V`.
    pub fn detect(tmux: Option<&Path>, run_dir: PathBuf, store: Arc<Store>) -> Self {
        let bin = tmux.map(Path::to_path_buf).unwrap_or_else(|| "tmux".into());
        match std::process::Command::new(&bin).arg("-V").output() {
            Ok(out) if out.status.success() => {
                let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
                tracing::info!(%version, "terminal sessions enabled");
                Self::new(bin, run_dir, store)
            }
            _ => {
                let reason = format!("{} is not installed or does not run", bin.display());
                tracing::warn!(%reason, "terminal sessions disabled");
                Self::disabled(reason)
            }
        }
    }

    fn shared(&self) -> Result<&Arc<Shared>, SessionError> {
        self.shared
            .as_ref()
            .ok_or_else(|| SessionError::Unavailable(self.reason.to_string()))
    }

    /// The tmux server for a Project's workspace.
    pub fn server(&self, project_id: &str) -> Result<Server, SessionError> {
        let shared = self.shared()?;
        if project_id.is_empty()
            || !project_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(SessionError::Invalid(format!(
                "invalid project id {project_id:?}"
            )));
        }
        Ok(Server::new(
            &shared.tmux,
            shared.run_dir.join("tmux").join(project_id),
        )?)
    }

    /// Starts a program in a new window on the Project's server, starting the
    /// server first if needed, then maps it. Use `role: coordinator` for the
    /// Project's coordinator. Returns the tmux window id.
    pub async fn start_window(
        &self,
        project_id: &str,
        spec: &WindowSpec,
    ) -> Result<String, SessionError> {
        let window = self.server(project_id)?.start_window(spec).await?;
        self.workspace(project_id)?.rescan().await;
        Ok(window)
    }

    /// Updates which tasks own which window targets for one Project and
    /// reconciles its panes. Called after every projection refresh.
    pub async fn sync(
        &self,
        project_id: &str,
        targets: Vec<TaskTarget>,
    ) -> Result<(), SessionError> {
        let ws = self.workspace(project_id)?;
        *ws.targets.lock().unwrap() = targets.into_iter().map(|t| (t.target, t.task_id)).collect();
        ws.rescan().await;
        Ok(())
    }

    /// Live terminals of one Project.
    pub fn list(&self, project_id: &str) -> Vec<Terminal> {
        let Ok(shared) = self.shared() else {
            return Vec::new();
        };
        let ws = shared.workspaces.lock().unwrap().get(project_id).cloned();
        let Some(ws) = ws else {
            return Vec::new();
        };
        let mut out: Vec<Terminal> = ws
            .panes
            .lock()
            .unwrap()
            .values()
            .map(|p| p.terminal.clone())
            .collect();
        out.sort_by(|a, b| (a.role as u8, &a.id).cmp(&(b.role as u8, &b.id)));
        out
    }

    pub fn get(&self, terminal_id: &str) -> Result<Terminal, SessionError> {
        let (ws, pane) = self.find(terminal_id)?;
        let panes = ws.panes.lock().unwrap();
        Ok(panes
            .get(&pane)
            .ok_or(SessionError::NotFound)?
            .terminal
            .clone())
    }

    /// Types raw bytes into a terminal. With `seq`, input whose sequence
    /// number is not above the last one applied is refused, so a retried
    /// request is never typed twice.
    pub async fn input(
        &self,
        terminal_id: &str,
        bytes: &[u8],
        seq: Option<u64>,
    ) -> Result<(), SessionError> {
        let (ws, pane) = self.find(terminal_id)?;
        let client = ws.client_for(&pane)?;
        // Hold the sequence lock while typing so concurrent requests for one
        // terminal are applied in sequence order or refused.
        let _guard = ws.input_lock.lock().await;
        if let Some(seq) = seq {
            let mut last = ws.input_seq.lock().unwrap();
            let entry = last.entry(terminal_id.to_string()).or_insert(0);
            if seq <= *entry {
                return Err(SessionError::StaleInput {
                    got: seq,
                    last: *entry,
                });
            }
            *entry = seq;
        }
        for chunk in bytes.chunks(INPUT_CHUNK) {
            client.run(&send_keys(&pane, chunk)).await?;
        }
        Ok(())
    }

    /// Resizes a terminal's window. The program gets SIGWINCH and redraws;
    /// the last caller's size wins.
    pub async fn resize(
        &self,
        terminal_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<Terminal, SessionError> {
        if !(2..=1000).contains(&cols) || !(2..=1000).contains(&rows) {
            return Err(SessionError::Invalid(format!(
                "size {cols}x{rows} is out of range"
            )));
        }
        let (ws, pane) = self.find(terminal_id)?;
        let client = ws.client_for(&pane)?;
        let window = {
            let panes = ws.panes.lock().unwrap();
            panes
                .get(&pane)
                .ok_or(SessionError::NotFound)?
                .window_id
                .clone()
        };
        // resize-window also switches this one window to manual sizing; the
        // global `window-size manual` option crashes tmux 3.4.
        client
            .run(&format!("resize-window -t {window} -x {cols} -y {rows}"))
            .await?;
        let mut panes = ws.panes.lock().unwrap();
        let p = panes.get_mut(&pane).ok_or(SessionError::NotFound)?;
        p.terminal.cols = cols;
        p.terminal.rows = rows;
        Ok(p.terminal.clone())
    }

    /// Appends a fresh snapshot of the terminal's screen to the event stream
    /// and returns that event. A client opening a terminal applies it, then
    /// the terminal's `worker.output` events with a greater `seq`.
    pub async fn snapshot(&self, terminal_id: &str) -> Result<Event, SessionError> {
        let (ws, pane) = self.find(terminal_id)?;
        let client = ws.client_for(&pane)?;
        let (tx, rx) = oneshot::channel();
        request_snapshot(&client, &pane, Some(tx)).await?;
        rx.await
            .map_err(|_| SessionError::Unavailable("the terminal closed".into()))
    }

    fn workspace(&self, project_id: &str) -> Result<Arc<Workspace>, SessionError> {
        let server = self.server(project_id)?;
        let shared = self.shared()?;
        let mut map = shared.workspaces.lock().unwrap();
        Ok(map
            .entry(project_id.to_string())
            .or_insert_with(|| Workspace::start(project_id, server, shared.sink.clone()))
            .clone())
    }

    fn find(&self, terminal_id: &str) -> Result<(Arc<Workspace>, String), SessionError> {
        let shared = self.shared()?;
        let workspaces: Vec<_> = shared
            .workspaces
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        for ws in workspaces {
            let panes = ws.panes.lock().unwrap();
            if let Some((pane, _)) = panes.iter().find(|(_, p)| p.terminal.id == terminal_id) {
                let pane = pane.clone();
                drop(panes);
                return Ok((ws, pane));
            }
        }
        Err(SessionError::NotFound)
    }

    /// Detaches every control client. tmux servers and their programs keep
    /// running, so a later daemon reattaches to them.
    pub fn detach_all(&self) {
        if let Some(shared) = &self.shared {
            for ws in shared.workspaces.lock().unwrap().drain().map(|(_, ws)| ws) {
                ws.detach();
            }
        }
    }
}

fn send_keys(pane: &str, bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut cmd = format!("send-keys -t {pane} -H");
    for b in bytes {
        write!(cmd, " {b:02x}").unwrap();
    }
    cmd
}

/// One Project workspace's server, its control clients and pane map.
struct Workspace {
    project_id: String,
    server: Server,
    sink: std::sync::mpsc::Sender<Chunk>,
    /// Window target (`session:window`) to task id.
    targets: Mutex<HashMap<String, String>>,
    /// Mapped panes by tmux pane id. Shared with the control readers.
    panes: Arc<Mutex<HashMap<String, PaneRoute>>>,
    /// Control clients by tmux session id.
    clients: Mutex<HashMap<String, ControlClient<Tag>>>,
    input_seq: Mutex<HashMap<String, u64>>,
    input_lock: tokio::sync::Mutex<()>,
    rescan_lock: tokio::sync::Mutex<()>,
    layout_changed: Arc<Notify>,
}

#[derive(Debug, Clone)]
struct PaneRoute {
    session_id: String,
    window_id: String,
    terminal: Terminal,
    /// Output is forwarded only once the pane's first snapshot is out.
    live: bool,
    cursor: Option<Cursor>,
}

#[derive(Debug, Clone, Copy)]
struct Cursor {
    x: u16,
    y: u16,
    cols: u16,
    rows: u16,
    alternate: bool,
    visible: bool,
}

impl Workspace {
    fn start(project_id: &str, server: Server, sink: std::sync::mpsc::Sender<Chunk>) -> Arc<Self> {
        let ws = Arc::new(Workspace {
            project_id: project_id.to_string(),
            server,
            sink,
            targets: Mutex::new(HashMap::new()),
            panes: Arc::new(Mutex::new(HashMap::new())),
            clients: Mutex::new(HashMap::new()),
            input_seq: Mutex::new(HashMap::new()),
            input_lock: tokio::sync::Mutex::new(()),
            rescan_lock: tokio::sync::Mutex::new(()),
            layout_changed: Arc::new(Notify::new()),
        });
        let weak = Arc::downgrade(&ws);
        let notify = ws.layout_changed.clone();
        tokio::spawn(async move {
            loop {
                notify.notified().await;
                tokio::time::sleep(RESCAN_DEBOUNCE).await;
                let Some(ws) = weak.upgrade() else { return };
                ws.rescan().await;
            }
        });
        ws
    }

    fn client_for(&self, pane: &str) -> Result<ControlClient<Tag>, SessionError> {
        let session = {
            let panes = self.panes.lock().unwrap();
            panes
                .get(pane)
                .ok_or(SessionError::NotFound)?
                .session_id
                .clone()
        };
        self.clients
            .lock()
            .unwrap()
            .get(&session)
            .filter(|c| !c.is_closed())
            .cloned()
            .ok_or(SessionError::Control(ControlError::Closed))
    }

    /// Reconciles control clients and the pane map with the server.
    async fn rescan(self: &Arc<Self>) {
        let _guard = self.rescan_lock.lock().await;
        let panes = match self.server.list_panes().await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(project = %self.project_id, error = %e, "listing tmux panes failed");
                return;
            }
        };

        // One control client per session; drop clients of sessions that are gone.
        let mut sessions: Vec<&str> = panes.iter().map(|p| p.session_id.as_str()).collect();
        sessions.sort();
        sessions.dedup();
        let mut fresh = Vec::new();
        {
            let mut clients = self.clients.lock().unwrap();
            clients.retain(|id, c| {
                let keep = sessions.contains(&id.as_str()) && !c.is_closed();
                if !keep {
                    c.detach();
                }
                keep
            });
            for session in &sessions {
                if clients.contains_key(*session) {
                    continue;
                }
                let handler = PaneHandler {
                    project_id: self.project_id.clone(),
                    session_id: session.to_string(),
                    panes: self.panes.clone(),
                    sink: self.sink.clone(),
                    layout_changed: self.layout_changed.clone(),
                    ws: Arc::downgrade(self),
                };
                match ControlClient::attach(
                    self.server.tmux(),
                    self.server.socket(),
                    session,
                    PAUSE_AFTER_SECS,
                    handler,
                ) {
                    Ok(c) => {
                        tracing::info!(project = %self.project_id, session, "attached tmux control client");
                        clients.insert(session.to_string(), c);
                        fresh.push(session.to_string());
                    }
                    Err(e) => {
                        tracing::warn!(project = %self.project_id, session, error = %e, "tmux control attach failed")
                    }
                }
            }
        }

        // The first pane of each window is the terminal.
        let mut first: HashMap<&str, &PaneInfo> = HashMap::new();
        for p in &panes {
            first
                .entry(p.window_id.as_str())
                .and_modify(|cur| {
                    if p.pane_index < cur.pane_index {
                        *cur = p;
                    }
                })
                .or_insert(p);
        }
        let targets = self.targets.lock().unwrap().clone();
        let mut wanted: HashMap<String, PaneRoute> = HashMap::new();
        for p in first.values() {
            let (role, id, task_id) = if p.role.as_deref() == Some("coordinator") {
                (TerminalRole::Coordinator, self.project_id.clone(), None)
            } else if let Some(task) = targets.get(&p.target) {
                (TerminalRole::Worker, task.clone(), Some(task.clone()))
            } else {
                continue;
            };
            // Two windows claiming one terminal: keep the first seen.
            if wanted.values().any(|w| w.terminal.id == id) {
                continue;
            }
            let title = p.target.split_once(':').map_or(p.target.as_str(), |t| t.1);
            wanted.insert(
                p.pane_id.clone(),
                PaneRoute {
                    session_id: p.session_id.clone(),
                    window_id: p.window_id.clone(),
                    terminal: Terminal {
                        id,
                        project_id: self.project_id.clone(),
                        role,
                        task_id,
                        title: title.to_string(),
                        cols: p.cols,
                        rows: p.rows,
                    },
                    live: false,
                    cursor: None,
                },
            );
        }

        let mut new_panes = Vec::new();
        {
            let mut current = self.panes.lock().unwrap();
            current.retain(|pane, route| match wanted.get(pane) {
                Some(w) => w.terminal.id == route.terminal.id && w.session_id == route.session_id,
                None => false,
            });
            for (pane, route) in wanted {
                match current.get_mut(&pane) {
                    Some(cur) => {
                        cur.terminal.cols = route.terminal.cols;
                        cur.terminal.rows = route.terminal.rows;
                        cur.terminal.title = route.terminal.title;
                        cur.window_id = route.window_id;
                        // A freshly attached client has not sent this pane yet.
                        if fresh.contains(&cur.session_id) {
                            cur.live = false;
                            new_panes.push((pane, cur.session_id.clone()));
                        }
                    }
                    None => {
                        new_panes.push((pane.clone(), route.session_id.clone()));
                        current.insert(pane, route);
                    }
                }
            }
        }

        for (pane, session) in new_panes {
            let client = self.clients.lock().unwrap().get(&session).cloned();
            if let Some(client) = client {
                if let Err(e) = request_snapshot(&client, &pane, None).await {
                    tracing::warn!(project = %self.project_id, pane, error = %e, "snapshot request failed");
                }
            }
        }
    }

    fn detach(&self) {
        for (_, c) in self.clients.lock().unwrap().drain() {
            c.detach();
        }
        self.panes.lock().unwrap().clear();
    }
}

/// Context for replies the control reader handles in stream order.
enum Tag {
    Cursor(String),
    Screen(String, Option<oneshot::Sender<Event>>),
}

/// Asks for the pane's cursor state and screen. Their replies arrive in
/// stream order: output before them is part of the capture, output after
/// them follows the snapshot.
async fn request_snapshot(
    client: &ControlClient<Tag>,
    pane: &str,
    done: Option<oneshot::Sender<Event>>,
) -> Result<(), ControlError> {
    client
        .send(
            &format!(
                "display-message -p -t {pane} \
                 '#{{cursor_x}} #{{cursor_y}} #{{pane_width}} #{{pane_height}} #{{alternate_on}} #{{cursor_flag}}'"
            ),
            Waiter::Reader(Tag::Cursor(pane.to_string())),
        )
        .await?;
    client
        .send(
            &format!("capture-pane -p -e -t {pane}"),
            Waiter::Reader(Tag::Screen(pane.to_string(), done)),
        )
        .await
}

struct PaneHandler {
    project_id: String,
    session_id: String,
    panes: Arc<Mutex<HashMap<String, PaneRoute>>>,
    sink: std::sync::mpsc::Sender<Chunk>,
    layout_changed: Arc<Notify>,
    ws: Weak<Workspace>,
}

impl PaneHandler {
    fn emit(
        &self,
        route: &PaneRoute,
        kind: TerminalChunkKind,
        data: &[u8],
        done: Option<oneshot::Sender<Event>>,
    ) {
        let (cols, rows) = match kind {
            TerminalChunkKind::Snapshot => (Some(route.terminal.cols), Some(route.terminal.rows)),
            TerminalChunkKind::Output => (None, None),
        };
        let _ = self.sink.send(Chunk {
            project_id: self.project_id.clone(),
            terminal_id: route.terminal.id.clone(),
            role: route.terminal.role,
            task_id: route.terminal.task_id.clone(),
            kind,
            data: data.to_vec(),
            cols,
            rows,
            done,
        });
    }
}

impl Handler for PaneHandler {
    type Tag = Tag;

    fn output(&mut self, pane: &str, data: Vec<u8>) {
        let panes = self.panes.lock().unwrap();
        if let Some(route) = panes.get(pane) {
            if route.live && route.session_id == self.session_id {
                let route = route.clone();
                drop(panes);
                self.emit(&route, TerminalChunkKind::Output, &data, None);
            }
        }
    }

    fn pause(&mut self, client: &ControlClient<Tag>, pane: &str) {
        // The client fell behind and tmux dropped this pane's queued output.
        // Resync from a snapshot, then let the pane continue.
        if let Some(route) = self.panes.lock().unwrap().get_mut(pane) {
            route.live = false;
        }
        tracing::debug!(project = %self.project_id, pane, "tmux paused pane; resyncing");
        let client = client.clone();
        let pane = pane.to_string();
        tokio::spawn(async move {
            if request_snapshot(&client, &pane, None).await.is_ok() {
                let _ = client
                    .send(
                        &format!("refresh-client -A '{pane}:continue'"),
                        Waiter::Discard,
                    )
                    .await;
            }
        });
    }

    fn reply(&mut self, tag: Tag, reply: Reply) {
        match tag {
            Tag::Cursor(pane) => {
                let cursor = parse_cursor(&reply);
                if let Some(route) = self.panes.lock().unwrap().get_mut(&pane) {
                    route.cursor = cursor;
                    if let Some(c) = cursor {
                        route.terminal.cols = c.cols;
                        route.terminal.rows = c.rows;
                    }
                }
            }
            Tag::Screen(pane, done) => {
                let route = {
                    let mut panes = self.panes.lock().unwrap();
                    let Some(route) = panes.get_mut(&pane) else {
                        return;
                    };
                    if !reply.ok {
                        tracing::warn!(project = %self.project_id, pane, error = %reply.error_text(), "capture-pane failed");
                        return;
                    }
                    route.live = true;
                    route.clone()
                };
                let data = render_snapshot(&reply.lines, route.cursor);
                self.emit(&route, TerminalChunkKind::Snapshot, &data, done);
            }
        }
    }

    fn layout(&mut self) {
        self.layout_changed.notify_one();
    }

    fn closed(&mut self) {
        tracing::info!(project = %self.project_id, session = %self.session_id, "tmux control client closed");
        if let Some(ws) = self.ws.upgrade() {
            ws.clients.lock().unwrap().remove(&self.session_id);
            ws.panes
                .lock()
                .unwrap()
                .retain(|_, r| r.session_id != self.session_id);
            ws.layout_changed.notify_one();
        }
    }
}

fn parse_cursor(reply: &Reply) -> Option<Cursor> {
    if !reply.ok {
        return None;
    }
    let line = String::from_utf8_lossy(reply.lines.first()?).into_owned();
    let f: Vec<u16> = line
        .split_whitespace()
        .map(|v| v.parse().ok())
        .collect::<Option<_>>()?;
    let [x, y, cols, rows, alternate, visible] = f[..] else {
        return None;
    };
    Some(Cursor {
        x,
        y,
        cols,
        rows,
        alternate: alternate == 1,
        visible: visible == 1,
    })
}

/// Bytes that repaint a terminal: reset, switch to the alternate screen if
/// the program uses it, draw each captured row in place, restore the cursor.
fn render_snapshot(lines: &[Vec<u8>], cursor: Option<Cursor>) -> Vec<u8> {
    use std::io::Write as _;
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1bc");
    if cursor.is_some_and(|c| c.alternate) {
        out.extend_from_slice(b"\x1b[?1049h");
    }
    let rows = cursor.map_or(lines.len(), |c| c.rows as usize);
    for (i, line) in lines.iter().take(rows).enumerate() {
        if line.is_empty() {
            continue;
        }
        write!(out, "\x1b[{};1H", i + 1).unwrap();
        out.extend_from_slice(line);
        out.extend_from_slice(b"\x1b[0m");
    }
    match cursor {
        Some(c) => {
            write!(out, "\x1b[{};{}H", c.y + 1, c.x + 1).unwrap();
            if !c.visible {
                out.extend_from_slice(b"\x1b[?25l");
            }
        }
        None => out.extend_from_slice(b"\x1b[H"),
    }
    out
}

/// Terminal bytes on their way to the event log.
struct Chunk {
    project_id: String,
    terminal_id: String,
    role: TerminalRole,
    task_id: Option<String>,
    kind: TerminalChunkKind,
    data: Vec<u8>,
    cols: Option<u16>,
    rows: Option<u16>,
    done: Option<oneshot::Sender<Event>>,
}

/// Writes output to the store: takes whatever is queued, merges adjacent
/// output for each terminal, appends it in one transaction, and prunes each
/// terminal's old output, asking for a fresh snapshot after pruning so the
/// retained output always starts from a full screen.
fn write_output(rx: std::sync::mpsc::Receiver<Chunk>, store: Arc<Store>, shared: Weak<Shared>) {
    let mut since_prune: HashMap<String, u64> = HashMap::new();
    while let Ok(first) = rx.recv() {
        let mut batch: Vec<Chunk> = vec![first];
        while let Ok(next) = rx.try_recv() {
            let mergeable = next.kind == TerminalChunkKind::Output
                && next.done.is_none()
                && batch
                    .iter()
                    .rposition(|c| c.terminal_id == next.terminal_id)
                    .is_some_and(|i| {
                        let c = &batch[i];
                        c.kind == TerminalChunkKind::Output
                            && c.data.len() + next.data.len() <= MAX_CHUNK
                    });
            if mergeable {
                let i = batch
                    .iter()
                    .rposition(|c| c.terminal_id == next.terminal_id)
                    .unwrap();
                batch[i].data.extend_from_slice(&next.data);
            } else {
                batch.push(next);
            }
            if batch.len() >= 512 {
                break;
            }
        }

        let rows: Vec<store::TerminalOutput> = batch
            .iter()
            .map(|c| store::TerminalOutput {
                project_id: Some(c.project_id.clone()),
                output: TerminalOutput {
                    terminal_id: c.terminal_id.clone(),
                    role: c.role,
                    task_id: c.task_id.clone(),
                    kind: c.kind,
                    data_b64: base64::engine::general_purpose::STANDARD.encode(&c.data),
                    cols: c.cols,
                    rows: c.rows,
                },
            })
            .collect();
        let events = match store.append_terminal_output(&rows) {
            Ok(events) => events,
            Err(e) => {
                tracing::error!(error = %e, "could not record terminal output");
                continue;
            }
        };
        for (chunk, event) in batch.into_iter().zip(events) {
            if let Some(done) = chunk.done {
                let _ = done.send(event);
            }
            let n = since_prune.entry(chunk.terminal_id.clone()).or_default();
            *n += chunk.data.len() as u64;
            if *n >= RETAIN_BYTES / 4 {
                *n = 0;
                if let Err(e) = store.prune_terminal_output(&chunk.terminal_id, RETAIN_BYTES) {
                    tracing::warn!(error = %e, "pruning terminal output failed");
                }
                if let Some(shared) = shared.upgrade() {
                    resnapshot(&shared, &chunk.project_id, &chunk.terminal_id);
                }
            }
        }
    }
}

/// Requests a snapshot of a terminal from the writer thread.
fn resnapshot(shared: &Shared, project_id: &str, terminal_id: &str) {
    let Some(ws) = shared.workspaces.lock().unwrap().get(project_id).cloned() else {
        return;
    };
    let pane = ws
        .panes
        .lock()
        .unwrap()
        .iter()
        .find(|(_, p)| p.terminal.id == terminal_id)
        .map(|(pane, _)| pane.clone());
    let Some(pane) = pane else { return };
    let Ok(client) = ws.client_for(&pane) else {
        return;
    };
    shared.runtime.spawn(async move {
        let _ = request_snapshot(&client, &pane, None).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_snapshots() {
        let cursor = Cursor {
            x: 2,
            y: 1,
            cols: 10,
            rows: 3,
            alternate: true,
            visible: false,
        };
        let out = render_snapshot(
            &[b"ab".to_vec(), Vec::new(), b"\x1b[31mc".to_vec()],
            Some(cursor),
        );
        assert_eq!(
            out,
            b"\x1bc\x1b[?1049h\x1b[1;1Hab\x1b[0m\x1b[3;1H\x1b[31mc\x1b[0m\x1b[2;3H\x1b[?25l"
        );
    }

    #[test]
    fn parses_cursor_replies() {
        let reply = Reply {
            ok: true,
            lines: vec![b"10 0 100 30 0 1".to_vec()],
        };
        let c = parse_cursor(&reply).unwrap();
        assert_eq!(
            (c.x, c.y, c.cols, c.rows, c.alternate, c.visible),
            (10, 0, 100, 30, false, true)
        );
        assert!(parse_cursor(&Reply {
            ok: true,
            lines: vec![b"1 2".to_vec()]
        })
        .is_none());
    }

    #[test]
    fn builds_send_keys() {
        assert_eq!(send_keys("%3", b"hi\r"), "send-keys -t %3 -H 68 69 0d");
    }
}
