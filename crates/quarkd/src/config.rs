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
    /// How often open pull requests are read from their forge.
    pub pr_refresh_interval: Duration,
    /// tmux binary for terminal sessions; `tmux` from `PATH` when unset.
    pub tmux: Option<PathBuf>,
    /// How often each account's quota is read.
    pub quota_refresh_interval: Duration,
    /// The `quota-axi` binary that reads account quota.
    pub quota_axi: PathBuf,
    /// User-level memory directory; `<home>/memory` when unset.
    pub user_memory: Option<PathBuf>,
}

impl Config {
    pub fn db_path(&self) -> PathBuf {
        self.home.join("quark.db")
    }

    /// The native engine's event log, separate from the projection store.
    pub fn events_path(&self) -> PathBuf {
        self.home.join(quark_eventlog::FILE_NAME)
    }

    /// Runtime files such as workspace tmux sockets, `~/.quark/run`.
    pub fn run_dir(&self) -> PathBuf {
        self.home.join("run")
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
    /// quarkd's own engine: the native supervisor over the event log
    /// ([`crate::engine::native`]).
    Native,
}

/// The engine for `kind`. The native engine needs the running supervisor,
/// so `serve` builds it.
pub fn build_engine(kind: EngineKind) -> anyhow::Result<Arc<dyn EngineAdapter>> {
    match kind {
        EngineKind::Stub => Ok(Arc::new(StubEngine::new())),
        EngineKind::Native => anyhow::bail!("the native engine is built by serve"),
    }
}
