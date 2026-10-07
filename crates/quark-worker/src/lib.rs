//! The worker protocol: how a worker talks to the engine.
//!
//! A worker says one of four things (`report`, `ask`, `learned`, `done`),
//! plus harness signals (`busy`, `turn_end`). Three transports carry them,
//! and every one ends in [`Recorder::receive`], which appends a
//! `worker.message` event to the [`quark_core::EventLog`] before answering:
//!
//! | Transport | Module | For |
//! |---|---|---|
//! | MCP tools over streamable HTTP | [`mcp`], [`http`] | harnesses that take an MCP server |
//! | Hook POSTs | [`hook`], [`http`] | harness hooks mapped by the manifest's `hooks.events` |
//! | Status file | [`file`] | harnesses with neither; the worker appends lines |
//!
//! A worker is addressed by its task and generation, both in the URL or the
//! file path quarkd hands it. quarkd mints a fresh, unguessable generation
//! per launch, so the pair also identifies the caller: a message naming an
//! old generation is still recorded, then refused so the old worker stops.
//!
//! The crate records messages only. Turning them into task transitions,
//! decisions and wakes belongs to the supervisor and coordinator slices.

pub mod file;
pub mod hook;
pub mod http;
pub mod mcp;
pub mod recorder;
pub mod shadow;

pub use file::{parse_status_line, StatusFile};
pub use hook::hook_message;
pub use http::router;
pub use mcp::McpServer;
pub use recorder::{Binding, MemoryTasks, Recorder, TaskDirectory, WorkerIdentity};
