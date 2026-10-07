//! The daemon's side of `quark-ptyd`.

use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use quark_core::session::{
    Output, OutputStream, SessionBackend, SessionId, SessionInfo, SessionSpec, Snapshot, TermSize,
};
use quark_core::{CoreError, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::unix::OwnedReadHalf;
use tokio::net::UnixStream;

use super::proto::{decode, encode, error, Frame, Request, Response};

/// How long [`PtyClient::start`] waits for a fresh `quark-ptyd` to listen.
const START_TIMEOUT: Duration = Duration::from_secs(5);

/// A [`SessionBackend`] for the `quark-ptyd` listening on one socket. Every
/// call opens its own connection, so the client recovers by itself when
/// `quark-ptyd` restarts. Cheap to clone.
#[derive(Debug, Clone)]
pub struct PtyClient {
    socket: PathBuf,
}

impl PtyClient {
    /// A client for `socket`, without checking that anything listens.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    /// A client for `socket`, starting `ptyd` (the `quark-ptyd` binary) on
    /// it first when nothing answers. `quark-ptyd` runs in its own session,
    /// so it and its programs outlive the caller.
    pub async fn start(socket: impl Into<PathBuf>, ptyd: &Path) -> Result<Self> {
        let client = Self::new(socket);
        if client.connect().await.is_ok() {
            return Ok(client);
        }
        let mut cmd = std::process::Command::new(ptyd);
        cmd.arg("--socket")
            .arg(&client.socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: setsid is a single system call, safe between fork and exec.
        unsafe {
            cmd.pre_exec(|| rustix::process::setsid().map(drop).map_err(Into::into));
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| CoreError::Backend(format!("starting {}: {e}", ptyd.display())))?;
        // Reap it if it exits early; otherwise it outlives us by design.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        let deadline = tokio::time::Instant::now() + START_TIMEOUT;
        loop {
            match client.connect().await {
                Ok(_) => return Ok(client),
                Err(e) if tokio::time::Instant::now() >= deadline => return Err(e),
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    async fn connect(&self) -> Result<UnixStream> {
        UnixStream::connect(&self.socket).await.map_err(|e| {
            CoreError::Backend(format!(
                "the PTY supervisor at {} does not answer: {e}",
                self.socket.display()
            ))
        })
    }

    /// Sends one request on a fresh connection; returns its response and
    /// the rest of the connection.
    async fn call(&self, request: &Request) -> Result<(Response, Lines<BufReader<OwnedReadHalf>>)> {
        let stream = self.connect().await?;
        let (read, mut write) = stream.into_split();
        let mut line =
            serde_json::to_vec(request).map_err(|e| CoreError::Invalid(e.to_string()))?;
        line.push(b'\n');
        write.write_all(&line).await.map_err(io)?;
        // One request per connection: shut down our side. The supervisor
        // still answers, and an attach keeps streaming until it fails to
        // write.
        drop(write);
        let mut lines = BufReader::new(read).lines();
        let reply =
            lines.next_line().await.map_err(io)?.ok_or_else(|| {
                CoreError::Backend("the PTY supervisor closed the connection".into())
            })?;
        let response: Response = serde_json::from_str(&reply)
            .map_err(|e| CoreError::Backend(format!("bad reply from the PTY supervisor: {e}")))?;
        if let Response::Error { kind, message } = response {
            return Err(error(kind, message));
        }
        Ok((response, lines))
    }

    async fn request(&self, request: &Request) -> Result<Response> {
        Ok(self.call(request).await?.0)
    }
}

fn io(e: std::io::Error) -> CoreError {
    CoreError::Backend(format!("talking to the PTY supervisor: {e}"))
}

fn unexpected(r: Response) -> CoreError {
    CoreError::Backend(format!("unexpected reply from the PTY supervisor: {r:?}"))
}

struct Remote {
    lines: Lines<BufReader<OwnedReadHalf>>,
}

#[async_trait]
impl OutputStream for Remote {
    async fn next(&mut self) -> Option<Output> {
        let line = self.lines.next_line().await.ok()??;
        serde_json::from_str::<Frame>(&line)
            .ok()?
            .into_output()
            .ok()
    }
}

#[async_trait]
impl SessionBackend for PtyClient {
    fn name(&self) -> &'static str {
        "pty"
    }

    async fn create(&self, spec: &SessionSpec) -> Result<SessionInfo> {
        match self
            .request(&Request::Create { spec: spec.clone() })
            .await?
        {
            Response::Session(info) => Ok(info),
            r => Err(unexpected(r)),
        }
    }

    async fn attach(&self, id: &SessionId) -> Result<Box<dyn OutputStream>> {
        let (_, lines) = self.call(&Request::Attach { id: id.clone() }).await?;
        Ok(Box::new(Remote { lines }))
    }

    async fn input(&self, id: &SessionId, bytes: &[u8]) -> Result<()> {
        self.request(&Request::Input {
            id: id.clone(),
            data: encode(bytes),
        })
        .await
        .map(drop)
    }

    async fn resize(&self, id: &SessionId, size: TermSize) -> Result<()> {
        self.request(&Request::Resize {
            id: id.clone(),
            size,
        })
        .await
        .map(drop)
    }

    async fn snapshot(&self, id: &SessionId) -> Result<Snapshot> {
        match self.request(&Request::Snapshot { id: id.clone() }).await? {
            Response::Snapshot { size, data } => Ok(Snapshot {
                size,
                bytes: decode(&data)?,
            }),
            r => Err(unexpected(r)),
        }
    }

    async fn kill(&self, id: &SessionId) -> Result<()> {
        self.request(&Request::Kill { id: id.clone() })
            .await
            .map(drop)
    }

    async fn list(&self) -> Result<Vec<SessionInfo>> {
        match self.request(&Request::List).await? {
            Response::Sessions(s) => Ok(s),
            r => Err(unexpected(r)),
        }
    }
}
