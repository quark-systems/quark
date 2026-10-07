//! Runtimes: where the engine runs things.
//!
//! A runtime reaches one host. [`Exec`] extends [`quark_core::Runtime`] with
//! the one primitive everything else is built on, running a command with
//! arguments, environment, working directory and stdin, so the same code
//! seeds a home, writes a file or drives a terminal session whether the host
//! is this machine ([`LocalRuntime`]) or an SSH host ([`SshRuntime`]).
//! Hosted and private-cloud runtimes plug in here with Cloud mode.
//!
//! - [`fs`]: atomic writes, offset reads and listings on any runtime.
//! - [`probe`]: the host's platform and which tools it has.
//! - [`TmuxSessions`]: a [`quark_core::SessionBackend`] on any runtime, so
//!   a session on an SSH host survives the connection that started it.
//!
//! An `Err` from [`Exec::run`] means the command did not run or its outcome
//! is unknown (the connection failed or timed out). An `Ok` with a non-zero
//! exit code means it ran and failed. Callers treat the first as "try again
//! later" and never as proof that something on the host is gone.

mod exec;
pub mod fs;
mod local;
pub mod probe;
pub mod quote;
mod ssh;
mod tmux;

pub use exec::{connect, Cmd, Exec, Options, Output, RuntimeSpec};
pub use local::LocalRuntime;
pub use probe::Probe;
pub use ssh::{SshRuntime, SshTarget};
pub use tmux::{TmuxSessions, DEFAULT_SOCKET as TMUX_SOCKET};
