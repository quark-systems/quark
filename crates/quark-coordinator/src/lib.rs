//! The judgment-only coordinator for the native Quark engine (slice 6).
//!
//! Rust owns the lifecycle (spawn, watch, recover, gates, rebase, merge,
//! teardown); the coordinator LLM is woken only for judgment and acts only
//! through native tools. This crate holds the deterministic half:
//!
//! - [`prompt`]: the layered prompt (a short built-in prompt versioned with
//!   quarkd, the persona, the Project's instructions and memory, and an index
//!   of skills loaded on demand).
//! - [`wake`]: which events need judgment, and the wake item each becomes.
//! - [`Coordinator`]: the durable wake queue that batches those items into
//!   turns, records every turn and tool call, and recovers from a crash by
//!   replay.
//! - [`tools`] and [`mcp`]: the coordinator's tool set, served over MCP.
//! - [`efficiency`]: turns and tokens per task, and the share of turns spent
//!   acknowledging status, for native turns and for firstmate's
//!   ([`baseline`]).
//!
//! Until slice 6 switches on it runs in [`Mode::Shadow`]: it records what it
//! would wake the coordinator for and reads firstmate's coordinator
//! transcript for the baseline, and acts on nothing. See
//! `docs/engine/coordinator.md`.

pub mod baseline;
mod coordinator;
pub mod efficiency;
pub mod events;
pub mod mcp;
pub mod prompt;
pub mod tools;
pub mod wake;

pub use coordinator::{Brain, Config, Coordinator, Mode, NoBrain, TickReport, Turn};
pub use efficiency::{Efficiency, Side};
pub use events::{CoordinatorEvent, Usage};
pub use prompt::{LayeredPrompt, Sources};
pub use tools::{Hands, NoHands, ToolCall};
pub use wake::{Need, WakeItem};

use quark_core::EventId;

/// Who the coordinator records as the actor of its own changes.
pub const ENGINE: &str = "engine";

/// FNV-1a over `parts`, each followed by a zero byte.
fn fnv(seed: &str, parts: &[&str]) -> u128 {
    const BASIS: u128 = 0x6c62272e07bb014262b821756295c58d;
    const PRIME: u128 = 0x0000000001000000000000000000013b;
    let mut h = BASIS;
    for part in std::iter::once(seed).chain(parts.iter().copied()) {
        for b in part.bytes().chain(std::iter::once(0)) {
            h ^= u128::from(b);
            h = h.wrapping_mul(PRIME);
        }
    }
    h
}

/// A deterministic event id for `parts`, stable across processes and Rust
/// versions, so the same fact appended twice lands once.
pub(crate) fn stable_id(parts: &[&str]) -> EventId {
    EventId(uuid::Uuid::new_v8(
        fnv("quark-coordinator", parts).to_be_bytes(),
    ))
}

/// A short content digest (not for security): 64 bits of FNV-1a as hex.
pub(crate) fn digest(text: &str) -> String {
    format!("{:016x}", fnv("digest", &[text]) as u64)
}
