use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use quark_engine::runner::{AdapterCall, CallLog};

use crate::engine::firstmate::FirstmateEngine;
use crate::engine::{EngineAdapter, StubEngine};

/// Default localhost port for the daemon API.
pub const DEFAULT_PORT: u16 = 7380;

#[derive(Debug, Clone)]
pub struct Config {
    /// Quark home, `~/.quark` by default.
    pub home: PathBuf,
    pub listen: SocketAddr,
    pub refresh_interval: Duration,
}

impl Config {
    pub fn db_path(&self) -> PathBuf {
        self.home.join("quark.db")
    }

    /// Pinned engine checkout, `~/.quark/engine`.
    pub fn engine_root(&self) -> PathBuf {
        self.home.join("engine")
    }
}

/// `$QUARK_HOME`, else `$HOME/.quark`.
pub fn default_home() -> PathBuf {
    if let Some(h) = std::env::var_os("QUARK_HOME") {
        return PathBuf::from(h);
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".quark")
}

/// Engine adapters the daemon can be started with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum EngineKind {
    /// In-memory adapter with no engine behind it.
    Stub,
    /// Real firstmate homes, read with scripts from the pinned engine checkout.
    Firstmate,
}

pub fn build_engine(kind: EngineKind, config: &Config) -> anyhow::Result<Arc<dyn EngineAdapter>> {
    match kind {
        EngineKind::Stub => Ok(Arc::new(StubEngine::new())),
        EngineKind::Firstmate => Ok(Arc::new(FirstmateEngine::new(
            config.engine_root(),
            Arc::new(TracingCallLog),
        ))),
    }
}

/// Logs each engine script call. The projector records one `adapter_calls`
/// row per operation; this keeps the per-script detail in the daemon log.
struct TracingCallLog;

impl CallLog for TracingCallLog {
    fn record(&self, call: &AdapterCall) {
        if call.exit_code == Some(0) {
            tracing::debug!(script = %call.script, args = ?call.args, duration_ms = call.duration.as_millis() as u64, stdout_bytes = call.stdout_bytes, "engine script");
        } else {
            tracing::warn!(script = %call.script, args = ?call.args, exit_code = ?call.exit_code, timed_out = call.timed_out, stderr = %call.stderr, "engine script failed");
        }
    }
}
