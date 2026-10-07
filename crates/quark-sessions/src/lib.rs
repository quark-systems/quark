//! Terminal sessions for the native Quark engine, behind
//! [`quark_core::SessionBackend`].
//!
//! Two backends:
//!
//! - [`tmux`]: a private tmux server driven through control mode. This is
//!   the code quarkd's terminal view has used since Phase 1, moved here so
//!   the native engine and quarkd share one implementation; quarkd builds
//!   its Project terminal map on [`tmux::control`] and [`tmux::server`], and
//!   [`tmux::TmuxBackend`] is the same machinery as a `SessionBackend`.
//! - [`pty`]: the PTY supervisor. [`pty::PtySupervisor`] owns pseudo-
//!   terminals and the programs in them, keeping a screen model of each for
//!   snapshots. The `quark-ptyd` binary runs one on a Unix socket so its
//!   sessions outlive a daemon crash, and [`pty::PtyClient`] is the daemon's
//!   `SessionBackend` for it.
//!
//! Neither depends on quarkd; quarkd depends on this crate.

pub mod pty;
mod stream;
pub mod tmux;
