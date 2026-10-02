use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use std::time::UNIX_EPOCH;

use quark_engine::runner::{AdapterCall, CallKind, CallLog};

use crate::engine::firstmate::FirstmateEngine;
use crate::engine::{EngineAdapter, StubEngine};
use crate::store::{ScriptCall, Store};

/// Default localhost port for the daemon API.
pub const DEFAULT_PORT: u16 = 7380;

#[derive(Debug, Clone)]
pub struct Config {
    /// Quark home, `~/.quark` by default.
    pub home: PathBuf,
    pub listen: SocketAddr,
    pub refresh_interval: Duration,
    /// tmux binary for terminal sessions; `tmux` from `PATH` when unset.
    pub tmux: Option<PathBuf>,
}

impl Config {
    pub fn db_path(&self) -> PathBuf {
        self.home.join("quark.db")
    }

    /// Pinned engine checkout, `~/.quark/engine`.
    pub fn engine_root(&self) -> PathBuf {
        self.home.join("engine")
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
    /// Real firstmate homes, read with scripts from the pinned engine checkout.
    Firstmate,
}

/// `tmux` is the `TMUX` value engine scripts run with, when the daemon has
/// terminal sessions.
pub fn build_engine(
    kind: EngineKind,
    config: &Config,
    store: Arc<Store>,
    tmux: Option<String>,
) -> anyhow::Result<Arc<dyn EngineAdapter>> {
    match kind {
        EngineKind::Stub => Ok(Arc::new(StubEngine::new())),
        EngineKind::Firstmate => Ok(Arc::new(
            FirstmateEngine::new(config.engine_root(), Arc::new(StoreCallLog { store }))
                .with_tmux(tmux),
        )),
    }
}

/// Logs every engine script call and keeps an `adapter_calls` record of each
/// write and each failed read. Successful reads run on a short timer, so the
/// projector's per-operation rows and the daemon log cover them.
pub struct StoreCallLog {
    pub store: Arc<Store>,
}

impl CallLog for StoreCallLog {
    fn record(&self, call: &AdapterCall) {
        let ok = call.exit_code == Some(0);
        if ok {
            tracing::debug!(kind = call.kind.as_str(), script = %call.script, args = ?call.args, duration_ms = call.duration.as_millis() as u64, stdout_bytes = call.stdout_bytes, "engine script");
        } else {
            tracing::warn!(kind = call.kind.as_str(), script = %call.script, args = ?call.args, exit_code = ?call.exit_code, timed_out = call.timed_out, stderr = %call.stderr, "engine script failed");
        }
        if ok && call.kind == CallKind::Read {
            return;
        }
        let detail = if call.timed_out {
            Some(format!("timed out; {}", call.stderr))
        } else if ok {
            None
        } else {
            Some(call.stderr.clone())
        };
        let row = ScriptCall {
            ts: rfc3339(call.started_at),
            project_id: None,
            kind: call.kind.as_str().into(),
            script: call.script.clone(),
            args: call.args.clone(),
            workspace: call.workspace.to_string_lossy().into_owned(),
            ok,
            exit_code: call.exit_code,
            duration_ms: call.duration.as_millis() as u64,
            detail,
        };
        if let Err(e) = self.store.record_script_call(&row) {
            tracing::error!(error = %e, script = %call.script, "could not record adapter call");
        }
    }
}

fn rfc3339(t: std::time::SystemTime) -> String {
    let nanos = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0);
    time::OffsetDateTime::from_unix_timestamp_nanos(nanos)
        .ok()
        .and_then(|t| {
            t.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_else(crate::now_rfc3339)
}
