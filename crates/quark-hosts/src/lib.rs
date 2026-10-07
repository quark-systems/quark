//! Host resource telemetry for the native Quark engine.
//!
//! [`HostSampler`] implements [`quark_core::Telemetry`]: each call to
//! `sample` reads the host's CPU, memory, memory pressure and free disk,
//! sizes the directories Quark itself fills (worktrees, logs, caches, the
//! event log), and attributes CPU, memory and disk to each Project and task
//! from its worker's process tree and worktree path.
//!
//! [`recorder::run`] samples on an interval and appends each sample to the
//! [`quark_core::EventLog`] as a [`recorder::SAMPLE`] event, so the dashboard
//! reads the time series from the log.
//!
//! What runs where comes from a [`Workloads`] source. The supervisor will
//! provide the real one; until then [`StaticWorkloads`] holds a list set by
//! the caller. Admission control (holding or placing workers by capacity)
//! is not in this crate yet; it reads `sample` when it lands.

pub mod attribution;
pub mod disk;
pub mod pressure;
pub mod probe;
pub mod recorder;
pub mod sampler;

pub use attribution::{StaticWorkloads, Workload, Workloads};
pub use probe::{Probe, ProcessInfo, Reading, SystemProbe};
pub use sampler::{HostSampler, QuarkPaths, SamplerConfig};
