//! Running worker processes natively or in a sandbox: the
//! [`quark_core::Isolation`] contract.
//!
//! [`HostIsolation`] turns a [`ProcessSpec`] into the argv and env a session
//! backend runs, so isolation composes with tmux and the PTY supervisor
//! instead of owning the process.
//!
//! - [`IsolationMode::Native`]: the command as given.
//! - [`IsolationMode::Sandbox`]: Seatbelt (`sandbox-exec`) on macOS,
//!   bubblewrap (`bwrap`) on Linux.
//! - [`IsolationMode::Container`]: not here; it arrives with Cloud mode.
//!
//! Both sandboxes enforce the same [`Policy`] ([`policy`] has the rules):
//! the whole filesystem stays readable, writes are limited to the policy's
//! writable paths plus temp directories and `/dev`, the policy's readable
//! paths are read-only even inside a writable one, and with `network` off
//! no IP traffic leaves the process (Unix sockets still work).
//! [`git_worktree_policy`] builds a policy that lets a worker commit in a
//! linked worktree without being able to plant config that would run code
//! the next time Quark runs git there outside the sandbox.

mod bwrap;
pub mod policy;
mod seatbelt;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use quark_core::isolation::ProcessSpec;
use quark_core::{CoreError, Isolation, IsolationMode, Result};
use tokio::process::Command;

pub use policy::{git_worktree_policy, Resolved};

/// Env var set on every sandboxed process, so a worker can tell it is
/// sandboxed and which backend holds it (`seatbelt` or `bubblewrap`).
pub const ISOLATION_ENV: &str = "QUARK_ISOLATION";

/// The sandbox a host offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sandbox {
    /// No sandbox: only [`IsolationMode::Native`].
    None,
    /// macOS Seatbelt through `sandbox-exec` at this path.
    Seatbelt(PathBuf),
    /// Linux bubblewrap through `bwrap` at this path.
    Bubblewrap(PathBuf),
}

impl Sandbox {
    fn name(&self) -> &'static str {
        match self {
            Sandbox::None => "none",
            Sandbox::Seatbelt(_) => "seatbelt",
            Sandbox::Bubblewrap(_) => "bubblewrap",
        }
    }
}

/// [`Isolation`] for the local host.
#[derive(Debug, Clone)]
pub struct HostIsolation {
    sandbox: Sandbox,
}

impl HostIsolation {
    /// Use this sandbox without probing it.
    pub fn new(sandbox: Sandbox) -> Self {
        Self { sandbox }
    }

    /// Find the host's sandbox and check that it can actually start a
    /// process (bubblewrap needs user namespaces, which some kernels and
    /// AppArmor policies forbid). Falls back to [`Sandbox::None`].
    pub async fn detect() -> Self {
        let sandbox = if cfg!(target_os = "macos") {
            let exe = PathBuf::from("/usr/bin/sandbox-exec");
            let probe = [
                "-p".to_string(),
                "(version 1)(allow default)".to_string(),
                "/usr/bin/true".to_string(),
            ];
            if probe_ok(&exe, &probe).await {
                Sandbox::Seatbelt(exe)
            } else {
                Sandbox::None
            }
        } else if cfg!(target_os = "linux") {
            match find_on_path("bwrap") {
                Some(exe) if probe_ok(&exe, &bwrap::probe_args()).await => Sandbox::Bubblewrap(exe),
                _ => Sandbox::None,
            }
        } else {
            Sandbox::None
        };
        Self { sandbox }
    }

    pub fn sandbox(&self) -> &Sandbox {
        &self.sandbox
    }
}

#[async_trait]
impl Isolation for HostIsolation {
    fn modes(&self) -> Vec<IsolationMode> {
        match self.sandbox {
            Sandbox::None => vec![IsolationMode::Native],
            _ => vec![IsolationMode::Native, IsolationMode::Sandbox],
        }
    }

    async fn wrap(&self, spec: &ProcessSpec) -> Result<(Vec<String>, BTreeMap<String, String>)> {
        if spec.argv.is_empty() {
            return Err(CoreError::Invalid("empty argv".into()));
        }
        match spec.mode {
            IsolationMode::Native => Ok((spec.argv.clone(), spec.env.clone())),
            IsolationMode::Container => Err(CoreError::Unsupported(
                "container isolation arrives with Cloud mode".into(),
            )),
            IsolationMode::Sandbox => {
                let resolved = Resolved::from_spec(spec)?;
                let mut argv = match &self.sandbox {
                    Sandbox::None => {
                        return Err(CoreError::Unsupported(
                            "this host has no usable sandbox (Seatbelt or bubblewrap)".into(),
                        ))
                    }
                    Sandbox::Seatbelt(exe) => seatbelt::argv(exe, &resolved),
                    Sandbox::Bubblewrap(exe) => bwrap::argv(exe, &resolved),
                };
                argv.extend(spec.argv.iter().cloned());
                let mut env = spec.env.clone();
                env.insert(ISOLATION_ENV.into(), self.sandbox.name().into());
                Ok((argv, env))
            }
        }
    }
}

async fn probe_ok(exe: &Path, args: &[String]) -> bool {
    Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}
