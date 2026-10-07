//! Another machine over OpenSSH.
//!
//! Each command is one `ssh` invocation in batch mode: no prompts, no agent
//! or port forwarding, and keepalives so a vanished host fails within a
//! bounded time instead of hanging. Authentication, host keys and aliases
//! come from the user's own OpenSSH configuration.
//!
//! The remote account's login shell must be POSIX-compatible (sh, bash,
//! zsh): the server hands it the quoted command line. Non-interactive SSH
//! sessions get a minimal `PATH`, so the usual per-user tool directories
//! ([`EXTRA_PATH`]) are put in front of it.
//!
//! Exit status 255 is how `ssh` reports a failed connection, so it is an
//! `Err` (outcome unknown), never an exit code.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use quark_core::host::{Health, Runtime, RuntimeKind};
use quark_core::{CoreError, HostId, Result};
use serde::{Deserialize, Serialize};

use crate::exec::{Cmd, Exec, Options, Output};
use crate::quote;

/// Directories put in front of the remote `PATH`, where per-user and
/// package-manager tools live.
pub const EXTRA_PATH: &[&str] = &[
    "$HOME/.local/bin",
    "$HOME/.cargo/bin",
    "$HOME/.nix-profile/bin",
    "/opt/homebrew/bin",
    "/usr/local/bin",
];

/// Where an SSH host is: an alias from the user's OpenSSH configuration or
/// a host name, optionally with user, port and key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshTarget {
    pub destination: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<PathBuf>,
}

impl SshTarget {
    pub fn new(destination: impl Into<String>) -> Self {
        Self {
            destination: destination.into(),
            user: None,
            port: None,
            identity: None,
        }
    }

    /// Refuses anything `ssh` could read as an option or that could not be
    /// a host, so a recorded target never changes what `ssh` does.
    pub fn validate(&self) -> Result<()> {
        fn word(what: &str, s: &str) -> Result<()> {
            let ok = !s.is_empty()
                && !s.starts_with('-')
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-@:[]%".contains(c));
            if ok {
                Ok(())
            } else {
                Err(CoreError::Invalid(format!("ssh {what} {s:?}")))
            }
        }
        word("destination", &self.destination)?;
        if let Some(u) = &self.user {
            word("user", u)?;
            if u.contains('@') {
                return Err(CoreError::Invalid(format!("ssh user {u:?}")));
            }
        }
        if self.port == Some(0) {
            return Err(CoreError::Invalid("ssh port 0".into()));
        }
        if let Some(i) = &self.identity {
            if !i.is_absolute() {
                return Err(CoreError::Invalid(format!(
                    "ssh identity {} is not an absolute path",
                    i.display()
                )));
            }
        }
        Ok(())
    }
}

/// Runs commands on an SSH host.
#[derive(Debug, Clone)]
pub struct SshRuntime {
    host: HostId,
    target: SshTarget,
    program: PathBuf,
    connect_timeout: Duration,
}

impl SshRuntime {
    pub fn new(host: HostId, target: SshTarget, options: &Options) -> Self {
        Self {
            host,
            target,
            program: options.ssh_program.clone(),
            connect_timeout: options.connect_timeout,
        }
    }

    pub fn target(&self) -> &SshTarget {
        &self.target
    }

    /// The local `ssh` argv for `cmd` (everything after the program).
    pub fn ssh_args(&self, cmd: &Cmd) -> Vec<String> {
        let secs = self.connect_timeout.as_secs().max(1);
        let mut a: Vec<String> = [
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            &format!("ConnectTimeout={secs}"),
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
            "-o",
            "ForwardAgent=no",
            "-o",
            "ForwardX11=no",
            "-o",
            "ClearAllForwardings=yes",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        if let Some(p) = self.target.port {
            a.extend(["-p".into(), p.to_string()]);
        }
        if let Some(u) = &self.target.user {
            a.extend(["-l".into(), u.clone()]);
        }
        if let Some(i) = &self.target.identity {
            a.extend([
                "-i".into(),
                i.display().to_string(),
                "-o".into(),
                "IdentitiesOnly=yes".into(),
            ]);
        }
        a.push(self.target.destination.clone());
        a.push(remote_line(cmd));
        a
    }
}

/// The command line the remote login shell runs for `cmd`.
pub fn remote_line(cmd: &Cmd) -> String {
    let mut parts = vec![format!("PATH=\"{}:$PATH\"", EXTRA_PATH.join(":"))];
    parts.push("export PATH".into());
    if let Some(cwd) = &cmd.cwd {
        parts.push(format!("cd {}", quote::word(&cwd.to_string_lossy())));
    }
    for (k, v) in &cmd.env {
        parts.push(format!("{k}={}", quote::word(v)));
        parts.push(format!("export {k}"));
    }
    parts.push(format!("exec {}", quote::line(&cmd.argv)));
    parts.join(" && ")
}

#[async_trait]
impl Runtime for SshRuntime {
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Ssh
    }

    fn host(&self) -> &HostId {
        &self.host
    }

    async fn health(&self) -> Result<Health> {
        Ok(crate::probe::health(self).await)
    }
}

#[async_trait]
impl Exec for SshRuntime {
    async fn run(&self, cmd: &Cmd) -> Result<Output> {
        cmd.validate()?;
        self.target.validate()?;
        let program = self.program.to_string_lossy();
        let out = crate::local::process(
            &program,
            &self.ssh_args(cmd),
            &Default::default(),
            None,
            cmd.stdin.as_deref(),
            cmd.limit() + self.connect_timeout,
        )
        .await?;
        if out.code == Some(255) {
            return Err(CoreError::Backend(format!(
                "ssh to {} failed; the outcome is unknown: {}",
                self.target.destination,
                out.stderr_str()
            )));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_that_look_like_options_are_refused() {
        assert!(SshTarget::new("build-mac").validate().is_ok());
        assert!(SshTarget::new("me@10.0.0.2").validate().is_ok());
        assert!(SshTarget::new("-oProxyCommand=x").validate().is_err());
        assert!(SshTarget::new("a b").validate().is_err());
        assert!(SshTarget::new("").validate().is_err());
        let mut t = SshTarget::new("h");
        t.identity = Some("rel/key".into());
        assert!(t.validate().is_err());
    }

    #[test]
    fn argv_is_batch_mode_without_forwarding() {
        let mut t = SshTarget::new("box");
        t.port = Some(2222);
        t.user = Some("q".into());
        let rt = SshRuntime::new(HostId::from("box"), t, &Options::default());
        let a = rt.ssh_args(&Cmd::new(["echo", "a b"]).cwd("/srv/home").env("K", "v'1"));
        assert!(a.windows(2).any(|w| w == ["-o", "BatchMode=yes"]));
        assert!(a.windows(2).any(|w| w == ["-o", "ForwardAgent=no"]));
        assert!(a.windows(2).any(|w| w == ["-p", "2222"]));
        assert!(a.windows(2).any(|w| w == ["-l", "q"]));
        let n = a.len();
        assert_eq!(a[n - 2], "box");
        assert!(a[n - 1].ends_with(r"cd /srv/home && K='v'\''1' && export K && exec echo 'a b'"));
    }
}
