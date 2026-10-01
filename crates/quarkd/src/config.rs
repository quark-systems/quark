use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

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
}

pub fn build_engine(kind: EngineKind, _config: &Config) -> anyhow::Result<Arc<dyn EngineAdapter>> {
    match kind {
        EngineKind::Stub => Ok(Arc::new(StubEngine::new())),
    }
}
