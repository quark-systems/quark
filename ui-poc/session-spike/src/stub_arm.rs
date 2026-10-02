//! The tmux arm: the UI POC stub daemon (tmux control mode) driven over its
//! HTTP + WebSocket contract (`ui-poc/CONTRACT.md`).

use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{Value, json};

use crate::herdr_arm::fmt_ms;
use crate::measure::{self, CpuWindow, EchoTarget, Stats};

type Result<T> = std::result::Result<T, String>;

pub struct Output {
    pub at: Instant,
    pub seq: u64,
    pub ts_ms: u64,
    pub worker: String,
    pub bytes: Vec<u8>,
}

pub struct Stub {
    base: String,
    port: u16,
    agent: ureq::Agent,
}

impl Stub {
    pub fn new(port: u16) -> Self {
        Self {
            base: format!("http://127.0.0.1:{port}"),
            port,
            agent: ureq::AgentBuilder::new().build(),
        }
    }

    fn post(&self, path: &str, body: Value) -> Result<Value> {
        let r = self
            .agent
            .post(&format!("{}{path}", self.base))
            .send_json(body)
            .map_err(|e| format!("POST {path}: {e}"))?;
        Ok(r.into_json::<Value>().unwrap_or(Value::Null))
    }

    pub fn get(&self, path: &str) -> Result<Value> {
        self.agent
            .get(&format!("{}{path}", self.base))
            .call()
            .map_err(|e| format!("GET {path}: {e}"))?
            .into_json()
            .map_err(|e| e.to_string())
    }

    /// Open the event stream; `cursor = None` means live only.
    /// Returns a receiver of `worker.output` events and a counter of all events.
    fn events(&self, cursor: Option<u64>) -> Result<Receiver<Output>> {
        let c = cursor.unwrap_or(u64::MAX / 2);
        let url = format!("ws://127.0.0.1:{}/v1/events?cursor={c}", self.port);
        let (mut ws, _) = tungstenite::connect(url).map_err(|e| format!("ws: {e}"))?;
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            while let Ok(msg) = ws.read() {
                let at = Instant::now();
                let tungstenite::Message::Text(t) = msg else {
                    continue;
                };
                let Ok(v) = serde_json::from_str::<Value>(&t) else {
                    continue;
                };
                if v["type"] != "worker.output" {
                    continue;
                }
                let out = Output {
                    at,
                    seq: v["seq"].as_u64().unwrap_or(0),
                    ts_ms: ts_ms_of_day(v["ts"].as_str().unwrap_or("")),
                    worker: v["payload"]["worker_id"].as_str().unwrap_or("").to_string(),
                    bytes: B64
                        .decode(v["payload"]["data_b64"].as_str().unwrap_or(""))
                        .unwrap_or_default(),
                };
                if tx.send(out).is_err() {
                    return;
                }
            }
        });
        Ok(rx)
    }
}

struct StubEcho<'a> {
    stub: &'a Stub,
    rx: &'a Receiver<Output>,
    worker: &'a str,
}

impl EchoTarget for StubEcho<'_> {
    fn send(&mut self, bytes: &[u8]) -> Result<()> {
        self.stub
            .post(
                &format!("/v1/workers/{}/input", self.worker),
                json!({"data_b64": B64.encode(bytes)}),
            )
            .map(|_| ())
    }
    fn wait_for(&mut self, needle: u8, timeout: Duration) -> Option<Instant> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.checked_duration_since(Instant::now())?;
            let o = self.rx.recv_timeout(left).ok()?;
            if o.worker == self.worker && o.bytes.contains(&needle) {
                return Some(o.at);
            }
        }
    }
    /// Discard queued output until this worker has been quiet for `settle`
    /// (other workers' output, e.g. a flood, does not count as activity).
    fn drain(&mut self, settle: Duration) {
        let hard = Instant::now() + Duration::from_secs(2);
        let mut quiet_until = Instant::now() + settle;
        while Instant::now() < quiet_until && Instant::now() < hard {
            if let Ok(o) = self.rx.recv_timeout(Duration::from_millis(5))
                && o.worker == self.worker
            {
                quiet_until = Instant::now() + settle;
            }
        }
    }
}

#[derive(Default)]
struct Tally {
    events: u64,
    bytes: u64,
    counter: Option<u64>,
}

fn tally(rx: &Receiver<Output>, worker: Option<&str>) -> std::collections::BTreeMap<String, Tally> {
    let mut m: std::collections::BTreeMap<String, Tally> = Default::default();
    while let Ok(o) = rx.try_recv() {
        if worker.is_some_and(|w| w != o.worker) {
            continue;
        }
        let t = m.entry(o.worker.clone()).or_default();
        t.events += 1;
        t.bytes += o.bytes.len() as u64;
        t.counter = t.counter.max(measure::max_flood_counter(&o.bytes));
    }
    m
}

pub fn run(port: u16, latency_samples: usize) -> Result<()> {
    let stub = Stub::new(port);
    let workers = stub.get("/v1/workers")?;
    println!("## tmux arm (stub daemon on port {port}, tmux control mode)\n");
    println!(
        "- workers: {}",
        workers.as_array().map(Vec::len).unwrap_or(0)
    );
    let socket = format!("quark-poc-{port}");
    let tmux_pid = std::process::Command::new("tmux")
        .args(["-L", &socket, "display", "-p", "#{pid}"])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<u32>()
                .ok()
        });
    let stub_pid =
        measure::find_pid(|exe, _| exe.file_name().is_some_and(|n| n == "quark-ui-stub-daemon"));
    println!(
        "- tmux server pid {tmux_pid:?} RSS {:.1} MiB; stub daemon pid {stub_pid:?} RSS {:.1} MiB",
        tmux_pid.and_then(measure::rss_mib).unwrap_or(f64::NAN),
        stub_pid.and_then(measure::rss_mib).unwrap_or(f64::NAN)
    );

    let t0 = Instant::now();
    let rx = stub.events(None)?;
    println!(
        "- WebSocket connect (live only): {}",
        fmt_ms(Some(t0.elapsed()))
    );

    // Steady state.
    std::thread::sleep(Duration::from_secs(1));
    tally(&rx, None);
    let secs = 10.0;
    let cpu = CpuWindow::start(&[("tmux server", tmux_pid), ("stub daemon", stub_pid)]);
    std::thread::sleep(Duration::from_secs_f64(secs));
    println!("\n### Steady state ({secs} s, normal scripts)\n");
    println!("| worker | events/s | KiB/s |\n|---|---|---|");
    for (w, t) in tally(&rx, None) {
        println!(
            "| {w} | {:.1} | {:.1} |",
            t.events as f64 / secs,
            t.bytes as f64 / 1024.0 / secs
        );
    }
    for (n, pct) in cpu.finish() {
        println!("\n{n} CPU: {pct:.1}% of one core");
    }

    println!("\n### Input-to-echo latency (w-4 bash, idle fleet)\n");
    let st = measure::echo_latency(
        &mut StubEcho {
            stub: &stub,
            rx: &rx,
            worker: "w-4",
        },
        latency_samples,
        Duration::from_millis(20),
    );
    println!(
        "- HTTP `POST /input` (tmux `send-keys -H`) -> WS `worker.output`: {}",
        st.summary()
    );

    println!("\n### Flood (w-1 stress for 10 s; echo latency measured on w-4 meanwhile)\n");
    stub.post("/v1/workers/w-1/stress", json!({"on": true}))?;
    std::thread::sleep(Duration::from_millis(800));
    // Measure on a dedicated stream so latency sampling below does not eat w-1 output.
    let flood_rx = stub.events(None)?;
    let t0 = Instant::now();
    let cpu = CpuWindow::start(&[("tmux server", tmux_pid), ("stub daemon", stub_pid)]);
    let first = flood_first_counter(&flood_rx);
    let mut lat = Stats::default();
    let mut w1 = Tally::default();
    while t0.elapsed() < Duration::from_secs(10) {
        let part = measure::echo_latency(
            &mut StubEcho {
                stub: &stub,
                rx: &rx,
                worker: "w-4",
            },
            20,
            Duration::from_millis(20),
        );
        lat.samples.extend(part.samples);
        lat.timeouts += part.timeouts;
        if let Some(t) = tally(&flood_rx, Some("w-1")).remove("w-1") {
            w1.events += t.events;
            w1.bytes += t.bytes;
            w1.counter = w1.counter.max(t.counter);
        }
    }
    let secs = t0.elapsed().as_secs_f64();
    let cpu = cpu.finish();
    if let Some(t) = tally(&flood_rx, Some("w-1")).remove("w-1") {
        w1.events += t.events;
        w1.bytes += t.bytes;
        w1.counter = w1.counter.max(t.counter);
    }
    let produced = match (first, w1.counter) {
        (Some(a), Some(b)) => format!("{:.0} lines/s", (b - a) as f64 / secs),
        _ => "unknown".into(),
    };
    println!("- producer (flood.sh) rate, from counters in the delivered bytes: {produced}");
    println!(
        "- w-1 stream to client: {:.1} events/s, {:.2} MiB/s",
        w1.events as f64 / secs,
        measure::mib(w1.bytes) / secs
    );
    println!("- w-4 echo latency during flood: {}", lat.summary());
    for (n, pct) in cpu {
        println!("- {n} CPU during flood: {pct:.1}% of one core");
    }
    stub.post("/v1/workers/w-1/stress", json!({"on": false}))?;
    drop(flood_rx);

    println!("\n### Resize\n");
    std::thread::sleep(Duration::from_millis(500));
    while rx.try_recv().is_ok() {}
    let sent = Instant::now();
    stub.post("/v1/workers/w-2/resize", json!({"cols": 100, "rows": 30}))?;
    let at = loop {
        match rx.recv_timeout(Duration::from_secs(3)) {
            Ok(o) if o.worker == "w-2" && o.at > sent => break Some(o.at),
            Ok(_) => {}
            Err(_) => break None,
        }
    };
    println!(
        "- w-2 (top) resize -> first w-2 output after {}",
        fmt_ms(at.map(|a| a - sent))
    );
    stub.post("/v1/workers/w-2/resize", json!({"cols": 120, "rows": 36}))?;

    println!("\n### Client reconnect with replay\n");
    std::thread::sleep(Duration::from_millis(300));
    let mut last_seq = 0;
    while let Ok(o) = rx.try_recv() {
        last_seq = last_seq.max(o.seq);
    }
    drop(rx);
    std::thread::sleep(Duration::from_secs(3));
    let (n, bytes, took) = replay(&stub, Some(last_seq))?;
    println!(
        "- reconnect after 3 s with cursor: {n} missed `worker.output` events ({} KiB) replayed in {}",
        bytes / 1024,
        fmt_ms(took)
    );
    let (n, bytes, took) = replay(&stub, Some(0))?;
    println!(
        "- fresh client, cursor=0 (full retained replay): {n} events, {:.1} MiB of `worker.output` in {}",
        measure::mib(bytes as u64),
        fmt_ms(took)
    );
    Ok(())
}

/// Reconnect with `cursor`; count `worker.output` events stamped before the
/// reconnect, and time until the first event stamped after it (replay done).
fn replay(stub: &Stub, cursor: Option<u64>) -> Result<(u64, usize, Option<Duration>)> {
    let t0 = Instant::now();
    let now_ms = utc_ms_of_day_now();
    let rx = stub.events(cursor)?;
    let (mut n, mut bytes) = (0u64, 0usize);
    loop {
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(o) if o.ts_ms < now_ms => {
                n += 1;
                bytes += o.bytes.len();
            }
            Ok(o) => return Ok((n, bytes, Some(o.at - t0))),
            Err(_) => return Ok((n, bytes, None)),
        }
    }
}

fn utc_ms_of_day_now() -> u64 {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    ms % 86_400_000
}

/// `2026-10-01T23:11:19.926Z` -> milliseconds since UTC midnight.
fn ts_ms_of_day(ts: &str) -> u64 {
    let t = ts.split('T').nth(1).unwrap_or("").trim_end_matches('Z');
    let mut parts = t.split(':');
    let h: u64 = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    let m: u64 = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    let s: f64 = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0.0);
    (h * 3600 + m * 60) * 1000 + (s * 1000.0) as u64
}

fn flood_first_counter(rx: &Receiver<Output>) -> Option<u64> {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if let Ok(o) = rx.recv_timeout(Duration::from_millis(100))
            && o.worker == "w-1"
            && let Some(c) = measure::max_flood_counter(&o.bytes)
        {
            return Some(c);
        }
    }
    None
}
