//! Dispatch, account pools and admission control: slice 5 of the native
//! engine.
//!
//! | Step | Module | Native replacement for |
//! |---|---|---|
//! | the Project's rules | [`rules`] | `config/crew-dispatch.json` |
//! | which rule fits the task | [`classify`] | the System-1 call in `fm-dispatch-resolve.sh` |
//! | quota evidence | [`quota`] | `quota-axi --json` plus local readers |
//! | which profile runs it | [`resolve`] | the `jq` selection in `fm-dispatch-resolve.sh` |
//! | which account | [`pools`] | quarkd's account leases |
//! | which host, or wait | [`admission`] | nothing: firstmate has no admission control |
//! | the whole path, as events | [`dispatcher`] | firstmate intake + `fm-spawn.sh` |
//! | agreement with firstmate | [`shadow`] | |
//!
//! Everything the dispatcher decides is a `dispatch.*` event ([`events`])
//! before it acts. Host telemetry comes from `telemetry.sample` events
//! (`quark-hosts`), running tasks from `task.transition` events, hosts from
//! a [`quark_core::HostRegistry`], and the red-main rule from a
//! [`quark_core::MergeGuard`].
//!
//! `docs/engine/dispatch.md` describes the decisions and their defaults.

pub mod admission;
pub mod classify;
pub mod dispatcher;
pub mod events;
pub mod pools;
pub mod quota;
pub mod resolve;
pub mod rules;
pub mod shadow;

pub use admission::{admit, Admission, HostLimits, HostView, Limits, ProjectLimits};
pub use classify::{Classification, Classifier, Given, SystemOne};
pub use dispatcher::{
    Configs, Dispatcher, FixedConfig, IdlePool, LocalSpawner, Parts, Pending, Settings, Spawner,
    Stage, Tick,
};
pub use events::{Choice, Decider, DispatchEvent, Request};
pub use pools::{Accounts, NoAccounts, PoolAccount, StaticAccounts};
pub use quota::{FixedQuota, ProviderFamilies, QuotaAxi, QuotaSnapshot, QuotaSource};
pub use resolve::{resolve, Resolution, Status};
pub use rules::{ClassifierSettings, DispatchConfig, Profile, Select};
