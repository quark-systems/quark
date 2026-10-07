//! The worker protocol (slice 3), shadowed beside the bash engine.
//!
//! With slice 3 in shadow (`QUARK_ENGINE_SLICES=...,3=shadow`, or
//! `QUARK_SHADOWS=all`), every status line a firstmate worker wrote, as the
//! slice 1 bridge mirrored it, is read by the native file protocol too, and
//! a reading that differs from firstmate's is a `shadow.divergence`
//! (operation `status_line`). See `quark_worker::shadow`. Firstmate still
//! reads every line and acts on it; this only compares.

use quark_core::{Event, Slice};
use quark_eventlog::firstmate::{kinds, StatusPayload};
use quark_worker::shadow;

use crate::log_shadow::{Found, Judge};

/// Judges each `firstmate.status` line.
pub struct StatusLines;

impl Judge for StatusLines {
    fn slice(&self) -> Slice {
        Slice::WorkerProtocol
    }

    fn checkpoint(&self) -> &'static str {
        "shadow/worker-protocol"
    }

    fn judge(&mut self, event: &Event) -> Vec<Found> {
        if event.kind.as_str() != kinds::STATUS {
            return Vec::new();
        }
        let Ok(line) = event.decode::<StatusPayload>() else {
            return Vec::new();
        };
        shadow::compare(&line.raw)
            .map(|(bash, native)| Found {
                project: None,
                operation: shadow::OPERATION.to_string(),
                bash: serde_json::to_value(bash).unwrap_or_default(),
                native: serde_json::to_value(native).unwrap_or_default(),
            })
            .into_iter()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use quark_core::{EventLog, HostId, ProjectId, Seq};
    use quark_eventlog::{FirstmateBridge, SqliteEventLog};

    use super::*;
    use crate::log_shadow::LogShadow;

    #[tokio::test]
    async fn misread_lines_are_divergences() {
        let home = tempfile::tempdir().unwrap();
        let state = home.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let mut f = std::fs::File::create(state.join("t1.status")).unwrap();
        f.write_all(
            b"working: go\nblocked [key=creds]: need a token\nresolved [key=creds]: given\ndone: PR https://github.com/o/r/pull/1\n",
        )
        .unwrap();
        let db = tempfile::tempdir().unwrap();
        let log = SqliteEventLog::open(db.path().join("events.db")).unwrap();
        FirstmateBridge::new(log.clone(), HostId::from("h"))
            .ingest(&ProjectId::from("p"), home.path())
            .await
            .unwrap();
        let mut shadow = LogShadow::new(log.clone(), HostId::from("h"), StatusLines);
        let pass = shadow.pass().await.unwrap();
        assert_eq!(pass.diverged, 1);
        let d: quark_core::slice::Divergence = log
            .read(Seq::ZERO, 100)
            .await
            .unwrap()
            .iter()
            .find(|e| e.kind.as_str() == quark_core::event::kinds::SHADOW_DIVERGENCE)
            .unwrap()
            .decode()
            .unwrap();
        assert_eq!(d.slice, Slice::WorkerProtocol);
        assert_eq!(d.bash["key"], "creds");
        assert!(d.native.get("key").is_none());
    }
}
