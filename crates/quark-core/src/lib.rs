//! Contracts for the native Quark engine.
//!
//! Every native subsystem crate codes against the traits and types here and
//! never against another subsystem's internals. This crate has types and
//! traits only: no files, sockets, processes or databases. The [`fake`]
//! module holds in-memory implementations for tests and for subsystems whose
//! real dependency has not landed yet.
//!
//! | Contract | Module | Implemented by |
//! |---|---|---|
//! | [`EventLog`] | [`event`] | `quark-eventlog` |
//! | [`TaskMachine`] | [`task`] | `quark-eventlog` |
//! | [`ReadModel`] | [`read_model`] | quarkd's projector |
//! | [`WorktreeProvider`] | [`worktree`] | `quark-worktree` |
//! | [`SessionBackend`] | [`session`] | `quark-sessions` |
//! | [`HarnessManifest`] | [`harness`] | `quark-harness` |
//! | [`WorkerProtocol`] | [`worker`] | `quark-worker` |
//! | [`VerifyPipeline`], [`MergeGuard`] | [`verify`] | `quark-verify` |
//! | [`Isolation`] | [`isolation`] | `quark-isolation` |
//! | [`Runtime`], [`HostRegistry`] | [`host`] | `quark-hosts` |
//! | [`Telemetry`] | [`telemetry`] | `quark-hosts` |
//! | [`PersonaPack`] | [`persona`] | `quark-persona` |
//! | [`SliceSwitch`] | [`slice`] | quarkd `engine/shadow.rs` |
//!
//! Names are neutral throughout (user, coordinator, worker, sub-coordinator,
//! investigation, decision); persona packs supply the flavor. Changing a
//! contract is a small PR to this crate on its own, and any new trait method
//! comes with a default so existing implementations keep compiling.
//!
//! `docs/engine/contracts.md` explains how the pieces fit.

pub mod error;
pub mod event;
pub mod fake;
pub mod harness;
pub mod host;
pub mod ids;
pub mod isolation;
pub mod persona;
pub mod read_model;
pub mod session;
pub mod slice;
pub mod task;
pub mod telemetry;
pub mod verify;
pub mod worker;
pub mod worktree;

pub use error::{CoreError, Result};
pub use event::{Event, EventKind, EventLog, NewEvent, Seq, Subscription};
pub use harness::{HarnessManifest, HarnessRegistry};
pub use host::{Host, HostRegistry, Runtime, RuntimeKind};
pub use ids::{EventId, HostId, ProjectId, TaskId};
pub use isolation::{Isolation, IsolationMode};
pub use persona::{PersonaPack, PersonaSource, Role};
pub use read_model::{replay, ReadModel};
pub use session::SessionBackend;
pub use slice::{Slice, SliceMode, SliceSwitch};
pub use task::{TaskEvent, TaskMachine};
pub use telemetry::Telemetry;
pub use verify::{MainHealth, MergeGuard, Permit, Verdict, VerifyPipeline};
pub use worker::{WorkerMessage, WorkerProtocol};
pub use worktree::WorktreeProvider;
