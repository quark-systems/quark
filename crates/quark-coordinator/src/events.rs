//! The coordinator's events, under the `coordinator.` prefix.
//!
//! Everything the coordinator must remember across a crash is one of these:
//! which prompt each Project's coordinator runs on, which items woke it in
//! which turn, every tool call and its outcome, how each turn ended and what
//! it cost, and (in shadow mode) what it would have woken for and how
//! firstmate's coordinator spent its turns. [`crate::Coordinator`] folds
//! them back.

use quark_core::{EventKind, Seq};
use serde::{Deserialize, Serialize};

use crate::baseline::BaselineTurn;
use crate::prompt::PromptRecord;
use crate::tools::ToolCall;
use crate::wake::WakeItem;

/// The prefix every coordinator event kind starts with.
pub const PREFIX: &str = "coordinator";

/// Tokens one turn used, as the harness reports them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Input tokens, including cache reads and writes.
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    /// Of `input`, how many were read from the prompt cache.
    #[serde(default)]
    pub cache_read: u64,
    /// Model calls in the turn.
    #[serde(default)]
    pub calls: u32,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input + self.output
    }

    pub fn add(&mut self, other: &Usage) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.calls += other.calls;
    }
}

/// How a turn ended.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnEnd {
    /// The harness reported the turn finished.
    #[default]
    Finished,
    /// No end was reported within the turn timeout; its items wake the
    /// coordinator again.
    TimedOut,
}

/// Payload of every `coordinator.*` event; the kind is `coordinator.<type>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CoordinatorEvent {
    /// The Project's layered prompt changed (layer digests, not the text).
    Prompt { prompt: PromptRecord },
    /// A wake asked for from outside the log's own events, such as a
    /// trigger rule's `wake` action or the API.
    Requested { note: String, by: String },
    /// Native: a turn was opened for `items` and handed to the coordinator.
    /// `attempt` counts deliveries of the same turn (a restart redelivers).
    Woken {
        turn: String,
        items: Vec<WakeItem>,
        #[serde(default = "one")]
        attempt: u32,
    },
    /// Shadow: native would have woken the coordinator for `items` in one
    /// turn.
    WouldWake { items: Vec<WakeItem> },
    /// The coordinator called a tool. Recorded before the tool runs.
    Tool {
        id: String,
        turn: Option<String>,
        call: ToolCall,
    },
    /// What the call `id` did.
    ToolDone {
        id: String,
        ok: bool,
        detail: String,
    },
    /// A turn ended. `turn` is `None` for a turn the coordinator took
    /// without a wake, such as answering the user directly.
    TurnEnded {
        turn: Option<String>,
        #[serde(default)]
        end: TurnEnd,
        #[serde(default)]
        usage: Usage,
    },
    /// Shadow: one turn firstmate's coordinator took, read from its
    /// transcript.
    Baseline { turn: BaselineTurn },
    /// Firstmate's coordinator transcript at `path` has been read up to
    /// `offset`.
    TranscriptRead { path: String, offset: u64 },
    /// Events up to `through` have been evaluated.
    Cursor { through: Seq },
}

fn one() -> u32 {
    1
}

impl CoordinatorEvent {
    pub fn kind(&self) -> EventKind {
        let name = match self {
            CoordinatorEvent::Prompt { .. } => "prompt",
            CoordinatorEvent::Requested { .. } => "requested",
            CoordinatorEvent::Woken { .. } => "woken",
            CoordinatorEvent::WouldWake { .. } => "would_wake",
            CoordinatorEvent::Tool { .. } => "tool",
            CoordinatorEvent::ToolDone { .. } => "tool_done",
            CoordinatorEvent::TurnEnded { .. } => "turn_ended",
            CoordinatorEvent::Baseline { .. } => "baseline",
            CoordinatorEvent::TranscriptRead { .. } => "transcript_read",
            CoordinatorEvent::Cursor { .. } => "cursor",
        };
        EventKind::new(format!("{PREFIX}.{name}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_matches_tag() {
        let e = CoordinatorEvent::WouldWake { items: vec![] };
        assert_eq!(e.kind().as_str(), "coordinator.would_wake");
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["type"], "would_wake");
        assert_eq!(serde_json::from_value::<CoordinatorEvent>(v).unwrap(), e);
    }
}
