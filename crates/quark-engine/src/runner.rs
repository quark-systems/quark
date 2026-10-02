//! Allowlisted execution of engine scripts.
//!
//! Mirrors the engine's own remote entrypoint rule: only genuine executable
//! `bin/fm-*.sh` files are run, never a symlink or a path outside `bin/`.
//! The read side allows a fixed set of script + argument vectors; writes will
//! get their own allowlist with argument validation.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::{Error, Result, Workspace};

pub const FLEET_SNAPSHOT: &str = "fm-fleet-snapshot.sh";

/// Read scripts and the exact argument vectors each may be called with.
const READ_ALLOWLIST: &[(&str, &[&[&str]])] = &[(FLEET_SNAPSHOT, &[&["--json"]])];

/// Stderr kept on an adapter record, in bytes.
const STDERR_KEEP: usize = 8 * 1024;

/// One engine script invocation, logged whether it succeeded or not.
#[derive(Debug, Clone)]
pub struct AdapterCall {
    pub script: String,
    pub args: Vec<String>,
    pub started_at: SystemTime,
    pub duration: Duration,
    /// `None` when the script was killed (timeout) or could not start.
    pub exit_code: Option<i32>,
    pub stdout_bytes: usize,
    /// Stderr, truncated to the last 8 KiB.
    pub stderr: String,
    pub timed_out: bool,
}

/// Where adapter call records go. The daemon persists them as `adapter_calls`.
pub trait CallLog: Send + Sync {
    fn record(&self, call: &AdapterCall);
}

/// In-memory call log, useful for tests and as a buffer.
#[derive(Default)]
pub struct MemoryCallLog(Mutex<Vec<AdapterCall>>);

impl MemoryCallLog {
    pub fn calls(&self) -> Vec<AdapterCall> {
        self.0.lock().unwrap().clone()
    }
}

impl CallLog for MemoryCallLog {
    fn record(&self, call: &AdapterCall) {
        self.0.lock().unwrap().push(call.clone());
    }
}

pub struct ScriptRunner {
    workspace: Workspace,
    log: Arc<dyn CallLog>,
    timeout: Duration,
}

impl ScriptRunner {
    pub fn new(workspace: Workspace, log: Arc<dyn CallLog>) -> Self {
        Self {
            workspace,
            log,
            timeout: Duration::from_secs(60),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Run an allowlisted read script with `FM_HOME` set to this workspace and
    /// return its stdout. A non-zero exit is an error carrying stderr.
    pub fn run(&self, script: &str, args: &[&str]) -> Result<Vec<u8>> {
        check_allowed(script, args)?;
        let path = self.genuine_script(script)?;
        let started_at = SystemTime::now();
        let start = Instant::now();
        let mut record = AdapterCall {
            script: script.to_string(),
            args: args.iter().map(|a| a.to_string()).collect(),
            started_at,
            duration: Duration::ZERO,
            exit_code: None,
            stdout_bytes: 0,
            stderr: String::new(),
            timed_out: false,
        };

        let spawned = Command::new(&path)
            .args(args)
            .current_dir(&self.workspace.engine_root)
            .env("FM_HOME", &self.workspace.home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(source) => {
                record.duration = start.elapsed();
                record.stderr = source.to_string();
                self.log.record(&record);
                return Err(Error::Spawn {
                    script: script.to_string(),
                    source,
                });
            }
        };

        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());
        let deadline = start + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                Ok(None) => thread::sleep(Duration::from_millis(10)),
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        let out = stdout.join().unwrap_or_default();
        let err = stderr.join().unwrap_or_default();

        record.duration = start.elapsed();
        record.stdout_bytes = out.len();
        record.stderr = tail_utf8(&err, STDERR_KEEP);
        record.exit_code = status.and_then(|s| s.code());
        record.timed_out = status.is_none();
        self.log.record(&record);

        match status {
            None => Err(Error::Timeout {
                script: script.to_string(),
                seconds: self.timeout.as_secs(),
            }),
            Some(s) if s.success() => Ok(out),
            Some(_) => Err(Error::ScriptFailed {
                script: script.to_string(),
                exit_code: record.exit_code,
                stderr: record.stderr,
            }),
        }
    }

    /// Resolve `bin/<script>` and require a regular, executable, non-symlink
    /// file whose real parent is the engine's `bin/`.
    fn genuine_script(&self, script: &str) -> Result<PathBuf> {
        let bin = self.workspace.engine_bin();
        let path = bin.join(script);
        let not_genuine = |reason: &str| Error::NotGenuine {
            path: path.clone(),
            reason: reason.to_string(),
        };
        let meta = std::fs::symlink_metadata(&path).map_err(|e| not_genuine(&e.to_string()))?;
        if meta.file_type().is_symlink() {
            return Err(not_genuine("is a symlink"));
        }
        if !meta.is_file() {
            return Err(not_genuine("is not a regular file"));
        }
        if meta.permissions().mode() & 0o111 == 0 {
            return Err(not_genuine("is not executable"));
        }
        let real_bin = bin
            .canonicalize()
            .map_err(|e| not_genuine(&e.to_string()))?;
        let real = path
            .canonicalize()
            .map_err(|e| not_genuine(&e.to_string()))?;
        if real.parent() != Some(real_bin.as_path()) {
            return Err(not_genuine("resolves outside the engine bin directory"));
        }
        Ok(path)
    }
}

fn check_allowed(script: &str, args: &[&str]) -> Result<()> {
    let allowed = READ_ALLOWLIST
        .iter()
        .any(|(name, vectors)| *name == script && vectors.contains(&args));
    if allowed {
        Ok(())
    } else {
        Err(Error::NotAllowed {
            script: script.to_string(),
            args: args.iter().map(|a| a.to_string()).collect(),
        })
    }
}

fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    })
}

fn tail_utf8(bytes: &[u8], keep: usize) -> String {
    let start = bytes.len().saturating_sub(keep);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn engine_with(script: &str, body: &str) -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("engine/bin");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(dir.path().join("home/state")).unwrap();
        let p = bin.join(script);
        fs::write(&p, body).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        let ws = Workspace::new(dir.path().join("home"), dir.path().join("engine"));
        (dir, ws)
    }

    #[test]
    fn rejects_unlisted_script_and_args() {
        let (_d, ws) = engine_with(FLEET_SNAPSHOT, "#!/bin/sh\necho {}\n");
        let log = Arc::new(MemoryCallLog::default());
        let r = ScriptRunner::new(ws, log.clone());
        assert!(matches!(
            r.run("fm-send.sh", &[]),
            Err(Error::NotAllowed { .. })
        ));
        assert!(matches!(
            r.run(FLEET_SNAPSHOT, &["--json", "--extra"]),
            Err(Error::NotAllowed { .. })
        ));
        assert!(log.calls().is_empty());
    }

    #[test]
    fn runs_with_fm_home_and_logs() {
        let (_d, ws) = engine_with(
            FLEET_SNAPSHOT,
            "#!/bin/sh\nprintf '%s' \"$FM_HOME\"\necho warn >&2\n",
        );
        let log = Arc::new(MemoryCallLog::default());
        let out = ScriptRunner::new(ws.clone(), log.clone())
            .run(FLEET_SNAPSHOT, &["--json"])
            .unwrap();
        assert_eq!(out, ws.home.to_string_lossy().as_bytes());
        let calls = log.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].exit_code, Some(0));
        assert_eq!(calls[0].args, vec!["--json"]);
        assert_eq!(calls[0].stderr, "warn\n");
    }

    #[test]
    fn failure_is_an_error_and_logged() {
        let (_d, ws) = engine_with(FLEET_SNAPSHOT, "#!/bin/sh\necho boom >&2\nexit 3\n");
        let log = Arc::new(MemoryCallLog::default());
        let err = ScriptRunner::new(ws, log.clone())
            .run(FLEET_SNAPSHOT, &["--json"])
            .unwrap_err();
        assert!(matches!(
            err,
            Error::ScriptFailed {
                exit_code: Some(3),
                ..
            }
        ));
        assert_eq!(log.calls()[0].exit_code, Some(3));
    }

    #[test]
    fn timeout_kills_and_logs() {
        let (_d, ws) = engine_with(FLEET_SNAPSHOT, "#!/bin/sh\nexec sleep 5\n");
        let log = Arc::new(MemoryCallLog::default());
        let err = ScriptRunner::new(ws, log.clone())
            .with_timeout(Duration::from_millis(200))
            .run(FLEET_SNAPSHOT, &["--json"])
            .unwrap_err();
        assert!(matches!(err, Error::Timeout { .. }));
        assert!(log.calls()[0].timed_out);
    }

    #[test]
    fn refuses_symlink_and_non_executable() {
        let (d, ws) = engine_with("real.sh", "#!/bin/sh\n");
        std::os::unix::fs::symlink(
            ws.engine_bin().join("real.sh"),
            ws.engine_bin().join(FLEET_SNAPSHOT),
        )
        .unwrap();
        let r = ScriptRunner::new(ws.clone(), Arc::new(MemoryCallLog::default()));
        assert!(matches!(
            r.run(FLEET_SNAPSHOT, &["--json"]),
            Err(Error::NotGenuine { .. })
        ));

        fs::remove_file(ws.engine_bin().join(FLEET_SNAPSHOT)).unwrap();
        fs::write(ws.engine_bin().join(FLEET_SNAPSHOT), "#!/bin/sh\n").unwrap();
        assert!(matches!(
            r.run(FLEET_SNAPSHOT, &["--json"]),
            Err(Error::NotGenuine { .. })
        ));
        drop(d);
    }
}
