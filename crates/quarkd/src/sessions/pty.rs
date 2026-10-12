//! Worker terminals on the PTY supervisor.
//!
//! The native engine runs workers in sessions of `quark-ptyd`, not in tmux
//! windows. It reports such a task's endpoint as `pty:<session id>`
//! ([`target`]), and [`PtyHub`] serves its terminal the way the tmux hub
//! serves a pane: same terminal ids, same `worker.output` events, same input
//! and resize.
//!
//! Each mapped terminal has one attach stream ([`SessionBackend::attach`]).
//! Its first chunk is a repaint of the screen taken in stream order, which
//! becomes the terminal's snapshot event; everything after it is output. A
//! viewer that falls behind gets a repaint from the backend, which starts
//! with a terminal reset and so renders correctly as output. A snapshot on
//! request, or after old output was pruned, opens a fresh attach: the old
//! stream stops forwarding first, so the new snapshot holds everything it
//! had not sent and nothing is sent twice.
//!
//! A relaunched worker runs in a new session; the engine names it, and the
//! terminal switches to it with a snapshot. When the program exits the
//! terminal goes away, as a tmux window does. `quark-ptyd` outlives the
//! daemon, so after a restart the terminals reattach and start again from a
//! snapshot.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use quark_core::session::{Output, SessionBackend, SessionId, TermSize};
use quark_core::CoreError;
use quark_systems::{Event, Terminal, TerminalChunkKind, TerminalRole};
use tokio::sync::oneshot;
use tokio::task::AbortHandle;

use super::{check_seq, Chunk, SessionError};

/// What starts a PTY endpoint.
pub const PREFIX: &str = "pty:";

/// The endpoint the engine reports for a task running in PTY session `id`.
pub fn target(id: &SessionId) -> String {
    format!("{PREFIX}{}", id.0)
}

/// The PTY session an endpoint names; `None` for a tmux target.
pub(super) fn parse(target: &str) -> Option<SessionId> {
    target
        .strip_prefix(PREFIX)
        .filter(|id| !id.is_empty())
        .map(|id| SessionId(id.to_string()))
}

/// Terminals backed by PTY sessions, by terminal id.
pub(super) struct PtyHub {
    backend: Arc<dyn SessionBackend>,
    sink: std::sync::mpsc::Sender<Chunk>,
    runtime: tokio::runtime::Handle,
    state: Mutex<State>,
    input_seq: Mutex<HashMap<String, u64>>,
    input_lock: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct State {
    routes: HashMap<String, Route>,
    /// Sessions whose program exited, with their Project. They are not
    /// attached again while the engine still names them.
    ended: HashMap<SessionId, String>,
    /// Bumped on every attach; a stream forwards only while its route
    /// carries its epoch.
    epoch: u64,
}

struct Route {
    session: SessionId,
    terminal: Terminal,
    epoch: u64,
    /// Listed once its first snapshot is out.
    attached: bool,
    pump: Option<AbortHandle>,
}

impl Route {
    fn stop(&mut self) {
        if let Some(pump) = self.pump.take() {
            pump.abort();
        }
    }
}

impl PtyHub {
    pub(super) fn new(
        backend: Arc<dyn SessionBackend>,
        sink: std::sync::mpsc::Sender<Chunk>,
    ) -> Arc<Self> {
        Arc::new(Self {
            backend,
            sink,
            runtime: tokio::runtime::Handle::current(),
            state: Mutex::new(State::default()),
            input_seq: Mutex::new(HashMap::new()),
            input_lock: tokio::sync::Mutex::new(()),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Replaces one Project's PTY terminals with `tasks` (terminal id and
    /// session), attaching new ones and switching any whose session changed.
    pub(super) fn sync(self: &Arc<Self>, project_id: &str, tasks: Vec<(String, SessionId)>) {
        let mut start = Vec::new();
        {
            let mut st = self.lock();
            st.routes.retain(|id, r| {
                let keep = r.terminal.project_id != project_id
                    || tasks.iter().any(|(t, s)| t == id && *s == r.session);
                if !keep {
                    r.stop();
                }
                keep
            });
            st.ended
                .retain(|s, p| p != project_id || tasks.iter().any(|(_, t)| t == s));
            for (id, session) in tasks {
                // A terminal another Project already has stays with it.
                if st.routes.contains_key(&id) || st.ended.contains_key(&session) {
                    continue;
                }
                st.routes.insert(
                    id.clone(),
                    Route {
                        terminal: Terminal {
                            id: id.clone(),
                            project_id: project_id.to_string(),
                            role: TerminalRole::Worker,
                            task_id: Some(id.clone()),
                            title: session.0.clone(),
                            cols: TermSize::default().cols,
                            rows: TermSize::default().rows,
                        },
                        session,
                        epoch: 0,
                        attached: false,
                        pump: None,
                    },
                );
                start.push(id);
            }
        }
        for id in start {
            let _ = self.attach(&id, None);
        }
    }

    /// (Re)attaches a terminal: stops its stream and opens a new one, whose
    /// snapshot `done` receives once it is recorded.
    fn attach(
        self: &Arc<Self>,
        terminal_id: &str,
        done: Option<oneshot::Sender<Event>>,
    ) -> Result<(), SessionError> {
        let mut st = self.lock();
        st.epoch += 1;
        let epoch = st.epoch;
        let route = st
            .routes
            .get_mut(terminal_id)
            .ok_or(SessionError::NotFound)?;
        route.stop();
        route.epoch = epoch;
        let pump = self.runtime.spawn(pump(
            Arc::downgrade(self),
            self.backend.clone(),
            terminal_id.to_string(),
            route.session.clone(),
            epoch,
            done,
        ));
        route.pump = Some(pump.abort_handle());
        Ok(())
    }

    /// Records a chunk from the stream of `epoch`. Returns false once that
    /// stream is no longer the terminal's, and it must stop.
    fn emit(
        &self,
        terminal_id: &str,
        epoch: u64,
        kind: TerminalChunkKind,
        data: Vec<u8>,
        size: Option<TermSize>,
        done: Option<oneshot::Sender<Event>>,
    ) -> bool {
        let mut st = self.lock();
        let Some(route) = st.routes.get_mut(terminal_id).filter(|r| r.epoch == epoch) else {
            return false;
        };
        let (cols, rows) = match kind {
            TerminalChunkKind::Snapshot => {
                if let Some(size) = size {
                    route.terminal.cols = size.cols;
                    route.terminal.rows = size.rows;
                }
                route.attached = true;
                (Some(route.terminal.cols), Some(route.terminal.rows))
            }
            TerminalChunkKind::Output => (None, None),
        };
        // Sent under the lock, so a stream replaced after this check can
        // never land behind its successor's snapshot.
        let _ = self.sink.send(Chunk {
            project_id: route.terminal.project_id.clone(),
            terminal_id: route.terminal.id.clone(),
            role: route.terminal.role,
            task_id: route.terminal.task_id.clone(),
            kind,
            data,
            cols,
            rows,
            done,
        });
        true
    }

    fn set_title(&self, terminal_id: &str, epoch: u64, title: String) {
        if let Some(r) = self
            .lock()
            .routes
            .get_mut(terminal_id)
            .filter(|r| r.epoch == epoch)
        {
            r.terminal.title = title;
        }
    }

    /// The stream of `epoch` ended. With `exited`, the program is gone and
    /// the session is not attached again; otherwise the next sync retries.
    fn ended(&self, terminal_id: &str, epoch: u64, exited: bool) {
        let mut st = self.lock();
        let Some(route) = st.routes.get(terminal_id).filter(|r| r.epoch == epoch) else {
            return;
        };
        let (session, project) = (route.session.clone(), route.terminal.project_id.clone());
        st.routes.remove(terminal_id);
        if exited {
            st.ended.insert(session, project);
        }
    }

    pub(super) fn owns(&self, terminal_id: &str) -> bool {
        self.lock().routes.contains_key(terminal_id)
    }

    pub(super) fn list(&self, project_id: &str) -> Vec<Terminal> {
        self.lock()
            .routes
            .values()
            .filter(|r| r.attached && r.terminal.project_id == project_id)
            .map(|r| r.terminal.clone())
            .collect()
    }

    pub(super) fn get(&self, terminal_id: &str) -> Result<Terminal, SessionError> {
        self.lock()
            .routes
            .get(terminal_id)
            .map(|r| r.terminal.clone())
            .ok_or(SessionError::NotFound)
    }

    fn session(&self, terminal_id: &str) -> Result<SessionId, SessionError> {
        self.lock()
            .routes
            .get(terminal_id)
            .map(|r| r.session.clone())
            .ok_or(SessionError::NotFound)
    }

    pub(super) async fn input(
        &self,
        terminal_id: &str,
        bytes: &[u8],
        seq: Option<u64>,
    ) -> Result<(), SessionError> {
        let session = self.session(terminal_id)?;
        let _guard = self.input_lock.lock().await;
        check_seq(&self.input_seq, terminal_id, seq)?;
        self.backend
            .input(&session, bytes)
            .await
            .map_err(session_error)
    }

    pub(super) async fn resize(
        &self,
        terminal_id: &str,
        size: TermSize,
    ) -> Result<Terminal, SessionError> {
        let session = self.session(terminal_id)?;
        self.backend
            .resize(&session, size)
            .await
            .map_err(session_error)?;
        let mut st = self.lock();
        let r = st
            .routes
            .get_mut(terminal_id)
            .ok_or(SessionError::NotFound)?;
        r.terminal.cols = size.cols;
        r.terminal.rows = size.rows;
        Ok(r.terminal.clone())
    }

    pub(super) async fn snapshot(
        self: &Arc<Self>,
        terminal_id: &str,
    ) -> Result<Event, SessionError> {
        let (tx, rx) = oneshot::channel();
        self.attach(terminal_id, Some(tx))?;
        rx.await
            .map_err(|_| SessionError::Unavailable("the terminal closed".into()))
    }

    /// Asks for a fresh snapshot, from the output writer after pruning.
    pub(super) fn resnapshot(self: &Arc<Self>, terminal_id: &str) {
        let _ = self.attach(terminal_id, None);
    }

    /// Stops every stream. The sessions keep running in the PTY supervisor.
    pub(super) fn detach(&self) {
        let mut st = self.lock();
        for r in st.routes.values_mut() {
            r.stop();
        }
        st.routes.clear();
        st.ended.clear();
    }
}

/// Streams one session into the terminal's events until the session ends
/// or the terminal moves to another stream.
async fn pump(
    hub: Weak<PtyHub>,
    backend: Arc<dyn SessionBackend>,
    terminal_id: String,
    session: SessionId,
    epoch: u64,
    mut done: Option<oneshot::Sender<Event>>,
) {
    let end = |exited: bool| {
        if let Some(hub) = hub.upgrade() {
            hub.ended(&terminal_id, epoch, exited);
        }
    };
    let info = match backend.list().await {
        Ok(all) => all.into_iter().find(|s| s.id == session),
        Err(e) => {
            tracing::debug!(terminal = %terminal_id, error = %e, "listing PTY sessions failed");
            return end(false);
        }
    };
    let Some(info) = info.filter(|i| i.alive) else {
        return end(true);
    };
    if let Some(hub) = hub.upgrade() {
        hub.set_title(&terminal_id, epoch, info.name);
    }
    // The size the snapshot is drawn at; the stream does not carry it.
    let size = backend.snapshot(&session).await.ok().map(|s| s.size);
    let mut stream = match backend.attach(&session).await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(terminal = %terminal_id, error = %e, "PTY attach failed");
            return end(matches!(e, CoreError::NotFound(_)));
        }
    };
    let mut kind = TerminalChunkKind::Snapshot;
    while let Some(out) = stream.next().await {
        let Some(hub) = hub.upgrade() else { return };
        match out {
            Output::Bytes { data } => {
                if !hub.emit(&terminal_id, epoch, kind, data, size, done.take()) {
                    return;
                }
                kind = TerminalChunkKind::Output;
            }
            Output::Exited { .. } => return hub.ended(&terminal_id, epoch, true),
        }
    }
    end(false);
}

/// A PTY supervisor error as a session error.
fn session_error(e: CoreError) -> SessionError {
    match e {
        CoreError::NotFound(_) => SessionError::NotFound,
        CoreError::Invalid(m) | CoreError::Refused(m) => SessionError::Invalid(m),
        other => SessionError::Unavailable(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_name_pty_sessions_only() {
        let id = SessionId("pty-12-3".into());
        assert_eq!(parse(&target(&id)), Some(id));
        assert_eq!(parse("quark:fm-fix-login"), None);
        assert_eq!(parse("pty:"), None);
    }
}
