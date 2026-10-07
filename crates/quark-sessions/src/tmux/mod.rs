//! tmux as a [`SessionBackend`].
//!
//! Every session is one window on a private tmux server, and its id is the
//! window's first pane id (`%7`). tmux owns the programs, so sessions survive
//! a daemon crash or restart: [`TmuxBackend::list`] finds them again on the
//! server. The task a session runs for is kept in the window's `@quark_task`
//! option, so it survives too.
//!
//! Output comes from one tmux control-mode client ([`control`]) per tmux
//! session, attached the first time a viewer attaches. Viewer streams start
//! with a repaint taken in stream order, and tmux pausing a pane the client
//! fell behind on is answered with a fresh repaint. tmux does not report
//! exit codes: a session whose window closed ends with
//! `Exited { code: None }`.

pub mod control;
pub mod server;
pub mod snapshot;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use quark_core::session::{
    OutputStream, SessionBackend, SessionId, SessionInfo, SessionSpec, Snapshot, TermSize,
};
use quark_core::{CoreError, Result, TaskId};
use tokio::sync::broadcast;

use crate::stream::{Chunk, Resync, Viewer, CHANNEL};
use control::{ControlClient, ControlError, Handler, Reply, Waiter};
use server::{Server, ServerError, WindowSpec};
use snapshot::{parse_cursor, render_snapshot, send_keys, Cursor, CURSOR_FORMAT};

/// tmux pauses a pane this many seconds behind and the backend repaints it.
const PAUSE_AFTER_SECS: u32 = 5;
/// Bytes typed per `send-keys` command.
const INPUT_CHUNK: usize = 512;
/// Attempts at listing panes after a window or the server went away.
const REAP_TRIES: u32 = 20;
const REAP_RETRY: std::time::Duration = std::time::Duration::from_millis(25);
/// Window option holding the session's task id.
const TASK_OPTION: &str = "@quark_task";

const LIST_FORMAT: &str =
    "#{session_id}|#{window_id}|#{pane_id}|#{pane_index}|#{@quark_task}|#{window_name}";

/// Sessions on one private tmux server. Cheap to clone.
#[derive(Clone)]
pub struct TmuxBackend {
    inner: Arc<Inner>,
}

struct Inner {
    server: Server,
    /// Control clients by tmux session id.
    clients: Mutex<HashMap<String, ControlClient<Tag>>>,
    /// Viewer broadcasts by pane id.
    feeds: Mutex<HashMap<String, broadcast::Sender<Chunk>>>,
    /// Last cursor reply per pane, for the next repaint.
    cursors: Mutex<HashMap<String, Cursor>>,
}

/// One row of [`LIST_FORMAT`].
struct Pane {
    session_id: String,
    window_id: String,
    pane_id: String,
    pane_index: u32,
    name: String,
    task: Option<TaskId>,
}

impl TmuxBackend {
    /// A backend for the server at `server`'s socket. Nothing starts until
    /// the first [`SessionBackend::create`].
    pub fn new(server: Server) -> Self {
        Self {
            inner: Arc::new(Inner {
                server,
                clients: Mutex::new(HashMap::new()),
                feeds: Mutex::new(HashMap::new()),
                cursors: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn server(&self) -> &Server {
        &self.inner.server
    }

    async fn panes(&self) -> Result<Vec<Pane>> {
        if !self.inner.server.is_running().await {
            return Ok(Vec::new());
        }
        let out = self
            .inner
            .server
            .run(&["list-panes", "-a", "-F", LIST_FORMAT])
            .await
            .map_err(backend)?;
        let mut panes: Vec<Pane> = out.lines().filter_map(parse_pane).collect();
        // A session is a window; its first pane stands for it.
        panes.sort_by(|a, b| (&a.window_id, a.pane_index).cmp(&(&b.window_id, b.pane_index)));
        panes.dedup_by(|b, a| a.window_id == b.window_id);
        Ok(panes)
    }

    async fn pane(&self, id: &SessionId) -> Result<Pane> {
        self.panes()
            .await?
            .into_iter()
            .find(|p| p.pane_id == id.0)
            .ok_or_else(|| CoreError::NotFound(format!("session {}", id.0)))
    }

    fn client(&self, session_id: &str) -> Result<ControlClient<Tag>> {
        let mut clients = self.inner.clients.lock().unwrap();
        if let Some(c) = clients.get(session_id).filter(|c| !c.is_closed()) {
            return Ok(c.clone());
        }
        let handler = Reader {
            session_id: session_id.to_string(),
            inner: Arc::downgrade(&self.inner),
        };
        let client = ControlClient::attach(
            self.inner.server.tmux(),
            self.inner.server.socket(),
            session_id,
            PAUSE_AFTER_SECS,
            handler,
        )
        .map_err(|e| CoreError::Backend(format!("tmux control attach: {e}")))?;
        clients.insert(session_id.to_string(), client.clone());
        Ok(client)
    }

    /// Sends the pane's cursor query and capture down `client`; the reader
    /// broadcasts the repaint when the capture reply arrives, in stream
    /// order with the pane's output.
    async fn request_repaint(client: &ControlClient<Tag>, pane: &str) -> Result<(), ControlError> {
        client
            .send(
                &format!("display-message -p -t {pane} '{CURSOR_FORMAT}'"),
                Waiter::Reader(Tag::Cursor(pane.to_string())),
            )
            .await?;
        client
            .send(
                &format!("capture-pane -p -e -t {pane}"),
                Waiter::Reader(Tag::Screen(pane.to_string())),
            )
            .await
    }

    fn resync(&self, client: ControlClient<Tag>, pane: String) -> Resync {
        Arc::new(move || {
            let client = client.clone();
            let pane = pane.clone();
            tokio::spawn(async move {
                let _ = Self::request_repaint(&client, &pane).await;
            });
        })
    }

    /// Detaches every control client. The server and its programs keep
    /// running; open viewer streams end.
    pub fn detach_all(&self) {
        for (_, c) in self.inner.clients.lock().unwrap().drain() {
            c.detach();
        }
        self.inner.feeds.lock().unwrap().clear();
    }
}

fn parse_pane(line: &str) -> Option<Pane> {
    let mut f = line.splitn(6, '|');
    let session_id = f.next()?.to_string();
    let window_id = f.next()?.to_string();
    let pane_id = f.next()?.to_string();
    let pane_index = f.next()?.parse().ok()?;
    let task = Some(f.next()?).filter(|t| !t.is_empty()).map(TaskId::new);
    let name = f.next()?.to_string();
    Some(Pane {
        session_id,
        window_id,
        pane_id,
        pane_index,
        name,
        task,
    })
}

fn backend(e: impl std::fmt::Display) -> CoreError {
    CoreError::Backend(e.to_string())
}

fn check_size(size: TermSize) -> Result<()> {
    if !(2..=1000).contains(&size.cols) || !(2..=1000).contains(&size.rows) {
        return Err(CoreError::Invalid(format!(
            "size {}x{} is out of range",
            size.cols, size.rows
        )));
    }
    Ok(())
}

fn info(p: Pane) -> SessionInfo {
    SessionInfo {
        id: SessionId(p.pane_id),
        name: p.name,
        task: p.task,
        alive: true,
        exit_code: None,
    }
}

#[async_trait]
impl SessionBackend for TmuxBackend {
    fn name(&self) -> &'static str {
        "tmux"
    }

    async fn create(&self, spec: &SessionSpec) -> Result<SessionInfo> {
        check_size(spec.size)?;
        let server = &self.inner.server;
        let target = server
            .start_window(&WindowSpec {
                name: spec.name.clone(),
                argv: spec.argv.clone(),
                cwd: Some(spec.cwd.clone()),
                env: spec
                    .env
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            })
            .await
            .map_err(|e| match e {
                ServerError::Parse(m) => CoreError::Invalid(m),
                e => backend(e),
            })?;
        let pane = server
            .run(&["display-message", "-p", "-t", &target, "#{pane_id}"])
            .await
            .map_err(backend)?
            .trim()
            .to_string();
        if let Some(task) = &spec.task {
            server
                .run(&["set-option", "-w", "-t", &pane, TASK_OPTION, task.as_str()])
                .await
                .map_err(backend)?;
        }
        self.resize(&SessionId(pane.clone()), spec.size).await?;
        Ok(SessionInfo {
            id: SessionId(pane),
            name: spec.name.clone(),
            task: spec.task.clone(),
            alive: true,
            exit_code: None,
        })
    }

    async fn attach(&self, id: &SessionId) -> Result<Box<dyn OutputStream>> {
        let pane = self.pane(id).await?;
        let client = self.client(&pane.session_id)?;
        let rx = self
            .inner
            .feeds
            .lock()
            .unwrap()
            .entry(pane.pane_id.clone())
            .or_insert_with(|| broadcast::channel(CHANNEL).0)
            .subscribe();
        Self::request_repaint(&client, &pane.pane_id)
            .await
            .map_err(backend)?;
        let resync = self.resync(client, pane.pane_id);
        Ok(Box::new(Viewer::new(rx, Vec::new(), resync)))
    }

    async fn input(&self, id: &SessionId, bytes: &[u8]) -> Result<()> {
        let pane = self.pane(id).await?;
        let client = self.client(&pane.session_id)?;
        for chunk in bytes.chunks(INPUT_CHUNK) {
            client
                .run(&send_keys(&pane.pane_id, chunk))
                .await
                .map_err(backend)?;
        }
        Ok(())
    }

    async fn resize(&self, id: &SessionId, size: TermSize) -> Result<()> {
        check_size(size)?;
        // resize-window also switches this one window to manual sizing; the
        // global `window-size manual` option crashes tmux 3.4.
        self.inner
            .server
            .run(&[
                "resize-window",
                "-t",
                &id.0,
                "-x",
                &size.cols.to_string(),
                "-y",
                &size.rows.to_string(),
            ])
            .await
            .map_err(|e| not_found_or(e, id))?;
        Ok(())
    }

    async fn snapshot(&self, id: &SessionId) -> Result<Snapshot> {
        let server = &self.inner.server;
        let cursor = server
            .run(&["display-message", "-p", "-t", &id.0, CURSOR_FORMAT])
            .await
            .map_err(|e| not_found_or(e, id))?;
        let cursor = parse_cursor(cursor.trim_end().as_bytes())
            .ok_or_else(|| CoreError::Backend(format!("bad cursor reply {cursor:?}")))?;
        let screen = server
            .run(&["capture-pane", "-p", "-e", "-t", &id.0])
            .await
            .map_err(|e| not_found_or(e, id))?;
        let lines: Vec<Vec<u8>> = screen.lines().map(|l| l.as_bytes().to_vec()).collect();
        Ok(Snapshot {
            size: TermSize {
                cols: cursor.cols,
                rows: cursor.rows,
            },
            bytes: render_snapshot(&lines, Some(cursor)),
        })
    }

    async fn kill(&self, id: &SessionId) -> Result<()> {
        self.inner
            .server
            .run(&["kill-window", "-t", &id.0])
            .await
            .map_err(|e| not_found_or(e, id))?;
        Ok(())
    }

    async fn list(&self) -> Result<Vec<SessionInfo>> {
        Ok(self.panes().await?.into_iter().map(info).collect())
    }
}

fn not_found_or(e: ServerError, id: &SessionId) -> CoreError {
    match &e {
        ServerError::Failed { stderr, .. }
            if stderr.contains("can't find") || stderr.contains("no server running") =>
        {
            CoreError::NotFound(format!("session {}", id.0))
        }
        _ => backend(e),
    }
}

/// Pane ids on the server; empty once the server is gone. A server that is
/// shutting down answers with errors such as "server exited unexpectedly",
/// so a failed listing is retried until it either succeeds or the server
/// has stopped. `None` only when a running server keeps failing.
async fn live_panes(server: &Server) -> Option<Vec<String>> {
    for attempt in 1..=REAP_TRIES {
        match server.run(&["list-panes", "-a", "-F", "#{pane_id}"]).await {
            Ok(out) => return Some(out.lines().map(str::to_string).collect()),
            Err(e) => {
                if !server.is_running().await {
                    return Some(Vec::new());
                }
                tracing::debug!(error = %e, attempt, "listing tmux panes failed");
                tokio::time::sleep(REAP_RETRY).await;
            }
        }
    }
    None
}

/// Context for replies the control reader handles in stream order.
enum Tag {
    Cursor(String),
    Screen(String),
}

/// Routes one control client's output to the viewer broadcasts.
struct Reader {
    session_id: String,
    inner: Weak<Inner>,
}

impl Reader {
    fn send(&self, pane: &str, chunk: Chunk) {
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let tx = inner.feeds.lock().unwrap().get(pane).cloned();
        if let Some(tx) = tx {
            let _ = tx.send(chunk);
        }
    }

    /// Ends the streams of panes no longer on the server.
    fn reap(&self) {
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let backend = TmuxBackend { inner };
        tokio::spawn(async move {
            let Some(live) = live_panes(&backend.inner.server).await else {
                return;
            };
            backend.inner.feeds.lock().unwrap().retain(|pane, tx| {
                let alive = live.contains(pane);
                if !alive {
                    let _ = tx.send(Chunk::exited(None));
                }
                alive
            });
        });
    }
}

impl Handler for Reader {
    type Tag = Tag;

    fn output(&mut self, pane: &str, data: Vec<u8>) {
        self.send(pane, Chunk::bytes(data));
    }

    fn pause(&mut self, client: &ControlClient<Tag>, pane: &str) {
        // tmux dropped this pane's queued output. Repaint, then continue.
        let client = client.clone();
        let pane = pane.to_string();
        tokio::spawn(async move {
            if TmuxBackend::request_repaint(&client, &pane).await.is_ok() {
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
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        match tag {
            Tag::Cursor(pane) => {
                let cursor = reply
                    .ok
                    .then(|| reply.lines.first().and_then(|l| parse_cursor(l)))
                    .flatten();
                let mut cursors = inner.cursors.lock().unwrap();
                match cursor {
                    Some(c) => cursors.insert(pane, c),
                    None => cursors.remove(&pane),
                };
            }
            Tag::Screen(pane) => {
                if !reply.ok {
                    tracing::debug!(pane, error = %reply.error_text(), "capture-pane failed");
                    return;
                }
                let cursor = inner.cursors.lock().unwrap().get(&pane).copied();
                drop(inner);
                self.send(&pane, Chunk::repaint(render_snapshot(&reply.lines, cursor)));
            }
        }
    }

    fn layout(&mut self) {
        self.reap();
    }

    fn closed(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            inner.clients.lock().unwrap().remove(&self.session_id);
        }
        self.reap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_list_rows() {
        let p = parse_pane("$1|@2|%4|0|t-1|work|er").unwrap();
        assert_eq!(p.pane_id, "%4");
        assert_eq!(p.name, "work|er");
        assert_eq!(p.task, Some(TaskId::new("t-1")));
        assert!(parse_pane("$1|@2|%4|0||w").unwrap().task.is_none());
        assert!(parse_pane("junk").is_none());
    }
}
