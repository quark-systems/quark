//! The PTY supervisor: pseudo-terminals owned by Quark itself.
//!
//! [`PtySupervisor`] starts each program on its own pseudo-terminal in a new
//! session, reads its output on a dedicated thread, and runs every byte
//! through a screen model ([`vt100`]) so a snapshot is the screen as it is
//! now, not a replay of history. Sessions stay in the supervisor after their
//! program exits, with the exit code, until they are killed.
//!
//! The supervisor lives as long as its process. For sessions that outlive
//! the daemon, the `quark-ptyd` binary runs one on a Unix socket
//! ([`serve`]) and the daemon talks to it through [`PtyClient`], which
//! starts `quark-ptyd` when nothing is listening. A daemon crash then drops
//! only connections: the programs keep running and a new daemon finds them
//! with [`SessionBackend::list`]. Losing `quark-ptyd` itself ends its
//! sessions, as losing the tmux server ends tmux's.

mod client;
mod proto;
mod serve;
mod spawn;

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::os::fd::AsFd as _;
use std::os::unix::process::ExitStatusExt as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use quark_core::session::{
    OutputStream, SessionBackend, SessionId, SessionInfo, SessionSpec, Snapshot, TermSize,
};
use quark_core::{CoreError, Result};
use rustix::process::{kill_process_group, Pid, Signal};
use tokio::sync::broadcast;

use crate::stream::{Chunk, Viewer, CHANNEL};
pub use client::PtyClient;
pub use serve::serve;

/// Rows of scrollback each screen model keeps.
const SCROLLBACK: usize = 10_000;
/// How long a killed program has between SIGHUP and SIGKILL.
const KILL_GRACE: Duration = Duration::from_secs(3);

/// Pseudo-terminal sessions owned by this process. Cheap to clone.
#[derive(Clone, Default)]
pub struct PtySupervisor {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    sessions: Mutex<BTreeMap<SessionId, Arc<Session>>>,
    next: AtomicU64,
}

struct Session {
    info: Mutex<SessionInfo>,
    /// The screen model. Output is applied and broadcast under this lock,
    /// so a repaint taken under it lines up exactly with the stream.
    screen: Mutex<vt100::Parser>,
    tx: broadcast::Sender<Chunk>,
    master: Mutex<std::fs::File>,
    pid: Pid,
}

impl Session {
    fn repaint(&self, screen: &vt100::Parser) -> Vec<u8> {
        let s = screen.screen();
        let mut out = b"\x1bc".to_vec();
        if s.alternate_screen() {
            out.extend_from_slice(b"\x1b[?1049h");
        }
        out.extend_from_slice(&s.state_formatted());
        out
    }

    fn size(screen: &vt100::Parser) -> TermSize {
        let (rows, cols) = screen.screen().size();
        TermSize { cols, rows }
    }
}

impl PtySupervisor {
    pub fn new() -> Self {
        Self::default()
    }

    fn get(&self, id: &SessionId) -> Result<Arc<Session>> {
        self.inner
            .sessions
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| CoreError::NotFound(format!("session {}", id.0)))
    }
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

/// Reads a session's output until its program is gone, then records the
/// exit.
fn pump(session: Weak<Session>, mut master: std::fs::File, mut child: std::process::Child) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        // EIO is how Linux reports that every program-side descriptor closed.
        let n = match master.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let Some(s) = session.upgrade() else { break };
        let mut screen = s.screen.lock().unwrap();
        screen.process(&buf[..n]);
        let _ = s.tx.send(Chunk::bytes(buf[..n].to_vec()));
    }
    let code = child
        .wait()
        .ok()
        .and_then(|st| st.code().or_else(|| st.signal().map(|sig| 128 + sig)));
    if let Some(s) = session.upgrade() {
        let _screen = s.screen.lock().unwrap();
        {
            let mut info = s.info.lock().unwrap();
            info.alive = false;
            info.exit_code = code;
        }
        let _ = s.tx.send(Chunk::exited(code));
    }
}

#[async_trait]
impl SessionBackend for PtySupervisor {
    fn name(&self) -> &'static str {
        "pty"
    }

    async fn create(&self, spec: &SessionSpec) -> Result<SessionInfo> {
        check_size(spec.size)?;
        if spec.name.is_empty() {
            return Err(CoreError::Invalid("session name is empty".into()));
        }
        let spec = spec.clone();
        let spawned = tokio::task::spawn_blocking(move || spawn::spawn(&spec).map(|s| (s, spec)))
            .await
            .map_err(|e| CoreError::Backend(e.to_string()))?
            .map_err(|e| CoreError::Backend(format!("starting the program: {e}")))?;
        let (spawned, spec) = spawned;
        let n = self.inner.next.fetch_add(1, Ordering::Relaxed) + 1;
        let id = SessionId(format!("pty-{}-{n}", std::process::id()));
        let info = SessionInfo {
            id: id.clone(),
            name: spec.name.clone(),
            task: spec.task.clone(),
            alive: true,
            exit_code: None,
        };
        let reader = spawned
            .master
            .try_clone()
            .map_err(|e| CoreError::Backend(e.to_string()))?;
        let pid = Pid::from_child(&spawned.child);
        let session = Arc::new(Session {
            info: Mutex::new(info.clone()),
            screen: Mutex::new(vt100::Parser::new(
                spec.size.rows,
                spec.size.cols,
                SCROLLBACK,
            )),
            tx: broadcast::channel(CHANNEL).0,
            master: Mutex::new(spawned.master),
            pid,
        });
        let weak = Arc::downgrade(&session);
        let child = spawned.child;
        std::thread::Builder::new()
            .name(format!("pty {}", id.0))
            .spawn(move || pump(weak, reader, child))
            .map_err(|e| CoreError::Backend(e.to_string()))?;
        self.inner.sessions.lock().unwrap().insert(id, session);
        Ok(info)
    }

    async fn attach(&self, id: &SessionId) -> Result<Box<dyn OutputStream>> {
        let s = self.get(id)?;
        let (rx, first) = {
            let screen = s.screen.lock().unwrap();
            let mut first = vec![Chunk::repaint(s.repaint(&screen))];
            let info = s.info.lock().unwrap();
            if !info.alive {
                first.push(Chunk::exited(info.exit_code));
            }
            (s.tx.subscribe(), first)
        };
        let weak = Arc::downgrade(&s);
        let resync = Arc::new(move || {
            if let Some(s) = weak.upgrade() {
                let screen = s.screen.lock().unwrap();
                let _ = s.tx.send(Chunk::repaint(s.repaint(&screen)));
            }
        });
        Ok(Box::new(Viewer::new(rx, first, resync)))
    }

    async fn input(&self, id: &SessionId, bytes: &[u8]) -> Result<()> {
        let s = self.get(id)?;
        if !s.info.lock().unwrap().alive {
            return Err(CoreError::Refused("the program has exited".into()));
        }
        let bytes = bytes.to_vec();
        tokio::task::spawn_blocking(move || {
            let mut master = s.master.lock().unwrap();
            master.write_all(&bytes)?;
            master.flush()
        })
        .await
        .map_err(|e| CoreError::Backend(e.to_string()))?
        .map_err(|e| CoreError::Backend(format!("typing into the session: {e}")))
    }

    async fn resize(&self, id: &SessionId, size: TermSize) -> Result<()> {
        check_size(size)?;
        let s = self.get(id)?;
        let mut screen = s.screen.lock().unwrap();
        spawn::set_size(s.master.lock().unwrap().as_fd(), size)
            .map_err(|e| CoreError::Backend(format!("resizing: {e}")))?;
        screen.screen_mut().set_size(size.rows, size.cols);
        Ok(())
    }

    async fn snapshot(&self, id: &SessionId) -> Result<Snapshot> {
        let s = self.get(id)?;
        let screen = s.screen.lock().unwrap();
        Ok(Snapshot {
            size: Session::size(&screen),
            bytes: s.repaint(&screen),
        })
    }

    async fn kill(&self, id: &SessionId) -> Result<()> {
        let s = self
            .inner
            .sessions
            .lock()
            .unwrap()
            .remove(id)
            .ok_or_else(|| CoreError::NotFound(format!("session {}", id.0)))?;
        if !s.info.lock().unwrap().alive {
            return Ok(());
        }
        // The program leads its own session and process group.
        let _ = kill_process_group(s.pid, Signal::HUP);
        std::thread::spawn(move || {
            std::thread::sleep(KILL_GRACE);
            if s.info.lock().unwrap().alive {
                let _ = kill_process_group(s.pid, Signal::KILL);
            }
        });
        Ok(())
    }

    async fn list(&self) -> Result<Vec<SessionInfo>> {
        Ok(self
            .inner
            .sessions
            .lock()
            .unwrap()
            .values()
            .map(|s| s.info.lock().unwrap().clone())
            .collect())
    }
}

#[cfg(test)]
mod tests;
