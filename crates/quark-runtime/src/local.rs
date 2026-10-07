//! This machine.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use quark_core::host::{Health, Runtime, RuntimeKind};
use quark_core::{CoreError, HostId, Result};
use tokio::io::AsyncWriteExt;

use crate::exec::{Cmd, Exec, Output};

/// Runs commands on this machine.
#[derive(Debug, Clone)]
pub struct LocalRuntime {
    host: HostId,
}

impl LocalRuntime {
    pub fn new(host: HostId) -> Self {
        Self { host }
    }
}

#[async_trait]
impl Runtime for LocalRuntime {
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Local
    }

    fn host(&self) -> &HostId {
        &self.host
    }

    async fn health(&self) -> Result<Health> {
        Ok(crate::probe::health(self).await)
    }
}

#[async_trait]
impl Exec for LocalRuntime {
    async fn run(&self, cmd: &Cmd) -> Result<Output> {
        cmd.validate()?;
        process(
            &cmd.argv[0],
            &cmd.argv[1..],
            &cmd.env,
            cmd.cwd.as_deref(),
            cmd.stdin.as_deref(),
            cmd.limit(),
        )
        .await
    }
}

/// `ETXTBSY` on Linux and macOS.
const TEXT_FILE_BUSY: i32 = 26;

/// Run a local process to completion, killing it at `timeout`.
pub(crate) async fn process(
    program: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    cwd: Option<&Path>,
    stdin: Option<&[u8]>,
    timeout: Duration,
) -> Result<Output> {
    let mut c = tokio::process::Command::new(program);
    c.args(args)
        .envs(env)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = cwd {
        c.current_dir(cwd);
    }
    // A program written just before it runs can fail with ETXTBSY while
    // another thread's freshly forked child still holds the write handle;
    // that clears within moments, so try again briefly.
    let mut tries = 0;
    let mut child = loop {
        match c.spawn() {
            Ok(child) => break child,
            Err(e) if e.raw_os_error() == Some(TEXT_FILE_BUSY) && tries < 20 => {
                tries += 1;
                tokio::time::sleep(Duration::from_millis(10 * tries)).await;
            }
            Err(e) => {
                return Err(CoreError::Backend(format!(
                    "could not start {program}: {e}"
                )))
            }
        }
    };
    let writer = match (stdin, child.stdin.take()) {
        (Some(bytes), Some(mut pipe)) => {
            let bytes = bytes.to_vec();
            Some(tokio::spawn(async move {
                // A command that exits without reading its input is not an
                // error of ours; its exit code says what happened.
                let _ = pipe.write_all(&bytes).await;
                let _ = pipe.shutdown().await;
            }))
        }
        _ => None,
    };
    let out = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| {
            CoreError::Backend(format!(
                "{program} did not finish within {}s; its outcome is unknown",
                timeout.as_secs()
            ))
        })?
        .map_err(|e| CoreError::Backend(format!("{program}: {e}")))?;
    if let Some(w) = writer {
        let _ = w.await;
    }
    Ok(Output {
        code: out.status.code(),
        stdout: out.stdout,
        stderr: out.stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt() -> LocalRuntime {
        LocalRuntime::new(HostId::from("here"))
    }

    #[tokio::test]
    async fn runs_with_env_cwd_and_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let out = rt()
            .run(
                &Cmd::sh(
                    "printf '%s|%s|' \"$X\" \"$(pwd)\"; cat",
                    Vec::<String>::new(),
                )
                .env("X", "x y")
                .cwd(dir.path())
                .stdin(b"in".to_vec()),
            )
            .await
            .unwrap();
        let want = format!("x y|{}|in", dir.path().canonicalize().unwrap().display());
        assert_eq!(out.stdout_str(), want);
        assert!(out.success());
    }

    #[tokio::test]
    async fn failures_are_ok_with_their_code() {
        let out = rt().run(&Cmd::sh("exit 4", [""; 0])).await.unwrap();
        assert_eq!(out.code, Some(4));
    }

    #[tokio::test]
    async fn timeouts_and_missing_programs_are_errors() {
        let slow = Cmd::new(["sleep", "5"]).timeout(Duration::from_millis(100));
        assert!(rt().run(&slow).await.is_err());
        assert!(rt()
            .run(&Cmd::new(["/nonexistent/quark-runtime-test"]))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn healthy_here() {
        assert_eq!(rt().health().await.unwrap(), Health::Healthy);
    }
}
