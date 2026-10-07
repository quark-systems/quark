//! Terminal sessions in tmux on any runtime.
//!
//! Each call is one `tmux` command on the host, so sessions belong to the
//! host's tmux server, not to the connection or daemon that started them:
//! a session on an SSH host keeps running when the connection drops and is
//! found again by [`SessionBackend::list`]. Sessions live on their own
//! server socket (`tmux -L <socket>`), away from the user's own tmux, with
//! `remain-on-exit` on so an exited session still reports its exit code.
//!
//! Session ids are the session names, which stay stable across a tmux
//! server's lifetime. There is no live output stream over a runtime yet:
//! [`SessionBackend::attach`] is unsupported and viewers poll
//! [`SessionBackend::snapshot`].
//!
//! Needs tmux 3.2 or later (`new-session -e`).

use std::sync::Arc;

use async_trait::async_trait;
use quark_core::session::{
    OutputStream, SessionBackend, SessionId, SessionInfo, SessionSpec, Snapshot, TermSize,
};
use quark_core::{CoreError, Result};

use crate::exec::{Cmd, Exec, Output};
use crate::quote;

/// The tmux socket name Quark's sessions use by default.
pub const DEFAULT_SOCKET: &str = "quark";

/// A [`SessionBackend`] driving tmux on a runtime's host.
#[derive(Clone)]
pub struct TmuxSessions {
    exec: Arc<dyn Exec>,
    socket: String,
}

impl TmuxSessions {
    pub fn new(exec: Arc<dyn Exec>) -> Self {
        Self::with_socket(exec, DEFAULT_SOCKET)
    }

    /// On tmux server socket `socket` (`tmux -L`).
    pub fn with_socket(exec: Arc<dyn Exec>, socket: impl Into<String>) -> Self {
        Self {
            exec,
            socket: socket.into(),
        }
    }

    async fn tmux(&self, args: &[&str], stdin: Option<Vec<u8>>) -> Result<Output> {
        let mut argv = vec!["tmux", "-L", &self.socket, "-f", "/dev/null"];
        argv.extend_from_slice(args);
        let mut cmd = Cmd::new(argv);
        cmd.stdin = stdin;
        self.exec.run(&cmd).await
    }

    fn target(id: &SessionId) -> String {
        format!("={}:", id.0)
    }
}

/// tmux reads `:` and `.` in names as target separators.
fn check_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(CoreError::Invalid(format!("session name {name:?}")))
    }
}

/// tmux's message for a missing server or session.
fn missing(out: &Output) -> bool {
    let e = out.stderr_str();
    e.contains("no server running")
        || e.contains("can't find session")
        || e.contains("no current target")
        || e.contains("error connecting to")
        || e.contains("No such file or directory")
}

#[async_trait]
impl SessionBackend for TmuxSessions {
    fn name(&self) -> &'static str {
        "tmux"
    }

    async fn create(&self, spec: &SessionSpec) -> Result<SessionInfo> {
        check_name(&spec.name)?;
        if spec.argv.is_empty() {
            return Err(CoreError::Invalid("empty command".into()));
        }
        let cols = spec.size.cols.to_string();
        let rows = spec.size.rows.to_string();
        let cwd = spec.cwd.to_string_lossy().into_owned();
        let env: Vec<String> = spec.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let command = format!("exec {}", quote::line(&spec.argv));
        let mut args: Vec<&str> = vec![
            "start-server",
            ";",
            "set-option",
            "-g",
            "remain-on-exit",
            "on",
            ";",
            "set-option",
            "-g",
            "exit-empty",
            "off",
            ";",
            "set-option",
            "-g",
            "default-shell",
            "/bin/sh",
            ";",
            "new-session",
            "-d",
            "-s",
            &spec.name,
            "-x",
            &cols,
            "-y",
            &rows,
            "-c",
            &cwd,
        ];
        for e in &env {
            args.extend(["-e", e]);
        }
        args.push(&command);
        self.tmux(&args, None)
            .await?
            .check(&format!("starting session {}", spec.name))?;
        Ok(SessionInfo {
            id: SessionId(spec.name.clone()),
            name: spec.name.clone(),
            task: spec.task.clone(),
            alive: true,
            exit_code: None,
        })
    }

    async fn attach(&self, _id: &SessionId) -> Result<Box<dyn OutputStream>> {
        Err(CoreError::Unsupported(
            "live output from tmux over a runtime; poll snapshot".into(),
        ))
    }

    async fn input(&self, id: &SessionId, bytes: &[u8]) -> Result<()> {
        let buffer = format!("quark-{}", uuid::Uuid::new_v4().simple());
        let target = Self::target(id);
        let out = self
            .tmux(
                &[
                    "load-buffer",
                    "-b",
                    &buffer,
                    "-",
                    ";",
                    "paste-buffer",
                    "-d",
                    "-r",
                    "-b",
                    &buffer,
                    "-t",
                    &target,
                ],
                Some(bytes.to_vec()),
            )
            .await?;
        if missing(&out) {
            return Err(CoreError::NotFound(format!("session {}", id.0)));
        }
        out.check(&format!("typing into session {}", id.0))?;
        Ok(())
    }

    async fn resize(&self, id: &SessionId, size: TermSize) -> Result<()> {
        let (x, y) = (size.cols.to_string(), size.rows.to_string());
        let target = Self::target(id);
        self.tmux(&["resize-window", "-t", &target, "-x", &x, "-y", &y], None)
            .await?
            .check(&format!("resizing session {}", id.0))?;
        Ok(())
    }

    async fn snapshot(&self, id: &SessionId) -> Result<Snapshot> {
        let target = Self::target(id);
        let out = self
            .tmux(
                &[
                    "display-message",
                    "-p",
                    "-t",
                    &target,
                    "#{pane_width} #{pane_height}",
                    ";",
                    "capture-pane",
                    "-p",
                    "-e",
                    "-J",
                    "-S",
                    "-",
                    "-t",
                    &target,
                ],
                None,
            )
            .await?;
        if missing(&out) {
            return Err(CoreError::NotFound(format!("session {}", id.0)));
        }
        let out = out.check(&format!("reading session {}", id.0))?;
        let split = out
            .stdout
            .iter()
            .position(|b| *b == b'\n')
            .unwrap_or(out.stdout.len());
        let head = String::from_utf8_lossy(&out.stdout[..split]).into_owned();
        let mut dims = head.split_whitespace().map(|n| n.parse::<u16>().ok());
        let size = match (dims.next().flatten(), dims.next().flatten()) {
            (Some(cols), Some(rows)) => TermSize { cols, rows },
            _ => TermSize::default(),
        };
        let bytes = out.stdout.get(split + 1..).unwrap_or_default().to_vec();
        Ok(Snapshot { size, bytes })
    }

    async fn kill(&self, id: &SessionId) -> Result<()> {
        let target = format!("={}", id.0);
        let out = self.tmux(&["kill-session", "-t", &target], None).await?;
        if missing(&out) {
            return Ok(());
        }
        out.check(&format!("ending session {}", id.0))?;
        Ok(())
    }

    async fn list(&self) -> Result<Vec<SessionInfo>> {
        let out = self
            .tmux(
                &[
                    "list-sessions",
                    "-F",
                    "#{session_name}|#{pane_dead}|#{pane_dead_status}",
                ],
                None,
            )
            .await?;
        if missing(&out) {
            return Ok(Vec::new());
        }
        let out = out.check("listing sessions")?;
        Ok(out.stdout_str().lines().filter_map(parse_session).collect())
    }
}

/// One `list-sessions` line. A dead pane's exit status is best effort:
/// tmux sometimes marks a pane dead without ever recording its status or
/// signal, so an exited session can report no code.
fn parse_session(line: &str) -> Option<SessionInfo> {
    let mut f = line.rsplitn(3, '|');
    let status = f.next()?;
    let dead = f.next()? == "1";
    let name = f.next()?;
    Some(SessionInfo {
        id: SessionId(name.to_string()),
        name: name.to_string(),
        task: None,
        alive: !dead,
        exit_code: if dead { status.parse().ok() } else { None },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dead_panes_are_exited_with_any_known_code() {
        let s = parse_session("sub-a|0|").unwrap();
        assert!(s.alive);
        let s = parse_session("sub-a|1|").unwrap();
        assert_eq!((s.alive, s.exit_code), (false, None));
        let s = parse_session("sub-a|1|5").unwrap();
        assert_eq!((s.alive, s.exit_code), (false, Some(5)));
        let s = parse_session("a|b|1|0").unwrap();
        assert_eq!((s.name.as_str(), s.exit_code), ("a|b", Some(0)));
    }

    #[test]
    fn names_must_be_plain() {
        assert!(check_name("sub-web_1").is_ok());
        assert!(check_name("a:b").is_err());
        assert!(check_name("a.b").is_err());
        assert!(check_name("").is_err());
    }
}
