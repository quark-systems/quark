//! Sub-coordinators: slice 7 of the native engine.
//!
//! A sub-coordinator is a persistent coordinator with its own home: every
//! Project is one. It lives on a host reached through a runtime from
//! `quark-runtime` (this machine, or an SSH host on a private network) and
//! takes only the work its parent routes to it. Firstmate calls these
//! second mates.
//!
//! [`SubCoordinators`] does for them what firstmate's `fm-home-seed`,
//! `fm-remote-*`, `fm-spawn --secondmate`, `fm-send`, `fm-backlog-handoff`,
//! `fm-config-push`, the parent channel and secondmate retirement do:
//!
//! - **register** records the charter, placement, harness and projects;
//! - **seed** checks the host is ready, creates the home and clones the
//!   projects on the host itself;
//! - **launch**, **relaunch** and **stop** run its agent in a session on
//!   its host (tmux there, or quarkd's PTY supervisor locally);
//! - **send**, **answer** and **handoff** put a message file in its inbox
//!   and ring its session; the agent acknowledges by moving the file to
//!   `inbox/handled`;
//! - **inherit** writes shared configuration into its home;
//! - **tick** records host health, relaunches an agent whose session ended,
//!   delivers waiting messages, notes acknowledgements and reads the lines
//!   it appended to its parent channel;
//! - **retire** stops it for good and leaves its home in place.
//!
//! Every change is a `subcoordinator.*` event ([`events`]) folded into the
//! [`Registry`], so the log alone restores the engine's view after a crash.
//! [`shadow`] compares that view with firstmate's registry.
//! `docs/engine/sub-coordinators.md` describes the lifecycle.

pub mod events;
pub mod home;
mod manager;
pub mod model;
pub mod readiness;
mod registry;
pub mod shadow;

pub use events::{Cause, Message, MessageKind, SubEvent};
pub use manager::{
    env, Config, Connection, HandoffItem, Hosts, RuntimeHosts, SubCoordinators, Tick,
};
pub use model::{Charter, Placement, Profile, ProjectSource, Registration};
pub use readiness::{Gap, Readiness};
pub use registry::{Generation, Outgoing, Registry, Report, SubCoordinator};
