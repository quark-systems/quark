//! Neutral types for the Quark `/v1` API and its typed event stream.
//!
//! These types are the contract between `quarkd` and every client (the desktop
//! app today, generated web and mobile clients later). They use neutral names
//! only: no engine vocabulary leaks through here.
//!
//! Types live in one module per domain and are re-exported here, so callers
//! keep using `quark_systems::Task`. A new domain gets its own module rather
//! than growing an existing one.

mod account;
mod automation;
mod dispatch;
mod event;
mod harness;
mod memory;
mod metrics;
mod overview;
mod persona;
mod pr;
mod project;
mod service;
mod settings;
mod task;
mod terminal;
mod transcript;

pub use account::*;
pub use automation::*;
pub use dispatch::*;
pub use event::*;
pub use harness::*;
pub use memory::*;
pub use metrics::*;
pub use overview::*;
pub use persona::*;
pub use pr::*;
pub use project::*;
pub use service::*;
pub use settings::*;
pub use task::*;
pub use terminal::*;
pub use transcript::*;

/// API version prefix every route is served under.
pub const API_VERSION: &str = "v1";
