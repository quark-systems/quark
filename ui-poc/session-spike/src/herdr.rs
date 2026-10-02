//! Thin client for herdr's public surfaces:
//!
//! - [`Api`]: the documented newline-delimited JSON socket API (`herdr.sock`).
//! - [`TermSession`]: the documented `herdr terminal session observe|control`
//!   CLI bridge, which prints one `terminal.frame` JSON record per rendered
//!   frame and (in control mode) reads `terminal.input` / `terminal.resize`
//!   commands on stdin. The CLI speaks herdr's private binary protocol to the
//!   server, so it must be the same herdr build as the server.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{Value, json};

pub type Result<T> = std::result::Result<T, String>;

/// One herdr install plus an isolated config directory (socket, session, logs).
#[derive(Clone)]
pub struct Herdr {
    pub bin: PathBuf,
    pub config_home: PathBuf,
}

impl Herdr {
    pub fn cmd(&self) -> Command {
        let mut c = Command::new(&self.bin);
        c.env("XDG_CONFIG_HOME", &self.config_home)
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_SESSION")
            .env_remove("HERDR_ENV");
        c
    }

    pub fn socket(&self) -> PathBuf {
        self.config_home.join("herdr").join("herdr.sock")
    }

    pub fn server_running(&self) -> bool {
        Api::connect(&self.socket())
            .and_then(|mut a| a.call("ping", json!({})))
            .is_ok()
    }

    /// Start the headless server if needed. `herdr server` stays in the
    /// foreground, so run it in its own process group and do not wait.
    pub fn ensure_server(&self) -> Result<()> {
        if self.server_running() {
            return Ok(());
        }
        use std::os::unix::process::CommandExt as _;
        self.cmd()
            .arg("server")
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn herdr server: {e}"))?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.server_running() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err("herdr server did not come up".into())
    }

    pub fn stop_server(&self) -> Result<()> {
        let st = self
            .cmd()
            .args(["server", "stop"])
            .stdout(Stdio::null())
            .status()
            .map_err(|e| e.to_string())?;
        if !st.success() {
            return Err(format!("herdr server stop: {st}"));
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.server_running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }

    /// PID of the `herdr server` process for this install (via /proc).
    pub fn server_pid(&self) -> Option<u32> {
        let bin = std::fs::canonicalize(&self.bin).ok()?;
        crate::measure::find_pid(|exe, args| *exe == bin && args.iter().any(|a| a == "server"))
    }
}

/// Client for the JSON socket API. herdr serves one request per connection
/// (the server closes it after the response), except `events.subscribe`,
/// which keeps the connection open for pushed events.
pub struct Api {
    sock: PathBuf,
}

impl Api {
    pub fn connect(sock: &Path) -> Result<Self> {
        Ok(Self {
            sock: sock.to_path_buf(),
        })
    }

    fn send(&self, method: &str, params: Value) -> Result<(String, BufReader<UnixStream>)> {
        let mut s = UnixStream::connect(&self.sock)
            .map_err(|e| format!("connect {}: {e}", self.sock.display()))?;
        s.set_read_timeout(Some(Duration::from_secs(30))).ok();
        let id = format!("q-{method}");
        let mut line = json!({"id": id, "method": method, "params": params}).to_string();
        line.push('\n');
        s.write_all(line.as_bytes())
            .map_err(|e| format!("{method}: write: {e}"))?;
        Ok((id, BufReader::new(s)))
    }

    fn response(method: &str, id: &str, r: &mut BufReader<UnixStream>) -> Result<Value> {
        let mut buf = String::new();
        loop {
            buf.clear();
            if r.read_line(&mut buf)
                .map_err(|e| format!("{method}: read: {e}"))?
                == 0
            {
                return Err(format!("{method}: connection closed"));
            }
            let v: Value = serde_json::from_str(&buf).map_err(|e| format!("{method}: {e}"))?;
            if v["id"] != id {
                continue;
            }
            if let Some(err) = v.get("error") {
                return Err(format!("{method}: {err}"));
            }
            return Ok(v["result"].clone());
        }
    }

    pub fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let (id, mut r) = self.send(method, params)?;
        Self::response(method, &id, &mut r)
    }

    /// `events.subscribe`: returns the ack and the open connection.
    pub fn subscribe(&self, subscriptions: Value) -> Result<(Value, Subscription)> {
        let (id, mut r) = self.send("events.subscribe", json!({"subscriptions": subscriptions}))?;
        r.get_ref().set_read_timeout(None).ok();
        let ack = Self::response("events.subscribe", &id, &mut r)?;
        Ok((ack, Subscription(r)))
    }
}

pub struct Subscription(BufReader<UnixStream>);

impl Subscription {
    /// Next pushed event line.
    pub fn next_line(&mut self) -> Option<String> {
        let mut buf = String::new();
        match self.0.read_line(&mut buf) {
            Ok(n) if n > 0 => Some(buf),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum StreamEvent {
    Frame(Frame),
    Closed(String),
}

#[derive(Debug)]
pub struct Frame {
    pub at: Instant,
    pub width: u16,
    pub height: u16,
    pub full: bool,
    pub bytes: Vec<u8>,
}

/// A running `herdr terminal session observe|control` child.
pub struct TermSession {
    child: Child,
    stdin: Option<ChildStdin>,
    pub events: Receiver<StreamEvent>,
    pub started: Instant,
}

impl TermSession {
    pub fn observe(h: &Herdr, target: &str, cols: u16, rows: u16) -> Result<Self> {
        Self::spawn(h, &["observe", target], cols, rows, false)
    }

    pub fn control(h: &Herdr, target: &str, cols: u16, rows: u16, takeover: bool) -> Result<Self> {
        let mut args = vec!["control", target];
        if takeover {
            args.push("--takeover");
        }
        Self::spawn(h, &args, cols, rows, true)
    }

    fn spawn(h: &Herdr, args: &[&str], cols: u16, rows: u16, writable: bool) -> Result<Self> {
        let mut cmd = h.cmd();
        cmd.args(["terminal", "session"])
            .args(args)
            .args(["--cols", &cols.to_string(), "--rows", &rows.to_string()])
            .stdin(if writable {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let started = Instant::now();
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("spawn terminal session: {e}"))?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stdin = child.stdin.take();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut r = BufReader::with_capacity(1 << 20, stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match r.read_line(&mut line) {
                    Ok(0) | Err(_) => {
                        let _ = tx.send(StreamEvent::Closed("eof".into()));
                        return;
                    }
                    Ok(_) => {}
                }
                let at = Instant::now();
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let ev = match v["type"].as_str() {
                    Some("terminal.frame") => StreamEvent::Frame(Frame {
                        at,
                        width: v["width"].as_u64().unwrap_or(0) as u16,
                        height: v["height"].as_u64().unwrap_or(0) as u16,
                        full: v["full"].as_bool().unwrap_or(false),
                        bytes: B64
                            .decode(v["bytes"].as_str().unwrap_or(""))
                            .unwrap_or_default(),
                    }),
                    Some("terminal.closed") => {
                        StreamEvent::Closed(v["reason"].as_str().unwrap_or("").to_string())
                    }
                    _ => continue,
                };
                if tx.send(ev).is_err() {
                    return;
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            events: rx,
            started,
        })
    }

    fn command(&mut self, v: Value) -> Result<()> {
        let stdin = self.stdin.as_mut().ok_or("observer has no input")?;
        let mut line = v.to_string();
        line.push('\n');
        stdin
            .write_all(line.as_bytes())
            .and_then(|_| stdin.flush())
            .map_err(|e| format!("terminal session stdin: {e}"))
    }

    pub fn input(&mut self, bytes: &[u8]) -> Result<()> {
        self.command(json!({"type": "terminal.input", "bytes": B64.encode(bytes)}))
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.command(json!({"type": "terminal.resize", "cols": cols, "rows": rows}))
    }

    /// Kill the CLI bridge without a clean release (simulates a quarkd crash).
    pub fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for TermSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
