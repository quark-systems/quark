//! Engine terminals: listing, input, resize and output chunks.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Who a terminal belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TerminalRole {
    /// A Project's coordinator session. Its terminal id is the Project id.
    Coordinator,
    /// A task's worker session. Its terminal id is the task id.
    Worker,
}

/// A live terminal: one session pane the daemon streams as `worker.output`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Terminal {
    /// The task id for a worker, the Project id for a coordinator.
    pub id: String,
    pub project_id: String,
    pub role: TerminalRole,
    /// Set for worker terminals.
    pub task_id: Option<String>,
    pub title: String,
    pub cols: u16,
    pub rows: u16,
}

/// Raw bytes to type into a terminal.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TerminalInput {
    /// Base64 of the bytes, exactly as a terminal would send them
    /// (`\r` for Enter, escape sequences for keys).
    pub data_b64: String,
    /// Optional per-terminal sequence number. When given, it must be greater
    /// than the last one applied, so a retried request is not typed twice.
    pub seq: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
pub struct TerminalResize {
    pub cols: u16,
    pub rows: u16,
}

/// What a `worker.output` event carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TerminalChunkKind {
    /// Bytes the program wrote, in order. Not aligned to lines or UTF-8.
    Output,
    /// A full repaint: reset the emulator to `cols` x `rows`, then feed
    /// `data_b64`. Sent when the daemon (re)attaches to a pane or had to drop
    /// output, so a client never needs history from before it.
    Snapshot,
}

/// Payload of a `worker.output` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TerminalOutput {
    pub terminal_id: String,
    pub role: TerminalRole,
    pub task_id: Option<String>,
    pub kind: TerminalChunkKind,
    pub data_b64: String,
    /// Terminal size; set on snapshots.
    pub cols: Option<u16>,
    pub rows: Option<u16>,
}
