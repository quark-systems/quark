//! Spawn, steer and supervise workers: slice 4 of the native engine.
//!
//! [`Supervisor`] composes the subsystems behind their quark-core
//! contracts:
//!
//! | Step | Contract | Implemented by |
//! |---|---|---|
//! | isolated worktree per task | [`quark_core::WorktreeProvider`] | `quark-worktree` |
//! | launch command, keys, hooks | [`quark_core::HarnessManifest`] | `quark-harness` |
//! | native or sandboxed process | [`quark_core::Isolation`] | `quark-isolation` |
//! | terminal session | [`quark_core::SessionBackend`] | `quark-sessions` (PTY supervisor, tmux) |
//! | worker messages | [`quark_core::WorkerProtocol`] | `quark-worker` |
//! | task state | [`quark_eventlog::TaskLedger`] | `quark-eventlog` |
//!
//! Every change is an event before it is anything else: task transitions
//! go through the ledger, and what the supervisor must remember (the
//! assignment, worktree, current generation and session, steering messages
//! and their delivery) is a `supervisor.*` event ([`events`]) folded into
//! the [`Fleet`]. Recovery is replay: a new supervisor on the same log
//! adopts sessions that survived ([`Supervisor::recover`]) and relaunches
//! workers whose sessions are gone in the same worktree
//! ([`Supervisor::tick`]).
//!
//! `docs/engine/supervision.md` describes the lifecycle and guarantees.

pub mod events;
mod fleet;
pub mod launch;
mod supervisor;

pub use events::{Assignment, Cause, SupervisorEvent};
pub use fleet::{Fleet, Generation, Steer, Worker};
pub use launch::WorkerUrls;
pub use supervisor::{assert_isolated, Config, Relaunch, SpawnRequest, Supervisor, Tick};
