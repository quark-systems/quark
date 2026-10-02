//! Deterministic synthetic terminal workloads.
//!
//! Every generator is seeded, so two runs (and both cores) see byte-identical
//! input. Sizes are approximate targets; generators stop at the first line
//! boundary past the target.

use std::fmt::Write as _;

/// Small, fast, deterministic PRNG (xorshift64*). Quality is irrelevant here;
/// reproducibility is what matters.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }

    pub fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Large colourful compiler/build log: SGR 16/256/truecolor, long wrapped lines.
    BuildLog,
    /// Full-screen TUI redraws: alt screen, CUP, scroll regions, EL/ED (top/vim-like).
    Tui,
    /// Unicode-heavy text: CJK wide chars, ZWJ emoji, flags, combining marks.
    Unicode,
    /// Plain ASCII `cat` of a large text file.
    Ascii,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::BuildLog, Kind::Tui, Kind::Unicode, Kind::Ascii];

    pub fn name(self) -> &'static str {
        match self {
            Kind::BuildLog => "build-log",
            Kind::Tui => "tui-redraw",
            Kind::Unicode => "unicode",
            Kind::Ascii => "ascii-cat",
        }
    }

    /// Generate the workload. The TUI stream is geometry-specific (it targets
    /// the full screen), so it takes the terminal size.
    pub fn generate(self, target_bytes: usize, cols: u16, rows: u16) -> Vec<u8> {
        match self {
            Kind::BuildLog => build_log(target_bytes),
            Kind::Tui => tui_stream(target_bytes, cols, rows),
            Kind::Unicode => unicode_text(target_bytes),
            Kind::Ascii => ascii_text(target_bytes),
        }
    }
}

const WORDS: &[&str] = &[
    "the",
    "quick",
    "brown",
    "fox",
    "jumps",
    "over",
    "lazy",
    "dog",
    "terminal",
    "emulator",
    "render",
    "buffer",
    "scroll",
    "region",
    "cursor",
    "glyph",
    "atlas",
    "shader",
    "frame",
    "latency",
    "throughput",
    "parser",
    "escape",
    "sequence",
    "unicode",
    "grapheme",
    "cluster",
    "daemon",
    "session",
    "workspace",
    "agent",
    "window",
    "pane",
    "split",
    "layout",
    "resize",
    "reflow",
    "history",
    "selection",
    "clipboard",
    "kitty",
    "keyboard",
    "protocol",
    "mouse",
];

const PATHS: &[&str] = &[
    "src/terminal/parser.rs",
    "src/render/atlas.rs",
    "crates/quark-core/src/session/mod.rs",
    "crates/quark-ui/src/widgets/pane.rs",
    "third_party/harfbuzz/src/hb-ot-shape.cc",
    "lib/libc/musl/src/stdio/vfprintf.c",
    "pkg/vendor/github.com/acme/very/long/package/path/internal/handler.go",
];

fn words(rng: &mut Rng, out: &mut String, n: usize) {
    for i in 0..n {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(rng.pick(WORDS));
    }
}

/// (a) Colourful build log, roughly cargo/cmake/clang output piped through a
/// colouriser. Mix of SGR 16-colour, 256-colour and truecolor sequences,
/// bold/underline/italic, and lines that wrap well past 200 columns.
pub fn build_log(target: usize) -> Vec<u8> {
    let mut rng = Rng::new(0x0B01_D106);
    let mut s = String::with_capacity(target + 4096);
    let mut n: u64 = 0;
    while s.len() < target {
        n += 1;
        let pct = (n / 97) % 100;
        match rng.below(10) {
            // cmake-style progress with 16-colour green
            0..=3 => {
                let _ = write!(
                    s,
                    "[{pct:3}%] \x1b[32mBuilding CXX object\x1b[0m {}.o\r\n",
                    rng.pick(PATHS)
                );
            }
            // cargo-style "Compiling" with bold 256-colour
            4 | 5 => {
                let c = 16 + rng.below(216);
                let _ = write!(
                    s,
                    "\x1b[1;38;5;{c}m   Compiling\x1b[0m quark-{} v0.{}.{} (/home/dev/quark/crates/",
                    rng.pick(WORDS),
                    rng.below(30),
                    rng.below(10)
                );
                words(&mut rng, &mut s, 2);
                s.push_str(")\r\n");
            }
            // clang-style warning with truecolor + underline + a very long line
            6 | 7 => {
                let (r, g, b) = (200 + rng.below(56), 120 + rng.below(80), rng.below(60));
                let _ = write!(
                    s,
                    "\x1b[1m{}:{}:{}: \x1b[38;2;{r};{g};{b}mwarning:\x1b[0m\x1b[1m unused variable '",
                    rng.pick(PATHS),
                    1 + rng.below(2000),
                    1 + rng.below(120)
                );
                words(&mut rng, &mut s, 1);
                s.push_str("' [-Wunused-variable]\x1b[0m\r\n    ");
                // Long source excerpt: wraps at 80 and often at 200 columns.
                let len = 20 + rng.below(50) as usize;
                for _ in 0..len {
                    let fg = 31 + rng.below(7);
                    let _ = write!(s, "\x1b[{fg}m{}\x1b[39m ", rng.pick(WORDS));
                }
                s.push_str("\r\n    \x1b[32m^~~~~~~~\x1b[0m\r\n");
            }
            // test-runner line with background colours and italics
            8 => {
                let ok = rng.chance(90);
                let (bg, label) = if ok { (42, " PASS ") } else { (41, " FAIL ") };
                let _ = write!(
                    s,
                    "\x1b[30;{bg}m{label}\x1b[0m \x1b[3m{}::",
                    rng.pick(PATHS)
                );
                words(&mut rng, &mut s, 3);
                let _ = write!(s, "\x1b[23m \x1b[2m({} ms)\x1b[22m\r\n", rng.below(5000));
            }
            // plain uncoloured line (linker spam), long
            _ => {
                s.push_str("note: ");
                let len = 10 + rng.below(40) as usize;
                words(&mut rng, &mut s, len);
                s.push_str("\r\n");
            }
        }
    }
    s.into_bytes()
}

/// (b) Full-screen TUI redraw stream. Alternates between a `top`-like frame
/// (header + process table redrawn with CUP/EL, reverse-video header) and a
/// `vim`-like frame (scroll region set to the text area, scrolled with
/// LF/RI/SU/SD, gutter numbers, status + command lines). Ends by leaving a
/// known cursor position so the cores can be cross-checked.
pub fn tui_stream(target: usize, cols: u16, rows: u16) -> Vec<u8> {
    let mut rng = Rng::new(0x007E_10D0);
    let cols = cols as usize;
    let rows = rows as usize;
    let mut s = String::with_capacity(target + 4096);
    s.push_str("\x1b[?1049h\x1b[?25l\x1b[H\x1b[2J");
    let mut frame: u64 = 0;
    while s.len() < target {
        frame += 1;
        if frame.is_multiple_of(2) {
            // top-like: absolute redraw of every row.
            s.push_str("\x1b[H");
            let _ = write!(
                s,
                "\x1b[1mtop - 12:{:02}:{:02} up 3 days, load average: {}.{:02}\x1b[0m\x1b[K",
                (frame / 60) % 60,
                frame % 60,
                rng.below(8),
                rng.below(100)
            );
            let _ = write!(
                s,
                "\x1b[2;1HTasks: \x1b[1m{}\x1b[0m total, \x1b[1;32m{}\x1b[0m running\x1b[K",
                200 + rng.below(100),
                rng.below(10)
            );
            let header = "  PID USER      PR  NI    VIRT    RES  %CPU %MEM     TIME+ COMMAND";
            let _ = write!(s, "\x1b[4;1H\x1b[7m{header:<cols$.cols$}\x1b[27m");
            for r in 5..=rows {
                let cpu = rng.below(1000);
                let color = if cpu > 500 { "\x1b[1;31m" } else { "" };
                let _ = write!(
                    s,
                    "\x1b[{r};1H{color}{:5} quark     20   0 {:7} {:6} {:3}.{} {:4}.{} {:3}:{:02}.{:02} {}\x1b[0m\x1b[K",
                    1000 + rng.below(60000),
                    rng.below(9_999_999),
                    rng.below(999_999),
                    cpu / 10,
                    cpu % 10,
                    rng.below(100),
                    rng.below(10),
                    rng.below(100),
                    rng.below(60),
                    rng.below(100),
                    rng.pick(WORDS)
                );
            }
        } else {
            // vim-like: scroll region over the text area, scroll a few lines,
            // repaint the exposed lines, then repaint status/command lines.
            let text_rows = rows - 2;
            let _ = write!(s, "\x1b[1;{text_rows}r");
            let lines = 1 + rng.below(4) as usize;
            let down = rng.chance(70);
            if down {
                // Scroll up (content moves up) via LF at the bottom margin or SU.
                if rng.chance(50) {
                    let _ = write!(s, "\x1b[{text_rows};1H");
                    for _ in 0..lines {
                        s.push('\n');
                    }
                } else {
                    let _ = write!(s, "\x1b[{lines}S");
                }
            } else if rng.chance(50) {
                s.push_str("\x1b[1;1H");
                for _ in 0..lines {
                    s.push_str("\x1bM"); // RI
                }
            } else {
                let _ = write!(s, "\x1b[{lines}T");
            }
            let first = if down { text_rows - lines + 1 } else { 1 };
            for r in first..first + lines {
                let _ = write!(s, "\x1b[{r};1H\x1b[33m{:4} \x1b[0m", frame as usize * 3 + r);
                let mut line = String::new();
                let n = 4 + rng.below(12) as usize;
                words(&mut rng, &mut line, n);
                line.truncate(cols.saturating_sub(5));
                let kw = rng.pick(&["fn", "let", "match", "impl"]);
                let _ = write!(s, "\x1b[1;34m{kw}\x1b[0m {line}\x1b[K");
            }
            s.push_str("\x1b[r");
            let status = format!(
                " NORMAL  src/main.rs  [+]  {}:{}",
                frame % 999,
                rng.below(80)
            );
            let _ = write!(
                s,
                "\x1b[{};1H\x1b[30;47m{status:<cols$.cols$}\x1b[0m\x1b[{rows};1H\x1b[K",
                rows - 1
            );
        }
    }
    // Deterministic final state for correctness checks: a marker at a fixed
    // spot, cursor parked at (row 3, col 7) 1-based, cursor shown again.
    let _ = write!(s, "\x1b[2;3HMARK\x1b[3;7H\x1b[?25h");
    s.into_bytes()
}

const CJK: &[&str] = &[
    "终端",
    "渲染",
    "性能",
    "测试",
    "日本語",
    "表示",
    "한국어",
    "텍스트",
    "漢字",
    "東京",
];
const EMOJI: &[&str] = &[
    "😀",
    "🚀",
    "🎉",
    "👍🏽",
    "👨‍👩‍👧‍👦",
    "🏳️‍🌈",
    "🇯🇵",
    "🇺🇸",
    "❤️",
    "🧑‍💻",
    "👩🏾‍🚀",
];
const COMBINING: &[&str] = &[
    "e\u{301}",
    "a\u{308}",
    "n\u{303}",
    "o\u{302}\u{323}",
    "Z\u{335}\u{30C}",
    "ก\u{e34}",
    "अ\u{902}",
];

/// (c) Unicode-heavy text: CJK (wide), emoji incl. ZWJ sequences, skin-tone
/// modifiers, flags, VS16, and combining marks. Colour sprinkled in.
pub fn unicode_text(target: usize) -> Vec<u8> {
    let mut rng = Rng::new(0xC0DE_F00D);
    let mut s = String::with_capacity(target + 4096);
    while s.len() < target {
        let items = 5 + rng.below(20);
        for _ in 0..items {
            match rng.below(4) {
                0 => s.push_str(rng.pick(CJK)),
                1 => s.push_str(rng.pick(EMOJI)),
                2 => s.push_str(rng.pick(COMBINING)),
                _ => s.push_str(rng.pick(WORDS)),
            }
            if rng.chance(15) {
                let _ = write!(s, "\x1b[3{}m", 1 + rng.below(6));
            } else if rng.chance(10) {
                s.push_str("\x1b[0m");
            }
            s.push(' ');
        }
        s.push_str("\x1b[0m\r\n");
    }
    s.into_bytes()
}

/// (d) Plain ASCII prose, like `cat` of a large log/text file. Line lengths
/// vary 0-160 so some lines wrap at 80 columns.
pub fn ascii_text(target: usize) -> Vec<u8> {
    let mut rng = Rng::new(0x000A_5C11);
    let mut s = String::with_capacity(target + 256);
    while s.len() < target {
        let n = rng.below(26) as usize;
        words(&mut rng, &mut s, n);
        s.push('\n');
    }
    // `cat` to a tty goes through ONLCR, so the terminal sees CRLF.
    s.replace('\n', "\r\n").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        for k in Kind::ALL {
            assert_eq!(k.generate(50_000, 80, 24), k.generate(50_000, 80, 24));
        }
    }

    #[test]
    fn valid_utf8() {
        for k in Kind::ALL {
            assert!(std::str::from_utf8(&k.generate(50_000, 200, 50)).is_ok());
        }
    }
}
