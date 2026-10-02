//! Measurement helpers shared by the herdr and tmux arms.

use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Find the first process whose (exe, argv) satisfy `pred` (Linux /proc).
pub fn find_pid(pred: impl Fn(&PathBuf, &[String]) -> bool) -> Option<u32> {
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(exe) = std::fs::read_link(entry.path().join("exe")) else {
            continue;
        };
        let Ok(raw) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let args: Vec<String> = raw
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        if pred(&exe, &args) {
            return Some(pid);
        }
    }
    None
}

/// utime + stime of a process, in seconds.
pub fn cpu_seconds(pid: u32) -> Option<f64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 2..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    let ticks: f64 = f.get(11)?.parse::<f64>().ok()? + f.get(12)?.parse::<f64>().ok()?;
    Some(ticks / 100.0)
}

/// Resident set size in MiB.
pub fn rss_mib(pid: u32) -> Option<f64> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    let pages: f64 = s.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * 4096.0 / (1024.0 * 1024.0))
}

/// CPU usage of several processes over a window, as % of one core.
pub struct CpuWindow {
    start: Instant,
    pids: Vec<(String, u32, f64)>,
}

impl CpuWindow {
    pub fn start(pids: &[(&str, Option<u32>)]) -> Self {
        Self {
            start: Instant::now(),
            pids: pids
                .iter()
                .filter_map(|(n, p)| {
                    let p = (*p)?;
                    Some((n.to_string(), p, cpu_seconds(p)?))
                })
                .collect(),
        }
    }

    pub fn finish(&self) -> Vec<(String, f64)> {
        let secs = self.start.elapsed().as_secs_f64();
        self.pids
            .iter()
            .filter_map(|(n, p, c0)| Some((n.clone(), (cpu_seconds(*p)? - c0) / secs * 100.0)))
            .collect()
    }
}

#[derive(Default, Clone)]
pub struct Stats {
    pub samples: Vec<f64>,
    pub timeouts: usize,
}

impl Stats {
    pub fn push(&mut self, d: Option<Duration>) {
        match d {
            Some(d) => self.samples.push(d.as_secs_f64() * 1000.0),
            None => self.timeouts += 1,
        }
    }

    fn pct(&self, p: f64) -> f64 {
        let mut s = self.samples.clone();
        if s.is_empty() {
            return f64::NAN;
        }
        s.sort_by(|a, b| a.partial_cmp(b).unwrap());
        s[((s.len() - 1) as f64 * p).round() as usize]
    }

    /// `n=200 p50 0.42 ms, p95 0.80, p99 1.20, max 3.1`
    pub fn summary(&self) -> String {
        let mut out = format!(
            "n={} p50 {:.2} ms, p95 {:.2}, p99 {:.2}, max {:.2}",
            self.samples.len(),
            self.pct(0.5),
            self.pct(0.95),
            self.pct(0.99),
            self.pct(1.0)
        );
        if self.timeouts > 0 {
            out.push_str(&format!(", {} timeouts", self.timeouts));
        }
        out
    }
}

/// Something we can type into and watch the echo of.
pub trait EchoTarget {
    fn send(&mut self, bytes: &[u8]) -> Result<(), String>;
    /// Wait until output containing `needle` arrives; returns its arrival time.
    fn wait_for(&mut self, needle: u8, timeout: Duration) -> Option<Instant>;
    /// Discard whatever output is queued (waits `settle` for stragglers).
    fn drain(&mut self, settle: Duration);
}

/// Type single characters into an interactive bash and time the echo.
/// The line is cleared with Ctrl-U every 40 characters.
pub fn echo_latency(t: &mut dyn EchoTarget, samples: usize, gap: Duration) -> Stats {
    let mut stats = Stats::default();
    t.drain(Duration::from_millis(200));
    for i in 0..samples {
        if i % 40 == 0 && i > 0 {
            let _ = t.send(b"\x15");
            t.drain(Duration::from_millis(100));
        }
        let c = b'a' + (i % 26) as u8;
        let sent = Instant::now();
        if t.send(&[c]).is_err() {
            stats.push(None);
            continue;
        }
        stats.push(t.wait_for(c, Duration::from_secs(2)).map(|at| at - sent));
        std::thread::sleep(gap);
    }
    let _ = t.send(b"\x15");
    t.drain(Duration::from_millis(100));
    stats
}

/// Highest 8-digit flood counter (`flood.sh` prints `%08d`) in a byte slice.
pub fn max_flood_counter(bytes: &[u8]) -> Option<u64> {
    let mut best = None;
    let mut run = 0usize;
    for (i, b) in bytes.iter().enumerate() {
        if b.is_ascii_digit() {
            run += 1;
            continue;
        }
        if run == 8 {
            let v: u64 = std::str::from_utf8(&bytes[i - 8..i]).ok()?.parse().ok()?;
            // flood.sh puts a space after the counter; ignore other numbers.
            if *b == 0x1b || *b == b' ' {
                best = best.max(Some(v));
            }
        }
        run = 0;
    }
    best
}

pub fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}
