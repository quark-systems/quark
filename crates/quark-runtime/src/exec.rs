//! Commands, their output, and the [`Exec`] trait every runtime implements.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use quark_core::host::{Runtime, RuntimeKind};
use quark_core::{CoreError, HostId, Result};
use serde::{Deserialize, Serialize};

use crate::local::LocalRuntime;
use crate::ssh::{SshRuntime, SshTarget};

/// How long a command may run when it does not say.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// A command to run on a host: an argv, never a shell string, so nothing
/// the caller passes is reinterpreted on the way.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cmd {
    pub argv: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub stdin: Option<Vec<u8>>,
    /// Longest the command may run; [`DEFAULT_TIMEOUT`] when `None`.
    pub timeout: Option<Duration>,
}

impl Cmd {
    pub fn new<S: Into<String>>(argv: impl IntoIterator<Item = S>) -> Self {
        Self {
            argv: argv.into_iter().map(Into::into).collect(),
            ..Self::default()
        }
    }

    /// `sh -c <script> sh <args...>`: a fixed script with its inputs as
    /// positional parameters (`$1`, `$2`, ...), never spliced into the
    /// script text.
    pub fn sh<S: Into<String>>(script: &str, args: impl IntoIterator<Item = S>) -> Self {
        let mut argv = vec!["sh".to_string(), "-c".into(), script.into(), "sh".into()];
        argv.extend(args.into_iter().map(Into::into));
        Self {
            argv,
            ..Self::default()
        }
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn stdin(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(bytes.into());
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub(crate) fn limit(&self) -> Duration {
        self.timeout.unwrap_or(DEFAULT_TIMEOUT)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.argv.is_empty() || self.argv[0].is_empty() {
            return Err(CoreError::Invalid("empty command".into()));
        }
        for k in self.env.keys() {
            let ok = !k.is_empty()
                && !k.starts_with(|c: char| c.is_ascii_digit())
                && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !ok {
                return Err(CoreError::Invalid(format!("environment name {k:?}")));
            }
        }
        Ok(())
    }
}

/// What a command that ran printed and how it exited.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Output {
    /// `None` when it was ended by a signal.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Output {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    pub fn stdout_str(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_str(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_string()
    }

    /// `self` if it exited 0, else a [`CoreError::Backend`] naming `what`
    /// and what it printed on stderr.
    pub fn check(self, what: &str) -> Result<Output> {
        if self.success() {
            return Ok(self);
        }
        let code = self
            .code
            .map_or_else(|| "a signal".to_string(), |c| format!("exit {c}"));
        let err = self.stderr_str();
        Err(CoreError::Backend(if err.is_empty() {
            format!("{what} failed ({code})")
        } else {
            format!("{what} failed ({code}): {err}")
        }))
    }
}

/// A runtime that can run commands on its host.
#[async_trait]
pub trait Exec: Runtime {
    /// Run `cmd` to completion. `Err` means it did not run or its outcome is
    /// unknown; a command that ran and failed is `Ok` with its exit code.
    async fn run(&self, cmd: &Cmd) -> Result<Output>;
}

/// Where a host is reached, as recorded in the event log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuntimeSpec {
    /// This machine.
    Local,
    /// Another machine over OpenSSH.
    Ssh { target: SshTarget },
}

impl RuntimeSpec {
    pub fn kind(&self) -> RuntimeKind {
        match self {
            RuntimeSpec::Local => RuntimeKind::Local,
            RuntimeSpec::Ssh { .. } => RuntimeKind::Ssh,
        }
    }

    pub fn validate(&self) -> Result<()> {
        match self {
            RuntimeSpec::Local => Ok(()),
            RuntimeSpec::Ssh { target } => target.validate(),
        }
    }
}

/// How runtimes are built.
#[derive(Debug, Clone)]
pub struct Options {
    /// The OpenSSH client; `QUARK_SSH` or `ssh` on `PATH` by default.
    pub ssh_program: PathBuf,
    /// How long an SSH connection may take to open.
    pub connect_timeout: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            ssh_program: std::env::var_os("QUARK_SSH")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("ssh")),
            connect_timeout: Duration::from_secs(10),
        }
    }
}

/// The runtime `spec` describes, for `host`.
pub fn connect(spec: &RuntimeSpec, host: HostId, options: &Options) -> Result<Arc<dyn Exec>> {
    spec.validate()?;
    Ok(match spec {
        RuntimeSpec::Local => Arc::new(LocalRuntime::new(host)),
        RuntimeSpec::Ssh { target } => Arc::new(SshRuntime::new(host, target.clone(), options)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sh_keeps_inputs_out_of_the_script() {
        let c = Cmd::sh("echo \"$1\"", ["; rm -rf /"]);
        assert_eq!(c.argv, ["sh", "-c", "echo \"$1\"", "sh", "; rm -rf /"]);
    }

    #[test]
    fn env_names_are_checked() {
        assert!(Cmd::new(["true"]).env("OK_1", "x").validate().is_ok());
        assert!(Cmd::new(["true"]).env("BAD NAME", "x").validate().is_err());
        assert!(Cmd::new(["true"]).env("1X", "x").validate().is_err());
        assert!(Cmd::new(Vec::<String>::new()).validate().is_err());
    }

    #[test]
    fn spec_round_trips() {
        let spec = RuntimeSpec::Ssh {
            target: SshTarget::new("build-mac"),
        };
        let v = serde_json::to_value(&spec).unwrap();
        assert_eq!(v["kind"], "ssh");
        assert_eq!(v["target"]["destination"], "build-mac");
        assert_eq!(serde_json::from_value::<RuntimeSpec>(v).unwrap(), spec);
        assert_eq!(
            serde_json::to_value(RuntimeSpec::Local).unwrap()["kind"],
            "local"
        );
    }

    #[test]
    fn check_names_the_failure() {
        let out = Output {
            code: Some(2),
            stdout: vec![],
            stderr: b"nope\n".to_vec(),
        };
        assert_eq!(
            out.check("mkdir").unwrap_err(),
            CoreError::Backend("mkdir failed (exit 2): nope".into())
        );
    }
}
