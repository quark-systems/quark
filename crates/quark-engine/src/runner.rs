//! Allowlisted execution of engine scripts.
//!
//! Mirrors the engine's own remote entrypoint rule: only genuine executable
//! `bin/fm-*.sh` files are run, never a symlink or a path outside `bin/`.
//! Reads allow a fixed set of script + argument vectors. Writes are typed
//! [`WriteOp`]s that validate every field and render their own argument vector,
//! so no caller string ever becomes a script name or an option.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::write::WriteOp;
use crate::{Error, Result, Workspace};

pub const FLEET_SNAPSHOT: &str = "fm-fleet-snapshot.sh";
pub const DISPATCH_RESOLVE: &str = "fm-dispatch-resolve.sh";

/// Read scripts and the exact argument vectors each may be called with.
const READ_ALLOWLIST: &[(&str, &[&[&str]])] = &[(FLEET_SNAPSHOT, &[&["--json"]])];

/// Stderr kept on an adapter record, in bytes.
const STDERR_KEEP: usize = 8 * 1024;

/// Whether a call only reads engine state or changes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    Read,
    Write,
}

impl CallKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CallKind::Read => "read",
            CallKind::Write => "write",
        }
    }
}

/// One engine script invocation, logged whether it succeeded or not.
#[derive(Debug, Clone)]
pub struct AdapterCall {
    pub kind: CallKind,
    /// The workspace (`FM_HOME`) the script ran against.
    pub workspace: PathBuf,
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
    /// Overrides each write operation's own timeout when set.
    write_timeout: Option<Duration>,
}

impl ScriptRunner {
    pub fn new(workspace: Workspace, log: Arc<dyn CallLog>) -> Self {
        Self {
            workspace,
            log,
            timeout: Duration::from_secs(60),
            write_timeout: None,
        }
    }

    /// Timeout for reads (default 60s).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// One timeout for every write, replacing each operation's own bound.
    pub fn with_write_timeout(mut self, timeout: Duration) -> Self {
        self.write_timeout = Some(timeout);
        self
    }

    /// Run an allowlisted read script with `FM_HOME` set to this workspace and
    /// return its stdout. A non-zero exit is an error carrying stderr.
    pub fn run(&self, script: &str, args: &[&str]) -> Result<Vec<u8>> {
        check_allowed(script, args)?;
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        self.exec(CallKind::Read, script, args, &[], self.timeout)
    }

    /// Run the engine's dispatch resolution on one task's brief and return
    /// its stdout. The argument vector is built here from a validated task id
    /// and project name; no caller string becomes an option.
    pub fn run_dispatch_resolve(&self, task_id: &str, project: Option<&str>) -> Result<Vec<u8>> {
        let brief = self.workspace.brief_path(task_id)?;
        let mut args = vec![brief.to_string_lossy().into_owned()];
        if let Some(p) = project {
            if !safe_name(p) {
                return Err(Error::InvalidArgument {
                    script: DISPATCH_RESOLVE,
                    reason: format!("project name {p:?} is not path-safe"),
                });
            }
            args.extend(["--project".to_string(), p.to_string()]);
        }
        self.exec(CallKind::Read, DISPATCH_RESOLVE, args, &[], self.timeout)
    }

    /// Validate a write operation, run its script and return its stdout.
    /// Nothing runs, and nothing is logged, when validation fails.
    pub fn run_write(&self, op: &WriteOp) -> Result<Vec<u8>> {
        let args = op.argv()?;
        let timeout = self.write_timeout.unwrap_or_else(|| op.timeout());
        self.exec(CallKind::Write, op.script(), args, &op.env(), timeout)
    }

    fn exec(
        &self,
        kind: CallKind,
        script: &str,
        args: Vec<String>,
        env: &[(&'static str, String)],
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        let path = self.genuine_script(script)?;
        let started_at = SystemTime::now();
        let start = Instant::now();
        let mut record = AdapterCall {
            kind,
            workspace: self.workspace.home.clone(),
            script: script.to_string(),
            args: args.clone(),
            started_at,
            duration: Duration::ZERO,
            exit_code: None,
            stdout_bytes: 0,
            stderr: String::new(),
            timed_out: false,
        };

        let mut command = Command::new(&path);
        command
            .args(&args)
            .current_dir(&self.workspace.engine_root)
            .envs(
                self.workspace
                    .env
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str())),
            )
            .env("FM_HOME", &self.workspace.home)
            .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = match spawn_retrying_busy(&mut command, start + timeout) {
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
        let deadline = start + timeout;
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
                seconds: timeout.as_secs(),
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

/// A repo registry name: `A-Za-z0-9._-`, not starting with `-` or `.`.
fn safe_name(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with(['-', '.'])
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
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

/// Spawns `command`, retrying while the script is "text file busy" (ETXTBSY).
///
/// Linux refuses to exec a file that some process still holds open for writing. A script
/// that was just written can stay open briefly in a child forked concurrently by another
/// thread, before that child's exec closes its copy of the descriptor; the same happens
/// while an engine update rewrites its scripts. The condition clears on its own.
fn spawn_retrying_busy(command: &mut Command, deadline: Instant) -> std::io::Result<Child> {
    loop {
        match command.spawn() {
            Err(e)
                if e.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(5));
            }
            other => return other,
        }
    }
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
        assert_eq!(calls[0].kind, CallKind::Read);
        assert_eq!(calls[0].args, vec!["--json"]);
        assert_eq!(calls[0].stderr, "warn\n");
    }

    #[test]
    fn write_runs_validated_argv_and_logs_kind() {
        let (_d, ws) = engine_with(
            crate::write::SEND,
            "#!/bin/sh\nprintf '%s|%s|%s' \"$FM_HOME\" \"$1\" \"$2\"\n",
        );
        let log = Arc::new(MemoryCallLog::default());
        let r = ScriptRunner::new(ws.clone(), log.clone());
        let op = WriteOp::Send {
            task_id: "t1".into(),
            text: "add tests".into(),
        };
        let out = r.run_write(&op).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("{}|t1|add tests", ws.home.display())
        );
        let calls = log.calls();
        assert_eq!(calls[0].kind, CallKind::Write);
        assert_eq!(calls[0].workspace, ws.home);
        assert_eq!(calls[0].args, vec!["t1", "add tests"]);

        // Invalid input never starts the script and leaves no record.
        let bad = WriteOp::Send {
            task_id: "t1".into(),
            text: "/quit".into(),
        };
        assert!(matches!(
            r.run_write(&bad),
            Err(Error::InvalidArgument { .. })
        ));
        assert_eq!(log.calls().len(), 1);
    }

    #[test]
    fn write_env_reaches_the_script() {
        let (_d, ws) = engine_with(
            crate::write::HOME_SEED,
            "#!/bin/sh\nprintf '%s|%s|home=%s' \"$FM_SECONDMATE_CHARTER\" \"$FM_SECONDMATE_SCOPE\" \"$2\"\n",
        );
        let r = ScriptRunner::new(ws, Arc::new(MemoryCallLog::default()));
        let op = WriteOp::HomeSeed {
            id: "p1".into(),
            home: "/q/ws/p1".into(),
            projects: vec!["r".into()],
            charter: "Run\nit.".into(),
            scope: "All.".into(),
        };
        let out = String::from_utf8(r.run_write(&op).unwrap()).unwrap();
        assert_eq!(out, "Run it.|All.|home=/q/ws/p1");
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
