//! The native engine's event log (slice 1).
//!
//! - [`SqliteEventLog`]: the crash-safe [`quark_core::EventLog`] in its own
//!   SQLite file, `events.db`, separate from quarkd's projection store.
//! - [`tasks`]: task state as a replay-safe read model, and the
//!   [`TaskLedger`] that makes every transition an event.
//!
//! The `eventlog-crash` binary is the crash-test harness's child process:
//! `tests/crash.rs` kills it at random points and checks what survives.
//! `docs/engine/event-log.md` describes the file and the guarantees.

mod sqlite;
pub mod tasks;

pub use sqlite::SqliteEventLog;
pub use tasks::{TaskLedger, TaskRecord, TaskStates};

/// The log's file name inside quarkd's home.
pub const FILE_NAME: &str = "events.db";
