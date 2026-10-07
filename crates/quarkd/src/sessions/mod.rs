//! Terminal sessions: coordinator and worker panes streamed from tmux.
//!
//! quarkd runs one private tmux server for Quark, on
//! `<quark home>/run/tmux/quark`. The command center and every Project
//! coordinator run on it: engine calls get [`Sessions::tmux_env`] as `TMUX`,
//! and firstmate creates every window in the server it finds there, so
//! coordinators and the workers they spawn all land on this server.
//! Firstmate records no socket per task, which is why the server is shared
//! rather than one per Project.
//!
//! For every session on the server the daemon attaches one tmux control-mode
//! client ([`control`]), which delivers every pane's output as raw bytes.
//! Panes are mapped to terminals by their `session:window` target:
//!
//! - [`Sessions::set_coordinator`] names a Project's coordinator window; its
//!   terminal id is the Project id;
//! - [`Sessions::sync`] names a Project's task windows, from the engine
//!   endpoints in each snapshot; a task's terminal id is the task id.
//!
//! Output from mapped panes becomes `worker.output` events. The first event
//! for a pane is a snapshot of its screen, taken in stream order, and a new
//! snapshot follows whenever output had to be dropped (tmux paused a pane
//! the daemon fell behind on) or old output was pruned. After a daemon
//! restart the server is still running, so terminals reattach where they
//! were and start again from a snapshot.

pub use quark_sessions::tmux::{control, server};

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
use quark_sessions::tmux::snapshot::{
    parse_cursor, render_snapshot, send_keys, Cursor, CURSOR_FORMAT,
};
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
/// Socket name of the shared server under `<run dir>/tmux/`.
const SOCKET_NAME: &str = "quark";

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

/// The daemon's terminal sessions. Cheap to clone.
#[derive(Clone)]
pub struct Sessions {
    shared: Option<Arc<Shared>>,
    reason: Arc<str>,
}

struct Shared {
    hub: Arc<Hub>,
    runtime: tokio::runtime::Handle,
}

impl Sessions {
    /// Sessions backed by the `tmux` binary, with the server socket under
    /// `run_dir`. Output is written to `store` by a dedicated thread. Must be
    /// called from within a Tokio runtime.
    pub fn new(
        tmux: impl Into<PathBuf>,
        run_dir: impl Into<PathBuf>,
        store: Arc<Store>,
    ) -> Result<Self, SessionError> {
        let server = Server::new(tmux, run_dir.into().join("tmux").join(SOCKET_NAME))?;
        let (sink, rx) = std::sync::mpsc::channel();
        let shared = Arc::new(Shared {
            hub: Hub::start(server, sink),
            runtime: tokio::runtime::Handle::current(),
        });
        let weak = Arc::downgrade(&shared);
        std::thread::Builder::new()
            .name("terminal-output".into())
            .spawn(move || write_output(rx, store, weak))
            .expect("spawn terminal output writer");
        Ok(Self {
            shared: Some(shared),
            reason: Arc::from(""),
        })
    }

    /// Sessions that report `reason` for every call, e.g. without tmux.
    pub fn disabled(reason: impl Into<String>) -> Self {
        Self {
            shared: None,
            reason: Arc::from(reason.into()),
        }
    }

    /// The tmux binary to use: `tmux` from `PATH` unless given, checked by
    /// running `tmux -V`. Disabled sessions when it does not run.
    pub fn detect(tmux: Option<&Path>, run_dir: PathBuf, store: Arc<Store>) -> Self {
        let bin = tmux.map(Path::to_path_buf).unwrap_or_else(|| "tmux".into());
        let version = match std::process::Command::new(&bin).arg("-V").output() {
            Ok(out) if out.status.success() => {
                String::from_utf8_lossy(&out.stdout).trim().to_string()
            }
            _ => {
                let reason = format!("{} is not installed or does not run", bin.display());
                tracing::warn!(%reason, "terminal sessions disabled");
                return Self::disabled(reason);
            }
        };
        match Self::new(bin, run_dir, store) {
            Ok(s) => {
                tracing::info!(%version, "terminal sessions enabled");
                s
            }
            Err(e) => {
                tracing::warn!(error = %e, "terminal sessions disabled");
                Self::disabled(e.to_string())
            }
        }
    }

    fn hub(&self) -> Result<&Arc<Hub>, SessionError> {
        self.shared
            .as_ref()
            .map(|s| &s.hub)
            .ok_or_else(|| SessionError::Unavailable(self.reason.to_string()))
    }

    /// The shared tmux server.
    pub fn server(&self) -> Result<&Server, SessionError> {
        Ok(&self.hub()?.server)
    }

    /// The `TMUX` value that points engine calls (and every program they
    /// start) at the shared server. Firstmate only reads the socket path.
    pub fn tmux_env(&self) -> Result<String, SessionError> {
        Ok(format!("{},0,0", self.server()?.socket().display()))
    }

    /// Starts the shared server, with its `quark` session, if it is not
    /// running. Call before the first engine call that opens a window.
    pub async fn ensure_server(&self) -> Result<(), SessionError> {
        let hub = self.hub()?;
        hub.server.ensure_session().await?;
        hub.rescan().await;
        Ok(())
    }

    /// Starts a program in a new window of the `quark` session, starting the
    /// server first if needed. Returns the window's `session:window` target.
    pub async fn start_window(&self, spec: &WindowSpec) -> Result<String, SessionError> {
        let hub = self.hub()?;
        let target = hub.server.start_window(spec).await?;
        hub.rescan().await;
        Ok(target)
    }

    /// Names (or, with `None`, forgets) the window a Project's coordinator
    /// runs in, as the `session:window` target the engine reported.
    pub async fn set_coordinator(
        &self,
        project_id: &str,
        target: Option<String>,
    ) -> Result<(), SessionError> {
        let hub = self.hub()?;
        {
            let mut targets = hub.targets.lock().unwrap();
            targets.retain(|_, r| {
                !(r.project_id == project_id && r.role == TerminalRole::Coordinator)
            });
            if let Some(target) = target {
                targets.insert(
                    target,
                    Owner {
                        project_id: project_id.to_string(),
                        role: TerminalRole::Coordinator,
                        id: project_id.to_string(),
                    },
                );
            }
        }
        hub.rescan().await;
        Ok(())
    }

    /// Replaces the task window targets of one Project and reconciles the
    /// panes. Called after every projection refresh.
    pub async fn sync(&self, project_id: &str, tasks: Vec<TaskTarget>) -> Result<(), SessionError> {
        let hub = self.hub()?;
        {
            let mut targets = hub.targets.lock().unwrap();
            targets.retain(|_, r| !(r.project_id == project_id && r.role == TerminalRole::Worker));
            for t in tasks {
                // A target another Project already claims stays with it.
                targets.entry(t.target).or_insert(Owner {
                    project_id: project_id.to_string(),
                    role: TerminalRole::Worker,
                    id: t.task_id,
                });
            }
        }
        hub.rescan().await;
        Ok(())
    }

    /// Live terminals of one Project, coordinator first.
    pub fn list(&self, project_id: &str) -> Vec<Terminal> {
        let Ok(hub) = self.hub() else {
            return Vec::new();
        };
        let mut out: Vec<Terminal> = hub
            .panes
            .lock()
            .unwrap()
            .values()
            .filter(|p| p.terminal.project_id == project_id)
            .map(|p| p.terminal.clone())
            .collect();
        out.sort_by(|a, b| (a.role as u8, &a.id).cmp(&(b.role as u8, &b.id)));
        out
    }

    pub fn get(&self, terminal_id: &str) -> Result<Terminal, SessionError> {
        let hub = self.hub()?;
        let pane = hub.find(terminal_id)?;
        let panes = hub.panes.lock().unwrap();
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
        let hub = self.hub()?;
        let pane = hub.find(terminal_id)?;
        let client = hub.client_for(&pane)?;
        // Hold the input lock while typing so concurrent requests are applied
        // in sequence order or refused.
        let _guard = hub.input_lock.lock().await;
        if let Some(seq) = seq {
            let mut last = hub.input_seq.lock().unwrap();
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
        let hub = self.hub()?;
        let pane = hub.find(terminal_id)?;
        let client = hub.client_for(&pane)?;
        let window = {
            let panes = hub.panes.lock().unwrap();
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
        let mut panes = hub.panes.lock().unwrap();
        let p = panes.get_mut(&pane).ok_or(SessionError::NotFound)?;
        p.terminal.cols = cols;
        p.terminal.rows = rows;
        Ok(p.terminal.clone())
    }

    /// Appends a fresh snapshot of the terminal's screen to the event stream
    /// and returns that event. A client opening a terminal applies it, then
    /// the terminal's `worker.output` events with a greater `seq`.
    pub async fn snapshot(&self, terminal_id: &str) -> Result<Event, SessionError> {
        let hub = self.hub()?;
        let pane = hub.find(terminal_id)?;
        let client = hub.client_for(&pane)?;
        let (tx, rx) = oneshot::channel();
        request_snapshot(&client, &pane, Some(tx)).await?;
        rx.await
            .map_err(|_| SessionError::Unavailable("the terminal closed".into()))
    }

    /// Detaches every control client. The tmux server and its programs keep
    /// running, so a later daemon reattaches to them.
    pub fn detach_all(&self) {
        if let Ok(hub) = self.hub() {
            hub.detach();
        }
    }
}

/// Who a window target belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Owner {
    project_id: String,
    role: TerminalRole,
    /// Terminal id: the task id, or the Project id for a coordinator.
    id: String,
}

/// The shared server, its control clients and the pane map.
struct Hub {
    server: Server,
    sink: std::sync::mpsc::Sender<Chunk>,
    /// Window target (`session:window`) to owner.
    targets: Mutex<HashMap<String, Owner>>,
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

impl Hub {
    fn start(server: Server, sink: std::sync::mpsc::Sender<Chunk>) -> Arc<Self> {
        let hub = Arc::new(Hub {
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
        let weak = Arc::downgrade(&hub);
        let notify = hub.layout_changed.clone();
        tokio::spawn(async move {
            loop {
                notify.notified().await;
                tokio::time::sleep(RESCAN_DEBOUNCE).await;
                let Some(hub) = weak.upgrade() else { return };
                hub.rescan().await;
            }
        });
        hub
    }

    fn find(&self, terminal_id: &str) -> Result<String, SessionError> {
        self.panes
            .lock()
            .unwrap()
            .iter()
            .find(|(_, p)| p.terminal.id == terminal_id)
            .map(|(pane, _)| pane.clone())
            .ok_or(SessionError::NotFound)
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
                tracing::warn!(error = %e, "listing tmux panes failed");
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
                    session_id: session.to_string(),
                    panes: self.panes.clone(),
                    sink: self.sink.clone(),
                    layout_changed: self.layout_changed.clone(),
                    hub: Arc::downgrade(self),
                };
                match ControlClient::attach(
                    self.server.tmux(),
                    self.server.socket(),
                    session,
                    PAUSE_AFTER_SECS,
                    handler,
                ) {
                    Ok(c) => {
                        tracing::info!(session, "attached tmux control client");
                        clients.insert(session.to_string(), c);
                        fresh.push(session.to_string());
                    }
                    Err(e) => {
                        tracing::warn!(session, error = %e, "tmux control attach failed")
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
            let Some(owner) = targets.get(&p.target) else {
                continue;
            };
            let title = p.target.split_once(':').map_or(p.target.as_str(), |t| t.1);
            wanted.insert(
                p.pane_id.clone(),
                PaneRoute {
                    session_id: p.session_id.clone(),
                    window_id: p.window_id.clone(),
                    terminal: Terminal {
                        id: owner.id.clone(),
                        project_id: owner.project_id.clone(),
                        role: owner.role,
                        task_id: (owner.role == TerminalRole::Worker).then(|| owner.id.clone()),
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
                    tracing::warn!(pane, error = %e, "snapshot request failed");
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
            &format!("display-message -p -t {pane} '{CURSOR_FORMAT}'"),
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
    session_id: String,
    panes: Arc<Mutex<HashMap<String, PaneRoute>>>,
    sink: std::sync::mpsc::Sender<Chunk>,
    layout_changed: Arc<Notify>,
    hub: Weak<Hub>,
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
            project_id: route.terminal.project_id.clone(),
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
        // Let the pane continue, then resync from a snapshot. In this order
        // nothing is lost: output before the capture is in it, and output
        // after it follows in stream order. Continuing after the capture
        // would let tmux drop whatever the pane wrote in between.
        if let Some(route) = self.panes.lock().unwrap().get_mut(pane) {
            route.live = false;
        }
        tracing::debug!(pane, "tmux paused pane; resyncing");
        let client = client.clone();
        let pane = pane.to_string();
        tokio::spawn(async move {
            let resumed = client
                .send(
                    &format!("refresh-client -A '{pane}:continue'"),
                    Waiter::Discard,
                )
                .await;
            if resumed.is_ok() {
                let _ = request_snapshot(&client, &pane, None).await;
            }
        });
    }

    fn reply(&mut self, tag: Tag, reply: Reply) {
        match tag {
            Tag::Cursor(pane) => {
                let cursor = reply
                    .ok
                    .then(|| reply.lines.first().and_then(|l| parse_cursor(l)))
                    .flatten();
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
                        tracing::warn!(pane, error = %reply.error_text(), "capture-pane failed");
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
        tracing::info!(session = %self.session_id, "tmux control client closed");
        if let Some(hub) = self.hub.upgrade() {
            hub.clients.lock().unwrap().remove(&self.session_id);
            hub.panes
                .lock()
                .unwrap()
                .retain(|_, r| r.session_id != self.session_id);
            hub.layout_changed.notify_one();
        }
    }
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
                    resnapshot(&shared, &chunk.terminal_id);
                }
            }
        }
    }
}

/// Requests a snapshot of a terminal from the writer thread.
fn resnapshot(shared: &Shared, terminal_id: &str) {
    let hub = &shared.hub;
    let Ok(pane) = hub.find(terminal_id) else {
        return;
    };
    let Ok(client) = hub.client_for(&pane) else {
        return;
    };
    shared.runtime.spawn(async move {
        let _ = request_snapshot(&client, &pane, None).await;
    });
}
