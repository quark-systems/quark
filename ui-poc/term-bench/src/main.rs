//! quark-term-bench: compare libghostty-vt and alacritty_terminal as the
//! terminal core for Quark's desktop app.
//!
//! Subcommands:
//!   all        run every benchmark + checks, print Markdown (default)
//!   throughput ingest MB/s
//!   frame      per-frame render snapshot cost
//!   resize     resize/reflow cost with full scrollback
//!   memory     RSS delta with 10k scrollback (each sample in a fresh process)
//!   check      correctness spot checks + protocol probes
//!   mem-child  internal: one memory sample (used by `memory`)
//!   ingest-one <core> <workload> <cols> <rows> [scrollback]
//!              ingest a single workload into a single core (for profiling)
//!
//! Flags: --runs N (default 5), --scale F (data size multiplier, default 1.0),
//!        --frames N (default 1000)

mod cores;
mod probes;
mod stats;
mod workload;

use std::time::Instant;

use cores::{Core, alacritty::Alacritty, ghostty::Ghostty};
use stats::{Summary, median};
use workload::Kind;

const SCROLLBACK: usize = 10_000;
const GEOMS: [(u16, u16); 2] = [(80, 24), (200, 50)];
/// PTY read size: feed in chunks like a real read loop would.
const CHUNK: usize = 64 * 1024;

struct Opts {
    runs: usize,
    scale: f64,
    frames: usize,
}

impl Opts {
    fn bytes(&self, kind: Kind) -> usize {
        let mb = match kind {
            Kind::BuildLog | Kind::Ascii => 50.0,
            Kind::Tui | Kind::Unicode => 20.0,
        };
        (mb * self.scale * 1024.0 * 1024.0) as usize
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut opts = Opts {
        runs: 5,
        scale: 1.0,
        frames: 1000,
    };
    let mut positional = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--runs" => opts.runs = it.next().and_then(|v| v.parse().ok()).expect("--runs N"),
            "--scale" => opts.scale = it.next().and_then(|v| v.parse().ok()).expect("--scale F"),
            "--frames" => opts.frames = it.next().and_then(|v| v.parse().ok()).expect("--frames N"),
            _ => positional.push(a.clone()),
        }
    }
    let cmd = positional.first().map(String::as_str).unwrap_or("all");
    match cmd {
        "all" => {
            print_env(&opts);
            throughput(&opts);
            frame(&opts);
            resize(&opts);
            memory();
            probes::check_all();
            println!(
                "loadavg at end: {}",
                std::fs::read_to_string("/proc/loadavg")
                    .unwrap_or_default()
                    .trim()
            );
        }
        "throughput" => throughput(&opts),
        "frame" => frame(&opts),
        "resize" => resize(&opts),
        "memory" => memory(),
        "check" => probes::check_all(),
        "mem-child" => mem_child(&positional[1..]),
        "ingest-one" => ingest_one(&positional[1..], &opts),
        other => {
            eprintln!("unknown command {other}; see src/main.rs header");
            std::process::exit(2);
        }
    }
}

fn print_env(opts: &Opts) {
    println!("# quark-term-bench");
    println!();
    println!(
        "runs={} scale={} frames={} scrollback={} chunk={}KiB",
        opts.runs,
        opts.scale,
        opts.frames,
        SCROLLBACK,
        CHUNK / 1024
    );
    println!();
    println!(
        "loadavg at start: {}",
        std::fs::read_to_string("/proc/loadavg")
            .unwrap_or_default()
            .trim()
    );
    println!();
}

fn feed_all<C: Core>(core: &mut C, data: &[u8]) {
    for chunk in data.chunks(CHUNK) {
        core.feed(chunk);
    }
}

// ---------------------------------------------------------------------------
// Ingest throughput
// ---------------------------------------------------------------------------

fn throughput(opts: &Opts) {
    println!(
        "## Ingest throughput (median of {} interleaved runs; higher is better)",
        opts.runs
    );
    println!();
    println!(
        "`wall` = bytes / wall-clock time. `cpu` = bytes / on-CPU time of the feeding thread (from /proc/thread-self/schedstat), which is less sensitive to other load on the host. `sys` = kernel share of that CPU time (page faults, madvise)."
    );
    println!();
    println!(
        "| workload | size | geometry | ghostty wall MB/s | ghostty cpu MB/s | ghostty sys | alacritty wall MB/s | alacritty cpu MB/s | alacritty sys | cpu ratio (g/a) |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|");
    for kind in Kind::ALL {
        for (cols, rows) in GEOMS {
            let data = kind.generate(opts.bytes(kind), cols, rows);
            let mut g = Vec::new();
            let mut a = Vec::new();
            // Interleave so both cores see the same background load.
            for _ in 0..opts.runs {
                g.push(ingest_run::<Ghostty>(&data, cols, rows));
                a.push(ingest_run::<Alacritty>(&data, cols, rows));
            }
            let (g, a) = (Ingest::median(&g), Ingest::median(&a));
            println!(
                "| {} | {:.1} MB | {cols}x{rows} | {:.0} | {:.0} | {:.0}% | {:.0} | {:.0} | {:.0}% | {:.2}x |",
                kind.name(),
                data.len() as f64 / 1e6,
                g.wall,
                g.cpu,
                g.sys * 100.0,
                a.wall,
                a.cpu,
                a.sys * 100.0,
                g.cpu / a.cpu
            );
        }
    }
    println!();
}

#[derive(Clone, Copy)]
struct Ingest {
    /// MB/s by wall clock.
    wall: f64,
    /// MB/s by on-CPU time.
    cpu: f64,
    /// Kernel share of CPU time.
    sys: f64,
}

impl Ingest {
    fn median(runs: &[Ingest]) -> Ingest {
        let pick = |f: fn(&Ingest) -> f64| median(&mut runs.iter().map(f).collect::<Vec<_>>());
        Ingest {
            wall: pick(|r| r.wall),
            cpu: pick(|r| r.cpu),
            sys: pick(|r| r.sys),
        }
    }
}

fn ingest_run<C: Core>(data: &[u8], cols: u16, rows: u16) -> Ingest {
    let mut core = C::new(cols, rows, SCROLLBACK);
    let ((u0, s0), c0) = (cpu_ticks(), oncpu_secs());
    let t = Instant::now();
    feed_all(&mut core, data);
    let wall = t.elapsed().as_secs_f64();
    let ((u1, s1), c1) = (cpu_ticks(), oncpu_secs());
    std::hint::black_box(core.cursor());
    let mb = data.len() as f64 / 1e6;
    let (u, s) = (u1 - u0, s1 - s0);
    Ingest {
        wall: mb / wall,
        cpu: mb / (c1 - c0),
        sys: if u + s > 0.0 { s / (u + s) } else { 0.0 },
    }
}

/// (user, system) CPU seconds of this thread, from /proc/thread-self/stat
/// (clock ticks; assumes the usual USER_HZ of 100).
fn cpu_ticks() -> (f64, f64) {
    let stat = std::fs::read_to_string("/proc/thread-self/stat").unwrap_or_default();
    // Fields after the parenthesised comm; utime/stime are fields 14/15.
    let after = stat.rsplit_once(')').map_or("", |(_, r)| r);
    let f: Vec<&str> = after.split_whitespace().collect();
    let tick = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0) / 100.0;
    (tick(11), tick(12))
}

/// Nanosecond-resolution on-CPU time of this thread (user + system).
fn oncpu_secs() -> f64 {
    std::fs::read_to_string("/proc/thread-self/schedstat")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
        .unwrap_or(f64::NAN)
        / 1e9
}

fn ingest_one(args: &[String], opts: &Opts) {
    let [core, kind, cols, rows, rest @ ..] = args else {
        panic!("usage: ingest-one <core> <workload> <cols> <rows> [scrollback]");
    };
    let kind = Kind::ALL
        .into_iter()
        .find(|k| k.name() == kind)
        .unwrap_or_else(|| panic!("unknown workload {kind}"));
    let (cols, rows): (u16, u16) = (cols.parse().unwrap(), rows.parse().unwrap());
    let scrollback = rest.first().map_or(SCROLLBACK, |v| v.parse().unwrap());
    let data = kind.generate(opts.bytes(kind), cols, rows);
    fn run<C: Core>(data: &[u8], cols: u16, rows: u16, sb: usize, runs: usize) -> f64 {
        let mut rates: Vec<f64> = (0..runs)
            .map(|_| {
                let mut core = C::new(cols, rows, sb);
                let t = Instant::now();
                feed_all(&mut core, data);
                data.len() as f64 / 1e6 / t.elapsed().as_secs_f64()
            })
            .collect();
        median(&mut rates)
    }
    let rate = match core.as_str() {
        Ghostty::NAME | "ghostty" => run::<Ghostty>(&data, cols, rows, scrollback, opts.runs),
        Alacritty::NAME | "alacritty" => run::<Alacritty>(&data, cols, rows, scrollback, opts.runs),
        other => panic!("unknown core {other}"),
    };
    println!(
        "{core} {} {cols}x{rows} scrollback={scrollback}: {rate:.0} MB/s",
        kind.name()
    );
}

// ---------------------------------------------------------------------------
// Render-frame snapshot cost
// ---------------------------------------------------------------------------

fn frame(opts: &Opts) {
    println!(
        "## Render frame cost (µs per full visible-grid read; median over {} runs of {} frames)",
        opts.runs, opts.frames
    );
    println!();
    println!(
        "Full read: every visible cell (grapheme, resolved fg/bg RGB, attrs). `busy` = 4 KiB of new output fed before the frame (untimed); `idle` = nothing changed since the previous frame. Damage-driven: only rows the core reports dirty; `idle` = no change, `keystroke` = one echoed char/backspace."
    );
    println!();
    println!(
        "| workload | geometry | core | busy full mean | busy full p99 | idle full mean | idle full p99 | idle damage-driven mean | keystroke damage-driven mean | keystroke p99 |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|");
    for kind in Kind::ALL {
        for (cols, rows) in GEOMS {
            // Enough data to warm up + one 4 KiB chunk per frame.
            let need = (2 << 20) + opts.frames * 4096;
            let data = kind.generate(need, cols, rows);
            for (name, f) in [
                (Ghostty::NAME, frames::<Ghostty>(&data, cols, rows, opts)),
                (
                    Alacritty::NAME,
                    frames::<Alacritty>(&data, cols, rows, opts),
                ),
            ] {
                println!(
                    "| {} | {cols}x{rows} | {name} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} |",
                    kind.name(),
                    f.busy.mean_us,
                    f.busy.p99_us,
                    f.idle.mean_us,
                    f.idle.p99_us,
                    f.idle_dirty.mean_us,
                    f.typing.mean_us,
                    f.typing.p99_us
                );
            }
        }
    }
    println!();
}

struct FrameStats {
    busy: Summary,
    idle: Summary,
    idle_dirty: Summary,
    typing: Summary,
}

fn frames<C: Core>(data: &[u8], cols: u16, rows: u16, opts: &Opts) -> FrameStats {
    let (warm, rest) = data.split_at(2 << 20);
    let mut busy_runs = Vec::new();
    let mut idle_runs = Vec::new();
    let mut idle_dirty_runs = Vec::new();
    let mut typing_runs = Vec::new();
    for _ in 0..opts.runs {
        let mut core = C::new(cols, rows, SCROLLBACK);
        feed_all(&mut core, warm);
        core.render_frame(false);
        let mut busy = Vec::with_capacity(opts.frames);
        let mut idle = Vec::with_capacity(opts.frames);
        let mut idle_dirty = Vec::with_capacity(opts.frames);
        let mut typing = Vec::with_capacity(opts.frames);
        for (i, chunk) in rest.chunks(4096).take(opts.frames).enumerate() {
            core.feed(chunk);
            let t = Instant::now();
            core.render_frame(false);
            busy.push(t.elapsed().as_secs_f64() * 1e6);
            let t = Instant::now();
            core.render_frame(false);
            idle.push(t.elapsed().as_secs_f64() * 1e6);
            let t = Instant::now();
            core.render_frame(true);
            idle_dirty.push(t.elapsed().as_secs_f64() * 1e6);
            // One echoed keystroke (single cell changes), damage-driven redraw.
            core.feed(if i % 2 == 0 { b"x" } else { b"\x08" });
            let t = Instant::now();
            core.render_frame(true);
            typing.push(t.elapsed().as_secs_f64() * 1e6);
        }
        busy_runs.push(Summary::of(&mut busy));
        idle_runs.push(Summary::of(&mut idle));
        idle_dirty_runs.push(Summary::of(&mut idle_dirty));
        typing_runs.push(Summary::of(&mut typing));
    }
    FrameStats {
        busy: Summary::median_of(&busy_runs),
        idle: Summary::median_of(&idle_runs),
        idle_dirty: Summary::median_of(&idle_dirty_runs),
        typing: Summary::median_of(&typing_runs),
    }
}

// ---------------------------------------------------------------------------
// Resize / reflow
// ---------------------------------------------------------------------------

fn resize(opts: &Opts) {
    println!(
        "## Resize reflow cost (ms per resize with ~10k lines of build-log scrollback; median)"
    );
    println!();
    println!("| from | to | core | narrow (ms) | widen back (ms) | history lines before |");
    println!("|---|---|---|---|---|---|");
    // 4 MB of build log is ~40k+ logical lines: scrollback is saturated.
    let data = workload::build_log(4 << 20);
    for (cols, rows) in GEOMS {
        let (ncols, nrows) = (cols * 3 / 5, rows * 4 / 5);
        for (name, (narrow, widen, hist)) in [
            (
                Ghostty::NAME,
                reflow::<Ghostty>(&data, cols, rows, ncols, nrows, opts.runs),
            ),
            (
                Alacritty::NAME,
                reflow::<Alacritty>(&data, cols, rows, ncols, nrows, opts.runs),
            ),
        ] {
            println!(
                "| {cols}x{rows} | {ncols}x{nrows} | {name} | {narrow:.2} | {widen:.2} | {hist} |"
            );
        }
    }
    println!();
}

fn reflow<C: Core>(
    data: &[u8],
    cols: u16,
    rows: u16,
    ncols: u16,
    nrows: u16,
    runs: usize,
) -> (f64, f64, usize) {
    let mut narrow = Vec::new();
    let mut widen = Vec::new();
    let mut hist = 0;
    for _ in 0..runs {
        let mut core = C::new(cols, rows, SCROLLBACK);
        feed_all(&mut core, data);
        hist = core.history_lines();
        for _ in 0..3 {
            let t = Instant::now();
            core.resize(ncols, nrows);
            narrow.push(t.elapsed().as_secs_f64() * 1e3);
            let t = Instant::now();
            core.resize(cols, rows);
            widen.push(t.elapsed().as_secs_f64() * 1e3);
        }
    }
    (median(&mut narrow), median(&mut widen), hist)
}

// ---------------------------------------------------------------------------
// Memory (each sample in a fresh process so allocator state is clean)
// ---------------------------------------------------------------------------

fn memory() {
    println!("## Memory (RSS delta, fresh process per sample)");
    println!();
    println!(
        "`+compress` = after filling, call the core's scrollback compression (libghostty-vt `Terminal::compress(Full)`; alacritty_terminal has none). Time is for the compress call."
    );
    println!();
    println!("| scenario | geometry | core | RSS delta (MiB) | history lines |");
    println!("|---|---|---|---|---|");
    let exe = std::env::current_exe().expect("current exe");
    for scenario in ["empty", "ascii-cat", "build-log", "build-log+compress"] {
        for (cols, rows) in GEOMS {
            for core in [Ghostty::NAME, Alacritty::NAME] {
                let mut deltas = Vec::new();
                let mut hist = String::new();
                let mut extra = String::new();
                for _ in 0..3 {
                    let out = std::process::Command::new(&exe)
                        .args([
                            "mem-child",
                            core,
                            scenario,
                            &cols.to_string(),
                            &rows.to_string(),
                        ])
                        .output()
                        .expect("spawn mem-child");
                    let s = String::from_utf8_lossy(&out.stdout);
                    let mut parts = s.split_whitespace();
                    deltas.push(
                        parts
                            .next()
                            .and_then(|v| v.parse::<f64>().ok())
                            .unwrap_or(f64::NAN),
                    );
                    hist = parts.next().unwrap_or("?").to_owned();
                    extra = parts.collect::<Vec<_>>().join(" ");
                }
                println!(
                    "| {scenario} | {cols}x{rows} | {core} | {:.1} | {hist} {extra} |",
                    median(&mut deltas)
                );
            }
        }
    }
    println!();
}

fn rss_bytes() -> f64 {
    let statm = std::fs::read_to_string("/proc/self/statm").expect("statm");
    let pages: f64 = statm.split_whitespace().nth(1).unwrap().parse().unwrap();
    pages * 4096.0
}

fn mem_child(args: &[String]) {
    let core = args[0].as_str();
    let scenario = args[1].as_str();
    let cols: u16 = args[2].parse().unwrap();
    let rows: u16 = args[3].parse().unwrap();
    // 6 MB is >50k lines at either width: scrollback is saturated.
    let data = match scenario {
        "empty" => Vec::new(),
        "ascii-cat" => workload::ascii_text(6 << 20),
        "build-log" | "build-log+compress" => workload::build_log(6 << 20),
        other => panic!("unknown scenario {other}"),
    };
    let compress = scenario.ends_with("+compress");
    fn run<C: Core>(data: &[u8], cols: u16, rows: u16, compress: bool) -> (f64, usize, String) {
        let before = rss_bytes();
        let mut core = C::new(cols, rows, SCROLLBACK);
        feed_all(&mut core, data);
        core.render_frame(false);
        let mut note = String::new();
        if compress {
            let t = Instant::now();
            let ok = core.compress_scrollback();
            note = if ok {
                format!("(compress took {:.1} ms)", t.elapsed().as_secs_f64() * 1e3)
            } else {
                "(no compression API)".into()
            };
        }
        let delta = rss_bytes() - before;
        let hist = core.history_lines();
        std::hint::black_box(&core);
        (delta, hist, note)
    }
    let (delta, hist, note) = match core {
        Ghostty::NAME => run::<Ghostty>(&data, cols, rows, compress),
        Alacritty::NAME => run::<Alacritty>(&data, cols, rows, compress),
        other => panic!("unknown core {other}"),
    };
    println!("{:.2} {hist} {note}", delta / (1024.0 * 1024.0));
}
