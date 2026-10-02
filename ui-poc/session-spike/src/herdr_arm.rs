//! The herdr arm: four worker panes driven only through herdr's public
//! surfaces (JSON socket API + `herdr terminal session` CLI bridge).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::herdr::{Api, Herdr, Result, StreamEvent, TermSession};
use crate::measure::{self, CpuWindow, EchoTarget, Stats};

pub const WORKSPACE: &str = "quark-spike";
pub const COLS: u16 = 120;
pub const ROWS: u16 = 36;

/// Same programs as the stub daemon's tmux panes.
pub const WORKERS: [(&str, &str); 4] = [
    ("w-1", "cargo-loop.sh"),
    ("w-2", "monitor.sh"),
    ("w-3", "colors.sh"),
    ("w-4", "shell.sh"),
];

const SCRIPTS: [(&str, &str); 6] = [
    (
        "cargo-loop.sh",
        include_str!("../../stub-daemon/scripts/cargo-loop.sh"),
    ),
    (
        "monitor.sh",
        include_str!("../../stub-daemon/scripts/monitor.sh"),
    ),
    (
        "colors.sh",
        include_str!("../../stub-daemon/scripts/colors.sh"),
    ),
    (
        "shell.sh",
        include_str!("../../stub-daemon/scripts/shell.sh"),
    ),
    (
        "flood.sh",
        include_str!("../../stub-daemon/scripts/flood.sh"),
    ),
    ("loop.sh", include_str!("../../stub-daemon/scripts/loop.sh")),
];

pub struct Ctx {
    pub h: Herdr,
    pub scripts: PathBuf,
    pub panes: Vec<(String, String)>, // (worker id, herdr pane id)
}

impl Ctx {
    pub fn pane(&self, worker: &str) -> &str {
        &self.panes.iter().find(|(w, _)| w == worker).unwrap().1
    }
    pub fn api(&self) -> Result<Api> {
        Api::connect(&self.h.socket())
    }
}

/// Write the pane scripts to `<run>/scripts`. `loop.sh` waits for an
/// `attached` marker (a tmux control-mode concern); herdr keeps the screen
/// server-side, so we create it straight away.
pub fn install_scripts(run: &Path) -> Result<PathBuf> {
    let dir = run.join("scripts");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for (name, body) in SCRIPTS {
        let p = dir.join(name);
        std::fs::write(&p, body).map_err(|e| e.to_string())?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    std::fs::write(dir.join("attached"), b"").map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Find the spike workspace from an earlier run, or create it: one tab per
/// worker, each a single pane running `loop.sh <script> <stress-flag>`.
/// Returns whether existing panes were reused.
pub fn find_or_create(h: &Herdr, scripts: &Path) -> Result<(Vec<(String, String)>, bool)> {
    let mut api = Api::connect(&h.socket())?;
    let snap = api.call("session.snapshot", json!({}))?["snapshot"].clone();
    let ws = snap["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|w| w["label"] == WORKSPACE)
        .cloned();
    if let Some(ws) = ws {
        let wid = ws["workspace_id"].as_str().unwrap_or_default();
        let mut found = Vec::new();
        for (worker, _) in WORKERS {
            if let Some(p) = snap["panes"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|p| p["workspace_id"] == wid && p["label"] == worker)
            {
                found.push((
                    worker.to_string(),
                    p["pane_id"].as_str().unwrap().to_string(),
                ));
            }
        }
        if found.len() == WORKERS.len() {
            return Ok((found, true));
        }
        api.call("workspace.close", json!({"workspace_id": wid}))?;
    }

    let created = api.call(
        "workspace.create",
        json!({"label": WORKSPACE, "cwd": scripts, "focus": false}),
    )?;
    let wid = created["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    let seed_tab = created["tab"]["tab_id"].as_str().unwrap().to_string();
    let mut panes = Vec::new();
    for (worker, script) in WORKERS {
        let r = api.call(
            "layout.apply",
            json!({
                "workspace_id": wid,
                "tab_label": worker,
                "focus": false,
                "root": {
                    "type": "pane",
                    "label": worker,
                    "cwd": scripts,
                    "command": [
                        scripts.join("loop.sh"),
                        scripts.join(script),
                        scripts.join(format!("{worker}.stress")),
                    ],
                    "env": {"LANG": "C.UTF-8", "LC_ALL": "C.UTF-8", "COLORTERM": "truecolor"}
                }
            }),
        )?;
        let pane = r["layout"]["root"]["pane_id"]
            .as_str()
            .ok_or("no pane id")?;
        panes.push((worker.to_string(), pane.to_string()));
    }
    api.call("tab.close", json!({"tab_id": seed_tab}))?;
    Ok((panes, false))
}

/// Close the spike workspace if it exists.
pub fn close_workspace(h: &Herdr) -> Result<()> {
    let mut api = Api::connect(&h.socket())?;
    let snap = api.call("session.snapshot", json!({}))?["snapshot"].clone();
    for w in snap["workspaces"].as_array().into_iter().flatten() {
        if w["label"] == WORKSPACE {
            api.call(
                "workspace.close",
                json!({"workspace_id": w["workspace_id"]}),
            )?;
        }
    }
    Ok(())
}

/// Echo target: input through the pane's controller, echo from its frames.
struct ControlEcho<'a>(&'a mut TermSession);

impl EchoTarget for ControlEcho<'_> {
    fn send(&mut self, bytes: &[u8]) -> Result<()> {
        self.0.input(bytes)
    }
    fn wait_for(&mut self, needle: u8, timeout: Duration) -> Option<Instant> {
        wait_frame(self.0, timeout, |b| b.contains(&needle))
    }
    fn drain(&mut self, settle: Duration) {
        drain(self.0, settle);
    }
}

/// Echo target: input through the JSON API (`pane.send_input`), echo from
/// the controller's frames.
struct ApiEcho<'a> {
    api: Api,
    pane: String,
    session: &'a mut TermSession,
}

impl EchoTarget for ApiEcho<'_> {
    fn send(&mut self, bytes: &[u8]) -> Result<()> {
        let params = if bytes == b"\x15" {
            json!({"pane_id": self.pane, "keys": ["ctrl+u"]})
        } else {
            json!({"pane_id": self.pane, "text": String::from_utf8_lossy(bytes)})
        };
        self.api.call("pane.send_input", params).map(|_| ())
    }
    fn wait_for(&mut self, needle: u8, timeout: Duration) -> Option<Instant> {
        wait_frame(self.session, timeout, |b| b.contains(&needle))
    }
    fn drain(&mut self, settle: Duration) {
        drain(self.session, settle);
    }
}

fn wait_frame(
    s: &mut TermSession,
    timeout: Duration,
    pred: impl Fn(&[u8]) -> bool,
) -> Option<Instant> {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.checked_duration_since(Instant::now())?;
        match s.events.recv_timeout(left) {
            Ok(StreamEvent::Frame(f)) if pred(&f.bytes) => return Some(f.at),
            Ok(_) => {}
            Err(_) => return None,
        }
    }
}

fn drain(s: &mut TermSession, settle: Duration) {
    while s.events.recv_timeout(settle).is_ok() {}
}

#[derive(Default)]
struct Tally {
    frames: u64,
    full: u64,
    bytes: u64,
    sizes: HashMap<(u16, u16), u64>,
    closed: Option<String>,
    counter: Option<u64>,
}

fn tally(s: &mut TermSession) -> Tally {
    let mut t = Tally::default();
    while let Ok(ev) = s.events.try_recv() {
        match ev {
            StreamEvent::Frame(f) => {
                t.frames += 1;
                t.full += f.full as u64;
                t.bytes += f.bytes.len() as u64;
                *t.sizes.entry((f.width, f.height)).or_default() += 1;
                t.counter = t.counter.max(measure::max_flood_counter(&f.bytes));
            }
            StreamEvent::Closed(r) => t.closed = Some(r),
        }
    }
    t
}

fn first_frame(s: &mut TermSession, timeout: Duration) -> String {
    match s.events.recv_timeout(timeout) {
        Ok(StreamEvent::Frame(f)) => format!(
            "{:.1} ms ({}x{}, full={}, {} B)",
            (f.at - s.started).as_secs_f64() * 1000.0,
            f.width,
            f.height,
            f.full,
            f.bytes.len()
        ),
        Ok(StreamEvent::Closed(r)) => format!("closed: {r}"),
        Err(_) => format!("no frame within {} ms", timeout.as_millis()),
    }
}

fn read_text(api: &mut Api, pane: &str, source: &str, lines: u32) -> Result<String> {
    let r = api.call(
        "pane.read",
        json!({"pane_id": pane, "source": source, "lines": lines, "format": "text"}),
    )?;
    Ok(r["read"]["text"].as_str().unwrap_or_default().to_string())
}

/// Child PIDs of the pane's `loop.sh` (the program it is currently running).
fn pane_program_pids(api: &mut Api, pane: &str) -> Result<(u64, Vec<String>)> {
    let r = api.call("pane.process_info", json!({"pane_id": pane}))?;
    let shell = r["process_info"]["shell_pid"]
        .as_u64()
        .ok_or("no shell_pid")?;
    let out = std::process::Command::new("pgrep")
        .args(["-P", &shell.to_string()])
        .output()
        .map_err(|e| e.to_string())?;
    let kids = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .map(str::to_string)
        .collect();
    Ok((shell, kids))
}

/// Same mechanism as the stub daemon: flip the flag, stop the current
/// program, and loop.sh starts flood.sh (or the normal script) next.
fn set_stress(ctx: &Ctx, worker: &str, on: bool) -> Result<()> {
    let flag = ctx.scripts.join(format!("{worker}.stress"));
    if on {
        std::fs::write(&flag, b"").map_err(|e| e.to_string())?;
    } else {
        let _ = std::fs::remove_file(&flag);
    }
    let mut api = ctx.api()?;
    let (_, kids) = pane_program_pids(&mut api, ctx.pane(worker))?;
    if !kids.is_empty() {
        let _ = std::process::Command::new("kill")
            .arg("-HUP")
            .args(&kids)
            .status();
        std::thread::sleep(Duration::from_millis(300));
        let _ = std::process::Command::new("kill")
            .arg("-KILL")
            .args(&kids)
            .stderr(std::process::Stdio::null())
            .status();
    }
    Ok(())
}

fn last_counter(text: &str) -> Option<u64> {
    measure::max_flood_counter(text.replace('\n', " ").as_bytes())
}

pub fn run(ctx: &Ctx, reused: bool, latency_samples: usize) -> Result<()> {
    let h = &ctx.h;
    let server_pid = h.server_pid();
    println!("## herdr arm\n");
    println!(
        "- herdr server pid {:?}, RSS {:.1} MiB; workspace `{WORKSPACE}` {}",
        server_pid,
        server_pid.and_then(measure::rss_mib).unwrap_or(f64::NAN),
        if reused {
            "**reused from an earlier run** (panes survived the spike process exiting)"
        } else {
            "created"
        }
    );
    for (w, p) in &ctx.panes {
        println!("- {w} -> pane `{p}`");
    }

    // 1. Attach one controller per pane, as quarkd would.
    println!("\n### Attach (one `terminal session control` per pane, {COLS}x{ROWS})\n");
    let mut sessions: Vec<TermSession> = Vec::new();
    for (w, p) in &ctx.panes {
        let mut s = TermSession::control(h, p, COLS, ROWS, true)?;
        println!(
            "- {w}: first frame {}",
            first_frame(&mut s, Duration::from_secs(3))
        );
        sessions.push(s);
    }

    // 2. Steady-state stream volume.
    std::thread::sleep(Duration::from_secs(1));
    for s in &mut sessions {
        tally(s);
    }
    let secs = 10.0;
    let cpu = CpuWindow::start(&[("herdr server", server_pid)]);
    std::thread::sleep(Duration::from_secs_f64(secs));
    println!("\n### Steady state ({secs} s, normal scripts)\n");
    println!("| worker | frames/s | KiB/s | full frames |\n|---|---|---|---|");
    for ((w, _), s) in ctx.panes.iter().zip(&mut sessions) {
        let t = tally(s);
        println!(
            "| {w} | {:.1} | {:.1} | {} |",
            t.frames as f64 / secs,
            t.bytes as f64 / 1024.0 / secs,
            t.full
        );
    }
    for (n, pct) in cpu.finish() {
        println!("\n{n} CPU: {pct:.1}% of one core");
    }

    // 3. Input-to-echo latency on the bash pane.
    let w4 = ctx.panes.iter().position(|(w, _)| w == "w-4").unwrap();
    println!("\n### Input-to-echo latency (w-4 bash, idle fleet)\n");
    let st = measure::echo_latency(
        &mut ControlEcho(&mut sessions[w4]),
        latency_samples,
        Duration::from_millis(20),
    );
    println!(
        "- controller stdin (`terminal.input`) -> frame: {}",
        st.summary()
    );
    let st = measure::echo_latency(
        &mut ApiEcho {
            api: ctx.api()?,
            pane: ctx.pane("w-4").into(),
            session: &mut sessions[w4],
        },
        latency_samples / 2,
        Duration::from_millis(20),
    );
    println!("- JSON API `pane.send_input` -> frame: {}", st.summary());

    // 4. Flood w-1 while measuring echo latency on w-4.
    println!("\n### Flood (w-1 runs flood.sh for 10 s; echo latency measured on w-4 meanwhile)\n");
    let w1 = ctx.panes.iter().position(|(w, _)| w == "w-1").unwrap();
    set_stress(ctx, "w-1", true)?;
    std::thread::sleep(Duration::from_millis(500));
    let mut api = ctx.api()?;
    let c0 = last_counter(&read_text(&mut api, ctx.pane("w-1"), "visible", 200)?);
    tally(&mut sessions[w1]);
    let t0 = Instant::now();
    let cpu = CpuWindow::start(&[("herdr server", server_pid)]);
    let mut lat = Stats::default();
    while t0.elapsed() < Duration::from_secs(10) {
        let part = measure::echo_latency(
            &mut ControlEcho(&mut sessions[w4]),
            20,
            Duration::from_millis(20),
        );
        lat.samples.extend(part.samples);
        lat.timeouts += part.timeouts;
    }
    let secs = t0.elapsed().as_secs_f64();
    let cpu = cpu.finish();
    let c1 = last_counter(&read_text(&mut api, ctx.pane("w-1"), "visible", 200)?);
    let t = tally(&mut sessions[w1]);
    let produced = match (c0, c1) {
        (Some(a), Some(b)) => format!("{:.0} lines/s", (b - a) as f64 / secs),
        _ => "unknown".into(),
    };
    println!("- producer (flood.sh) rate, from the counter on screen: {produced}");
    println!(
        "- w-1 stream to quarkd: {:.1} frames/s, {:.2} MiB/s ({:.0} B/frame)",
        t.frames as f64 / secs,
        measure::mib(t.bytes) / secs,
        t.bytes as f64 / t.frames.max(1) as f64
    );
    println!("- w-4 echo latency during flood: {}", lat.summary());
    for (n, pct) in cpu {
        println!("- {n} CPU during flood: {pct:.1}% of one core");
    }
    set_stress(ctx, "w-1", false)?;

    // 5. Resize through the controller.
    println!("\n### Resize\n");
    let w2 = ctx.panes.iter().position(|(w, _)| w == "w-2").unwrap();
    drain(&mut sessions[w2], Duration::from_millis(50));
    let sent = Instant::now();
    sessions[w2].resize(100, 30)?;
    let at = wait_frame_sized(&mut sessions[w2], 100, 30, Duration::from_secs(3));
    println!(
        "- w-2 (top) controller resize 120x36 -> 100x30: first 100x30 frame after {}",
        fmt_ms(at.map(|a| a - sent))
    );
    sessions[w4].resize(90, 25)?;
    std::thread::sleep(Duration::from_millis(200));
    sessions[w4].input(b"stty size\r")?;
    std::thread::sleep(Duration::from_millis(500));
    let txt = read_text(&mut api, ctx.pane("w-4"), "visible", 200)?;
    println!(
        "- w-4 resized to 90x25 via controller; `stty size` in the pane prints `25 90`: {}",
        txt.contains("25 90")
    );
    let pane = api.call("pane.get", json!({"pane_id": ctx.pane("w-4")}))?;
    println!(
        "- `pane.get` viewport_rows after resize: {}",
        pane["pane"]["scroll"]["viewport_rows"]
    );
    sessions[w4].resize(COLS, ROWS)?;
    sessions[w2].resize(COLS, ROWS)?;
    sessions[w4].input(b"\x0c")?; // clear screen

    // 6. Two consumers of one pane at different sizes.
    println!(
        "\n### Two viewers, different sizes (w-3: controller {COLS}x{ROWS}, observer 80x20)\n"
    );
    let w3 = ctx.panes.iter().position(|(w, _)| w == "w-3").unwrap();
    let mut obs = TermSession::observe(h, ctx.pane("w-3"), 80, 20)?;
    tally(&mut sessions[w3]);
    std::thread::sleep(Duration::from_secs(3));
    let tc = tally(&mut sessions[w3]);
    let to = tally(&mut obs);
    println!(
        "- controller frame sizes: {:?}; observer frame sizes: {:?}",
        tc.sizes, to.sizes
    );
    println!(
        "- observer received {} frames in 3 s (controller {}); PTY size stays the controller's",
        to.frames, tc.frames
    );
    drop(obs);
    let mut second = TermSession::control(h, ctx.pane("w-3"), 100, 30, false)?;
    let r = first_frame(&mut second, Duration::from_secs(2));
    println!("- second controller without --takeover: {r}");
    drop(second);
    let mut second = TermSession::control(h, ctx.pane("w-3"), 100, 30, true)?;
    let r = first_frame(&mut second, Duration::from_secs(2));
    std::thread::sleep(Duration::from_millis(300));
    let first = tally(&mut sessions[w3]);
    println!(
        "- second controller with --takeover: first frame {r}; original controller closed: {:?}",
        first.closed
    );
    drop(second);
    sessions[w3] = TermSession::control(h, ctx.pane("w-3"), COLS, ROWS, true)?;

    // 7. Event subscription: output match on w-4.
    println!("\n### Events\n");
    let ack = ctx.api()?.subscribe(json!([
        {"type": "pane.output_matched", "pane_id": ctx.pane("w-4"), "source": "recent",
         "match": {"type": "substring", "value": "spike-event-marker"}},
        {"type": "pane.exited"}, {"type": "pane.agent_status_changed", "pane_id": ctx.pane("w-4")}
    ]));
    match ack {
        Ok((_, mut sub)) => {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                while let Some(line) = sub.next_line() {
                    if line.contains("pane.output_matched") {
                        let _ = tx.send(line);
                        return;
                    }
                }
            });
            let sent = Instant::now();
            sessions[w4].input(b"echo spike-event-marker\r")?;
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(line) => println!(
                    "- `pane.output_matched` arrived {} after typing: `{}`",
                    fmt_ms(Some(sent.elapsed())),
                    truncate(&line, 160)
                ),
                Err(_) => println!("- no `pane.output_matched` event within 5 s"),
            }
        }
        Err(e) => println!("- events.subscribe failed: {e}"),
    }

    // 8. Client crash and reattach.
    println!("\n### quarkd crash and reattach\n");
    sessions[w4].input(b"echo before-crash-marker\r")?;
    std::thread::sleep(Duration::from_millis(300));
    let before: Vec<_> = ctx
        .panes
        .iter()
        .map(|(_, p)| pane_program_pids(&mut api, p).map(|x| x.0).unwrap_or(0))
        .collect();
    for s in sessions.drain(..) {
        s.kill();
    }
    drop(api);
    std::thread::sleep(Duration::from_secs(3));
    let mut api = ctx.api()?;
    let after: Vec<_> = ctx
        .panes
        .iter()
        .map(|(_, p)| pane_program_pids(&mut api, p).map(|x| x.0).unwrap_or(0))
        .collect();
    println!("- pane shell pids before {before:?}, after {after:?} (same = processes survived)");
    for (w, p) in &ctx.panes {
        let mut s = TermSession::control(h, p, COLS, ROWS, true)?;
        println!(
            "- {w}: re-attach first frame {}",
            first_frame(&mut s, Duration::from_secs(2))
        );
        sessions.push(s);
    }
    let hist = read_text(&mut api, ctx.pane("w-4"), "recent", 200)?;
    println!(
        "- `pane.read` recent history of w-4 still holds the pre-crash command: {}",
        hist.contains("before-crash-marker")
    );
    let ansi = api.call(
        "pane.read",
        json!({"pane_id": ctx.pane("w-3"), "source": "visible", "format": "ansi", "strip_ansi": false}),
    )?;
    println!(
        "- `pane.read` visible screen as ANSI (snapshot for a fresh UI): {} bytes for w-3",
        ansi["read"]["text"].as_str().map(str::len).unwrap_or(0)
    );
    let t0 = Instant::now();
    let r = read_text(&mut api, ctx.pane("w-1"), "recent", 5000)?;
    println!(
        "- `pane.read` recent 5000 lines of w-1: {} lines, {} KiB in {}",
        r.lines().count(),
        r.len() / 1024,
        fmt_ms(Some(t0.elapsed()))
    );
    Ok(())
}

fn wait_frame_sized(s: &mut TermSession, w: u16, h: u16, timeout: Duration) -> Option<Instant> {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.checked_duration_since(Instant::now())?;
        match s.events.recv_timeout(left) {
            Ok(StreamEvent::Frame(f)) if f.width == w && f.height == h => return Some(f.at),
            Ok(_) => {}
            Err(_) => return None,
        }
    }
}

pub fn fmt_ms(d: Option<Duration>) -> String {
    match d {
        Some(d) => format!("{:.1} ms", d.as_secs_f64() * 1000.0),
        None => "timeout".into(),
    }
}

fn truncate(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..s.floor_char_boundary(n)])
    }
}

/// Stop and restart the herdr server; report what survives.
pub fn server_restart(ctx: &Ctx) -> Result<()> {
    let h = &ctx.h;
    println!("## herdr server restart\n");
    let mut api = ctx.api()?;
    let before: Vec<_> = ctx
        .panes
        .iter()
        .map(|(_, p)| pane_program_pids(&mut api, p).map(|x| x.0).unwrap_or(0))
        .collect();
    drop(api);
    h.stop_server()?;
    let alive = before
        .iter()
        .filter(|pid| Path::new(&format!("/proc/{pid}")).exists())
        .count();
    println!(
        "- pane processes alive after `herdr server stop`: {alive}/{}",
        before.len()
    );
    let t0 = Instant::now();
    h.ensure_server()?;
    println!("- server back after {}", fmt_ms(Some(t0.elapsed())));
    std::thread::sleep(Duration::from_secs(2));
    let mut api = ctx.api()?;
    let snap = api.call("session.snapshot", json!({}))?["snapshot"].clone();
    let panes: Vec<&Value> = snap["panes"].as_array().into_iter().flatten().collect();
    println!(
        "- restored layout: {} workspaces, {} panes; labels: {:?}",
        snap["workspaces"].as_array().map(Vec::len).unwrap_or(0),
        panes.len(),
        panes
            .iter()
            .map(|p| p["label"].as_str().unwrap_or(""))
            .collect::<Vec<_>>()
    );
    for p in panes {
        let id = p["pane_id"].as_str().unwrap_or_default();
        let info = api.call("pane.process_info", json!({"pane_id": id}));
        let fg: Vec<String> = info
            .as_ref()
            .ok()
            .and_then(|i| {
                i["process_info"]["foreground_processes"]
                    .as_array()
                    .cloned()
            })
            .unwrap_or_default()
            .iter()
            .map(|p| {
                p["cmdline"]
                    .as_str()
                    .or(p["name"].as_str())
                    .unwrap_or("?")
                    .to_string()
            })
            .collect();
        println!(
            "- {} (`{id}`): foreground now {:?}",
            p["label"].as_str().unwrap_or("?"),
            fg
        );
    }
    Ok(())
}
