//! Triggers, channels and the away policy: slice 7 of the native engine,
//! beside sub-coordinators.
//!
//! | Piece | Module | Replaces in firstmate |
//! |---|---|---|
//! | one inbound API; the user's inbox first, plug-in [`Channel`]s later | [`channel`] | `fm-inbox.sh` (mail, voice and Relay become channels) |
//! | typed event sources and condition-to-action [`Rule`]s | [`trigger`] | `fm-procevent*`, `fm-procevent-when.sh`, `fm-contributions.sh` |
//! | per-Project or global [`AwayPolicy`] and [`Posture`] | [`away`] | `/afk`, `/quiet`, `fm-supervise-daemon.sh`, `fm-afk-*` |
//!
//! Everything is an event first (`channel.*`, `trigger.*`, `away.*`) and
//! the [`Engine`] folds its read models back from the log, so recovery is
//! replay. Until slice 7 switches on, the engine runs in
//! [`Mode::Shadow`]: it mirrors firstmate's inbox and posture, compares its
//! routing with firstmate's away classifier, records the rules that would
//! have fired, and acts on nothing.
//!
//! `docs/engine/triggers.md` describes the events and guarantees.

pub mod away;
pub mod channel;
pub mod commands;
mod engine;
pub mod firstmate;
pub mod trigger;

pub use away::{AwayPolicy, Item, Occasion, Posture, Reach, Route};
pub use channel::{Channel, Inbound, INBOX};
pub use commands::{Commands, Processes};
pub use engine::{occasion, Effects, Engine, MirrorReport, Mode, NoEffects, TickReport, ENGINE};
pub use trigger::{Action, Condition, Expect, Rule, RuleId};

use quark_core::EventId;

/// A deterministic event id for `parts`: FNV-1a as a UUIDv8, stable across
/// processes and Rust versions, so the same fact appended twice lands once.
pub(crate) fn stable_id(parts: &[&str]) -> EventId {
    const BASIS: u128 = 0x6c62272e07bb014262b821756295c58d;
    const PRIME: u128 = 0x0000000001000000000000000000013b;
    let mut h = BASIS;
    for part in std::iter::once("quark-triggers").chain(parts.iter().copied()) {
        for b in part.bytes().chain(std::iter::once(0)) {
            h ^= u128::from(b);
            h = h.wrapping_mul(PRIME);
        }
    }
    EventId(uuid::Uuid::new_v8(h.to_be_bytes()))
}
