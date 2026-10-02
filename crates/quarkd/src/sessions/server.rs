//! One-shot commands against a workspace's private tmux server.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

/// Session the daemon creates windows in when it starts a server itself.
pub const SESSION_NAME: &str = "quark";
/// Size of windows no client has sized yet.
pub const DEFAULT_COLS: u16 = 120;
pub const DEFAULT_ROWS: u16 = 36;
/// `sun_path` is 104 bytes on macOS and 108 on Linux, including the NUL.
const MAX_SOCKET_PATH: usize = 103;

/// Server settings applied when the daemon starts a workspace server.
/// The global `window-size manual` setting is deliberately absent: it crashes
/// tmux 3.4. Windows switch to manual sizing one by one through
/// `resize-window` instead.
const BASE_CONF: &str = "\
set -g default-terminal tmux-256color
set -g default-size 120x36
set -g history-limit 10000
set -g escape-time 0
set-environment -g COLORTERM truecolor
";

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("tmux socket path is too long ({0} bytes): {1}")]
    SocketPath(usize, PathBuf),
    #[error("tmux {command} failed: {stderr}")]
    Failed { command: String, stderr: String },
    #[error("could not parse tmux output: {0}")]
    Parse(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// A program to start in a new window.
#[derive(Debug, Clone, Default)]
pub struct WindowSpec {
    /// Window name. Workers are found by `session:window` targets, so the
    /// name must be unique in its session.
    pub name: String,
    /// Program and arguments, run directly without a shell when there is
    /// more than one element.
    pub argv: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
}

/// One pane, as `list-panes -a` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneInfo {
    pub session_id: String,
    pub window_id: String,
    pub pane_id: String,
    pub pane_index: u32,
    pub cols: u16,
    pub rows: u16,
    /// `session_name:window_name`, the form firstmate records as a task's
    /// endpoint target.
    pub target: String,
}

const PANE_FORMAT: &str = "#{session_id}|#{window_id}|#{pane_id}|#{pane_index}|#{pane_width}|\
#{pane_height}|#{session_name}:#{window_name}";

/// A private tmux server, addressed by socket path (`tmux -S`).
#[derive(Debug, Clone)]
pub struct Server {
    tmux: PathBuf,
    socket: PathBuf,
}

impl Server {
    pub fn new(tmux: impl Into<PathBuf>, socket: impl Into<PathBuf>) -> Result<Self, ServerError> {
        let socket = socket.into();
        let len = socket.as_os_str().len();
        if len > MAX_SOCKET_PATH {
            return Err(ServerError::SocketPath(len, socket));
        }
        Ok(Self {
            tmux: tmux.into(),
            socket,
        })
    }

    pub fn tmux(&self) -> &Path {
        &self.tmux
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    fn command(&self) -> Command {
        let mut c = Command::new(&self.tmux);
        c.arg("-u").arg("-S").arg(&self.socket);
        c.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        c
    }

    async fn output(&self, mut c: Command, what: &str) -> Result<String, ServerError> {
        let out = c.output().await?;
        if !out.status.success() {
            return Err(ServerError::Failed {
                command: what.to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Whether a server is listening on the socket.
    pub async fn is_running(&self) -> bool {
        if !self.socket.exists() {
            return false;
        }
        let mut c = self.command();
        c.arg("list-sessions");
        matches!(c.output().await, Ok(o) if o.status.success())
    }

    /// Every pane on the server, or an empty list when no server runs.
    pub async fn list_panes(&self) -> Result<Vec<PaneInfo>, ServerError> {
        if !self.socket.exists() {
            return Ok(Vec::new());
        }
        let mut c = self.command();
        c.args(["list-panes", "-a", "-F", PANE_FORMAT]);
        let out = match self.output(c, "list-panes").await {
            Ok(out) => out,
            Err(ServerError::Failed { stderr, .. }) if no_server(&stderr) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        out.lines()
            .filter(|l| !l.is_empty())
            .map(parse_pane)
            .collect()
    }

    /// Starts the server with its `quark` session, holding one idle shell
    /// window, unless the session already exists.
    pub async fn ensure_session(&self) -> Result<(), ServerError> {
        if self.has_session(SESSION_NAME).await {
            return Ok(());
        }
        self.start_window(&WindowSpec {
            name: SESSION_NAME.into(),
            ..WindowSpec::default()
        })
        .await
        .map(drop)
    }

    /// Starts `spec` in a new window of the `quark` session, starting the
    /// server and the session first when needed. Returns the window's
    /// `session:window` target.
    pub async fn start_window(&self, spec: &WindowSpec) -> Result<String, ServerError> {
        if spec.name.is_empty() || spec.name.contains([':', '.']) {
            return Err(ServerError::Parse(format!(
                "invalid window name {:?}",
                spec.name
            )));
        }
        if let Some(dir) = self.socket.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut c = self.command();
        if self.has_session(SESSION_NAME).await {
            c.args(["new-window", "-d", "-t"])
                .arg(format!("{SESSION_NAME}:"));
        } else {
            let conf = self.socket.with_extension("conf");
            std::fs::write(&conf, server_conf())?;
            c.arg("-f").arg(&conf).args([
                "new-session",
                "-d",
                "-s",
                SESSION_NAME,
                "-x",
                &DEFAULT_COLS.to_string(),
                "-y",
                &DEFAULT_ROWS.to_string(),
            ]);
        }
        c.args([
            "-P",
            "-F",
            "#{session_name}:#{window_name}",
            "-n",
            &spec.name,
        ]);
        if let Some(cwd) = &spec.cwd {
            c.arg("-c").arg(cwd);
        }
        for (k, v) in &spec.env {
            c.arg("-e").arg(format!("{k}={v}"));
        }
        c.args(&spec.argv);
        Ok(self.output(c, "new-window").await?.trim().to_string())
    }

    async fn has_session(&self, name: &str) -> bool {
        if !self.socket.exists() {
            return false;
        }
        let mut c = self.command();
        c.args(["has-session", "-t"]).arg(format!("={name}"));
        matches!(c.output().await, Ok(o) if o.status.success())
    }

    /// Stops the server and every program in it.
    pub async fn kill(&self) {
        let mut c = self.command();
        c.arg("kill-server");
        let _ = c.output().await;
    }
}

fn no_server(stderr: &str) -> bool {
    stderr.contains("no server running")
        || stderr.contains("error connecting")
        || stderr.contains("No such file or directory")
}

fn parse_pane(line: &str) -> Result<PaneInfo, ServerError> {
    let bad = || ServerError::Parse(line.to_string());
    let mut f = line.splitn(7, '|');
    let mut next = || f.next().ok_or_else(bad);
    let session_id = next()?.to_string();
    let window_id = next()?.to_string();
    let pane_id = next()?.to_string();
    let pane_index = next()?.parse().map_err(|_| bad())?;
    let cols = next()?.parse().map_err(|_| bad())?;
    let rows = next()?.parse().map_err(|_| bad())?;
    let target = next()?.to_string();
    Ok(PaneInfo {
        session_id,
        window_id,
        pane_id,
        pane_index,
        cols,
        rows,
        target,
    })
}

/// [`BASE_CONF`], plus a UTF-8 character type when the daemon runs without
/// one: panes without a UTF-8 locale mangle non-ASCII input.
fn server_conf() -> String {
    let mut conf = BASE_CONF.to_string();
    let utf8 = ["LC_ALL", "LC_CTYPE", "LANG"].iter().any(|k| {
        std::env::var(k)
            .map(|v| {
                let v = v.to_ascii_lowercase();
                v.contains("utf-8") || v.contains("utf8")
            })
            .unwrap_or(false)
    });
    if !utf8 {
        conf.push_str("set-environment -g LC_CTYPE C.UTF-8\n");
    }
    conf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pane_lines() {
        let p = parse_pane("$0|@3|%7|0|120|36|quark:coord|inator").unwrap();
        assert_eq!(p.session_id, "$0");
        assert_eq!(p.window_id, "@3");
        assert_eq!(p.pane_id, "%7");
        assert_eq!((p.cols, p.rows), (120, 36));
        assert_eq!(p.target, "quark:coord|inator");
        assert!(parse_pane("garbage").is_err());
    }

    #[test]
    fn rejects_long_socket_paths() {
        let long = format!("/tmp/{}", "x".repeat(120));
        assert!(matches!(
            Server::new("tmux", long),
            Err(ServerError::SocketPath(..))
        ));
    }
}
