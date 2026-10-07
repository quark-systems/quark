use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use std::time::UNIX_EPOCH;

use quark_core::{Slice, SliceMode, SliceSwitch};
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
    check_slices(&crate::engine::shadow::slices_from_env()?)?;
    match kind {
        EngineKind::Stub => Ok(Arc::new(StubEngine::new())),
        EngineKind::Firstmate => Ok(Arc::new(
            FirstmateEngine::new(config.engine_root(), Arc::new(StoreCallLog { store }))
                .with_tmux(tmux),
        )),
    }
}

/// The slices with a native side that can run in shadow: slice 1's ingest
/// bridge (`event_ingest`), slice 2's decision shadow (`verify_shadow`),
/// slice 3's status-line reading (`worker_shadow`) and slice 4's supervision
/// rules (`engine::eventlog::SupervisionCheck`). None acts, so firstmate
/// keeps serving every call.
pub const SHADOWABLE: [Slice; 4] = [
    Slice::EventLog,
    Slice::Verification,
    Slice::WorkerProtocol,
    Slice::Supervision,
];

/// The slices quarkd can run native: slice 1, whose reads the event log
/// answers ([`crate::engine::shadow`]).
pub const NATIVE: [Slice; 1] = [Slice::EventLog];

/// Refuses any slice mode quarkd can't run: `shadow` outside
/// [`SHADOWABLE`], and `native` outside [`NATIVE`], rather than silently
/// ignoring it.
pub fn check_slices(slices: &SliceSwitch) -> anyhow::Result<()> {
    for (slice, mode) in slices.modes() {
        let ok = match mode {
            SliceMode::Bash => true,
            SliceMode::Shadow => SHADOWABLE.contains(&slice),
            SliceMode::Native => NATIVE.contains(&slice),
        };
        if !ok {
            anyhow::bail!(
                "slice {slice} can't be {}: no native engine for it yet",
                mode.as_str()
            );
        }
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_slices_one_to_four_may_shadow_and_only_slice_one_run_native() {
        for ok in [
            "",
            "1=shadow",
            "1=shadow,2=shadow",
            "1=shadow,2=shadow,3=shadow",
            "1=shadow,2=shadow,3=shadow,4=shadow",
            "1=native",
            "1=native,2=shadow,3=shadow,4=shadow",
        ] {
            assert!(
                check_slices(&SliceSwitch::parse(ok).unwrap()).is_ok(),
                "{ok}"
            );
        }
        for bad in [
            "1=native,2=native",
            "1=shadow,2=shadow,3=shadow,4=shadow,5=shadow",
        ] {
            let err = check_slices(&SliceSwitch::parse(bad).unwrap()).unwrap_err();
            assert!(
                err.to_string().contains("no native engine for it yet"),
                "{bad}"
            );
        }
    }
}
