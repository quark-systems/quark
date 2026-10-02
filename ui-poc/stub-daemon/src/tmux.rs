//! Live worker panes backed by a private tmux server in control mode.
//!
//! One session (`quark-poc`) holds one window per worker. A single
//! control-mode client (`tmux -C attach`) receives `%output` notifications
//! for every pane in that session; we decode them and publish
//! `worker.output` events. Input, resize and stress are sent as tmux commands
//! over the same client's stdin.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::model::Worker;
use crate::store::Hub;

/// tmux server socket name (`tmux -L <socket>`); set once by [`set_socket`].
static SOCKET: std::sync::OnceLock<String> = std::sync::OnceLock::new();
const SESSION: &str = "quark-poc";
const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 36;
/// Coalesce consecutive `%output` lines per pane up to this many bytes
/// while more input is already buffered; flush immediately when idle so
/// echo latency is not affected.
const COALESCE_LIMIT: usize = 64 * 1024;

const SCRIPTS: [(&str, &str); 6] = [
    ("cargo-loop.sh", include_str!("../scripts/cargo-loop.sh")),
    ("monitor.sh", include_str!("../scripts/monitor.sh")),
    ("colors.sh", include_str!("../scripts/colors.sh")),
    ("shell.sh", include_str!("../scripts/shell.sh")),
    ("flood.sh", include_str!("../scripts/flood.sh")),
    ("loop.sh", include_str!("../scripts/loop.sh")),
];

const TMUX_CONF: &str = "\
set -g default-terminal tmux-256color
set -g default-size 120x36
set -g status off
set -g history-limit 5000
set -g escape-time 0
set-environment -g COLORTERM truecolor
set-environment -g LANG C.UTF-8
set-environment -g LC_ALL C.UTF-8
";

struct Pane {
    worker: Worker,
    project_id: String,
    pane_id: String,
    window_id: String,
    stressed: bool,
}

pub struct Tmux {
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
    panes: Arc<Mutex<Vec<Pane>>>,
    dir: PathBuf,
}

/// Choose the private tmux socket: `quark-poc` on the default port,
/// `quark-poc-<port>` otherwise, so several stub instances can coexist.
pub fn set_socket(port: u16) {
    let name = if port == 7420 {
        "quark-poc".to_string()
    } else {
        format!("quark-poc-{port}")
    };
    let _ = SOCKET.set(name);
}

pub fn socket() -> &'static str {
    SOCKET.get().map(String::as_str).unwrap_or("quark-poc")
}

fn tmux() -> Command {
    let mut cmd = Command::new("tmux");
    cmd.args(["-L", socket()]);
    cmd
}

/// Kill the private tmux server (ignores "no server running").
pub fn kill_server() {
    let _ = tmux()
        .arg("kill-server")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Decode tmux control-mode escaping: `\ooo` octal escapes become bytes,
/// everything else is passed through verbatim.
pub fn decode_octal(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'\\'
            && i + 3 < input.len()
            && input[i + 1..i + 4]
                .iter()
                .all(|b| (b'0'..=b'7').contains(b))
        {
            let v = (input[i + 1] - b'0') as u16 * 64
                + (input[i + 2] - b'0') as u16 * 8
                + (input[i + 3] - b'0') as u16;
            out.push(v as u8);
            i += 4;
        } else {
            out.push(input[i]);
            i += 1;
        }
    }
    out
}

impl Tmux {
    /// Start the tmux server, the four worker windows and the control client.
    /// `bindings` is `(worker_id, task_id, project_id, title)` per pane.
    pub fn start(hub: Arc<Hub>, bindings: &[(&str, &str, &str, &str)]) -> std::io::Result<Self> {
        kill_server();
        remove_stale_script_dirs();
        let dir = std::env::temp_dir().join(format!("quark-ui-stub-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        for (name, body) in SCRIPTS {
            let path = dir.join(name);
            std::fs::write(&path, body)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
            }
        }
        let conf = dir.join("tmux.conf");
        std::fs::write(&conf, TMUX_CONF)?;

        let scripts = ["cargo-loop.sh", "monitor.sh", "colors.sh", "shell.sh"];
        let mut panes = Vec::new();
        for (i, ((worker_id, task_id, project_id, title), script)) in
            bindings.iter().zip(scripts).enumerate()
        {
            let cmd = command_for(&dir, script, worker_id);
            let mut c = tmux();
            if i == 0 {
                c.arg("-f")
                    .arg(&conf)
                    .args(["new-session", "-d", "-s", SESSION]);
                c.args([
                    "-x",
                    &DEFAULT_COLS.to_string(),
                    "-y",
                    &DEFAULT_ROWS.to_string(),
                ]);
            } else {
                c.args(["new-window", "-d", "-t", SESSION]);
            }
            let out = c
                .args(["-n", worker_id, "-P", "-F", "#{window_id} #{pane_id}", &cmd])
                .output()?;
            if !out.status.success() {
                return Err(std::io::Error::other(format!(
                    "tmux failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                )));
            }
            let ids = String::from_utf8_lossy(&out.stdout);
            let mut ids = ids.split_whitespace();
            let window_id = ids.next().unwrap_or_default().to_string();
            let pane_id = ids.next().unwrap_or_default().to_string();
            panes.push(Pane {
                worker: Worker {
                    id: worker_id.to_string(),
                    task_id: task_id.to_string(),
                    title: title.to_string(),
                    cols: DEFAULT_COLS,
                    rows: DEFAULT_ROWS,
                },
                project_id: project_id.to_string(),
                pane_id,
                window_id,
                stressed: false,
            });
        }
        // resize-window also switches each window to `window-size manual`, so
        // the control client never resizes it. (Setting `window-size manual`
        // globally crashes the tmux 3.4 server, so it is done per window.)
        for p in &panes {
            let _ = tmux()
                .args(["resize-window", "-t", &p.window_id, "-x"])
                .arg(DEFAULT_COLS.to_string())
                .arg("-y")
                .arg(DEFAULT_ROWS.to_string())
                .status();
        }

        let mut child = tmux()
            .args(["-C", "attach", "-t", SESSION])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");

        let panes = Arc::new(Mutex::new(panes));
        let reader_panes = panes.clone();
        let (attached_tx, attached_rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("tmux-control-reader".into())
            .spawn(move || read_control(stdout, hub, reader_panes, attached_tx))?;

        // Output produced before the control client attaches (the shell's
        // first prompt, top's first paint) is never sent as %output, so the
        // pane scripts wait for this file before starting.
        if attached_rx.recv_timeout(Duration::from_secs(3)).is_err() {
            eprintln!("warning: tmux control client did not report in; starting panes anyway");
        }
        std::fs::write(dir.join("attached"), b"")?;

        Ok(Self {
            stdin: Mutex::new(stdin),
            child: Mutex::new(child),
            panes,
            dir,
        })
    }

    fn send_command(&self, cmd: &str) -> std::io::Result<()> {
        let mut stdin = self.stdin.lock().unwrap();
        stdin.write_all(cmd.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()
    }

    pub fn workers(&self) -> Vec<Worker> {
        self.panes
            .lock()
            .unwrap()
            .iter()
            .map(|p| p.worker.clone())
            .collect()
    }

    fn pane_field<T>(&self, worker_id: &str, f: impl FnOnce(&mut Pane) -> T) -> Option<T> {
        self.panes
            .lock()
            .unwrap()
            .iter_mut()
            .find(|p| p.worker.id == worker_id)
            .map(f)
    }

    /// Type raw bytes into a pane via `send-keys -H`.
    pub fn input(&self, worker_id: &str, bytes: &[u8]) -> Option<std::io::Result<()>> {
        let pane = self.pane_field(worker_id, |p| p.pane_id.clone())?;
        Some((|| {
            for chunk in bytes.chunks(512) {
                let mut cmd = format!("send-keys -t {pane} -H");
                for b in chunk {
                    write!(cmd, " {b:02x}").unwrap();
                }
                self.send_command(&cmd)?;
            }
            Ok(())
        })())
    }

    /// Resize the worker's window; the app receives SIGWINCH and redraws.
    pub fn resize(&self, worker_id: &str, cols: u16, rows: u16) -> Option<std::io::Result<Worker>> {
        let cols = cols.clamp(10, 500);
        let rows = rows.clamp(3, 200);
        let (window, worker) = self.pane_field(worker_id, |p| {
            p.worker.cols = cols;
            p.worker.rows = rows;
            (p.window_id.clone(), p.worker.clone())
        })?;
        Some(
            self.send_command(&format!("resize-window -t {window} -x {cols} -y {rows}"))
                .map(|_| worker),
        )
    }

    /// Toggle flood mode. The pane's restart loop runs `flood.sh` while the
    /// worker's flag file exists; we flip the flag and stop the current
    /// program so the loop picks up the new mode.
    ///
    /// (`respawn-pane -k` would be simpler, but tmux 3.4 crashes when a pane
    /// that is flooding a control client is respawned.)
    pub fn stress(&self, worker_id: &str, on: bool) -> Option<std::io::Result<()>> {
        let (pane, changed) = self.pane_field(worker_id, |p| {
            let changed = p.stressed != on;
            p.stressed = on;
            (p.pane_id.clone(), changed)
        })?;
        if !changed {
            return Some(Ok(()));
        }
        let flag = self.dir.join(format!("{worker_id}.stress"));
        let r = if on {
            std::fs::write(&flag, b"")
        } else {
            std::fs::remove_file(&flag)
        };
        if r.is_ok() {
            std::thread::spawn(move || restart_pane_program(&pane));
        }
        Some(r)
    }

    pub fn shutdown(&self) {
        let _ = self.child.lock().unwrap().kill();
        kill_server();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Remove script directories left by earlier instances that are no longer
/// running (best effort; uses /proc, so a no-op off Linux).
fn remove_stale_script_dirs() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.strip_prefix("quark-ui-stub-")) else {
            continue;
        };
        if Path::new("/proc/self").exists() && !Path::new("/proc").join(pid).exists() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Shell command run in a pane: the script wrapped in the restart loop,
/// which switches to `flood.sh` while `<worker_id>.stress` exists.
fn command_for(dir: &Path, script: &str, worker_id: &str) -> String {
    format!(
        "{} {} {}",
        dir.join("loop.sh").display(),
        dir.join(script).display(),
        dir.join(format!("{worker_id}.stress")).display()
    )
}

/// Stop the program currently running under a pane's restart loop: SIGHUP
/// (which top, bash and the scripts all exit on cleanly), then SIGKILL for
/// anything still alive. The loop then starts the next program.
fn restart_pane_program(pane_id: &str) {
    let Ok(out) = tmux()
        .args(["display", "-p", "-t", pane_id, "#{pane_pid}"])
        .output()
    else {
        return;
    };
    let loop_pid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let Ok(out) = Command::new("pgrep").args(["-P", &loop_pid]).output() else {
        return;
    };
    let children: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .map(str::to_string)
        .collect();
    if children.is_empty() {
        return;
    }
    let _ = Command::new("kill").arg("-HUP").args(&children).status();
    std::thread::sleep(Duration::from_millis(300));
    let _ = Command::new("kill")
        .arg("-KILL")
        .args(&children)
        .stderr(Stdio::null())
        .status();
}

/// Control-mode reader: parses `%output %<pane> <escaped>` lines and
/// publishes decoded bytes. Runs on a dedicated OS thread.
fn read_control(
    stdout: std::process::ChildStdout,
    hub: Arc<Hub>,
    panes: Arc<Mutex<Vec<Pane>>>,
    attached: std::sync::mpsc::Sender<()>,
) {
    let mut reader = BufReader::with_capacity(256 * 1024, stdout);
    let mut pending: HashMap<String, Vec<u8>> = HashMap::new();
    let mut line = Vec::new();

    let flush = |pending: &mut HashMap<String, Vec<u8>>| {
        if pending.is_empty() {
            return;
        }
        let panes = panes.lock().unwrap();
        for (pane_id, data) in pending.drain() {
            if let Some(p) = panes.iter().find(|p| p.pane_id == pane_id) {
                hub.publish_worker_output(&p.project_id, &p.worker.id, &data);
            }
        }
    };

    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        // The first line (the attach command's %begin) means we are attached.
        let _ = attached.send(());
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if let Some(rest) = line.strip_prefix(b"%output ") {
            let split = rest.iter().position(|&b| b == b' ').unwrap_or(rest.len());
            let pane_id = String::from_utf8_lossy(&rest[..split]).into_owned();
            let data = decode_octal(rest.get(split + 1..).unwrap_or_default());
            let buf = pending.entry(pane_id).or_default();
            buf.extend_from_slice(&data);
            if buf.len() >= COALESCE_LIMIT {
                flush(&mut pending);
            }
        } else if line.starts_with(b"%error") {
            eprintln!("tmux: {}", String::from_utf8_lossy(&line));
        } else if line.starts_with(b"%exit") {
            eprintln!(
                "tmux control client exited: {}",
                String::from_utf8_lossy(&line)
            );
        }
        // Flush as soon as no more control output is already buffered.
        if reader.buffer().is_empty() {
            flush(&mut pending);
        }
    }
    flush(&mut pending);
}

#[cfg(test)]
mod tests {
    use super::decode_octal;

    #[test]
    fn decodes_octal_escapes() {
        assert_eq!(decode_octal(br"a\033[0m\015\012b"), b"a\x1b[0m\r\nb");
        assert_eq!(decode_octal(br"\134"), b"\\");
        assert_eq!(decode_octal(br"tail\0"), b"tail\\0");
    }
}
