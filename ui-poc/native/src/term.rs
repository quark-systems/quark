//! Terminal core abstraction. The UI only talks to [`TerminalCore`]. Backends:
//! `alacritty_terminal` (`term_alacritty.rs`, default on desktop), libghostty-vt
//! (`term_ghostty.rs`, feature `ghostty`), and a placeholder on wasm32.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgba(pub u8, pub u8, pub u8);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Attrs {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub dim: bool,
    pub strike: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellSnap {
    pub ch: char,
    pub fg: Rgba,
    pub bg: Rgba,
    pub attrs: Attrs,
    /// 2 for the leading half of a wide char, 0 for its spacer, 1 otherwise.
    pub width: u8,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub cols: usize,
    pub rows: usize,
    /// rows * cols cells, row-major.
    pub cells: Vec<CellSnap>,
    pub cursor: Option<(usize, usize)>,
    /// Bumped per row whenever that row changed since the previous snapshot.
    pub row_gen: Vec<u64>,
}

pub trait TerminalCore {
    fn feed(&mut self, bytes: &[u8]);
    fn resize(&mut self, cols: usize, rows: usize);
    #[allow(dead_code)] // unused on wasm
    fn size(&self) -> (usize, usize);
    /// Update `snap` in place, only touching rows that changed. Returns the number of rows rebuilt.
    fn snapshot_into(&mut self, snap: &mut Snapshot) -> usize;
    /// Encode a non-text key press (named key or chord) to the bytes the app expects.
    /// Default: legacy xterm encoding.
    fn encode_key(&mut self, key: &str, ctrl: bool, alt: bool, shift: bool, chars: &str) -> Option<Vec<u8>> {
        legacy_key_bytes(key, ctrl, alt, shift, chars)
    }
    /// Encode committed text (typed chars / IME commit). Default: raw UTF-8.
    fn encode_text(&mut self, text: &str) -> Vec<u8> {
        text.as_bytes().to_vec()
    }
    fn name(&self) -> &'static str;
}

pub fn legacy_key_bytes(key: &str, ctrl: bool, alt: bool, shift: bool, chars: &str) -> Option<Vec<u8>> {
    let b: Vec<u8> = match key {
        "enter" => b"\r".to_vec(),
        "backspace" => vec![0x7f],
        "tab" if shift => b"\x1b[Z".to_vec(),
        "tab" => b"\t".to_vec(),
        "escape" => vec![0x1b],
        "up" => b"\x1b[A".to_vec(),
        "down" => b"\x1b[B".to_vec(),
        "right" => b"\x1b[C".to_vec(),
        "left" => b"\x1b[D".to_vec(),
        "home" => b"\x1b[H".to_vec(),
        "end" => b"\x1b[F".to_vec(),
        "delete" => b"\x1b[3~".to_vec(),
        "pageup" => b"\x1b[5~".to_vec(),
        "pagedown" => b"\x1b[6~".to_vec(),
        k if ctrl && k.len() == 1 => {
            let ch = k.as_bytes()[0].to_ascii_lowercase();
            if ch.is_ascii_lowercase() || b"@[\\]^_".contains(&ch) {
                vec![ch & 0x1f]
            } else {
                return None;
            }
        }
        _ if alt && !chars.is_empty() => {
            let mut v = vec![0x1b];
            v.extend_from_slice(chars.as_bytes());
            v
        }
        _ => return None,
    };
    Some(b)
}

/// Creates the configured backend ("alacritty" or, with the `ghostty` feature, "ghostty").
pub fn new_core(kind: &str, cols: usize, rows: usize) -> Box<dyn TerminalCore> {
    match kind {
        #[cfg(feature = "ghostty")]
        "ghostty" => Box::new(crate::term_ghostty::GhosttyCore::new(cols, rows)),
        #[cfg(not(target_family = "wasm"))]
        _ => Box::new(crate::term_alacritty::AlacrittyCore::new(cols, rows)),
        #[cfg(target_family = "wasm")]
        _ => {
            let _ = kind;
            Box::new(NullCore { cols, rows, shown: false })
        }
    }
}

pub const DEFAULT_FG: Rgba = Rgba(0xd4, 0xd7, 0xde);
pub const DEFAULT_BG: Rgba = Rgba(0x10, 0x12, 0x16);

pub fn ansi16(i: u8) -> Rgba {
    const P: [Rgba; 16] = [
        Rgba(0x1d, 0x1f, 0x24),
        Rgba(0xe0, 0x6c, 0x75),
        Rgba(0x98, 0xc3, 0x79),
        Rgba(0xe5, 0xc0, 0x7b),
        Rgba(0x61, 0xaf, 0xef),
        Rgba(0xc6, 0x78, 0xdd),
        Rgba(0x56, 0xb6, 0xc2),
        Rgba(0xab, 0xb2, 0xbf),
        Rgba(0x5c, 0x63, 0x70),
        Rgba(0xff, 0x7b, 0x86),
        Rgba(0xb5, 0xe8, 0x90),
        Rgba(0xff, 0xd6, 0x8a),
        Rgba(0x7d, 0xc4, 0xff),
        Rgba(0xde, 0x95, 0xf0),
        Rgba(0x7a, 0xd8, 0xe4),
        Rgba(0xff, 0xff, 0xff),
    ];
    P[i as usize & 15]
}

/// Web build placeholder: no VT parser that builds for wasm32 is wired in (alacritty_terminal
/// pulls in a PTY event loop), so panes only show a notice and ignore output.
#[cfg(target_family = "wasm")]
pub struct NullCore {
    cols: usize,
    rows: usize,
    shown: bool,
}

#[cfg(target_family = "wasm")]
impl TerminalCore for NullCore {
    fn name(&self) -> &'static str {
        "none (web build)"
    }
    fn feed(&mut self, _bytes: &[u8]) {}
    fn resize(&mut self, cols: usize, rows: usize) {
        self.cols = cols.max(2);
        self.rows = rows.max(1);
        self.shown = false;
    }
    fn size(&self) -> (usize, usize) {
        (self.cols, self.rows)
    }
    fn snapshot_into(&mut self, snap: &mut Snapshot) -> usize {
        if self.shown && snap.cols == self.cols && snap.rows == self.rows {
            return 0;
        }
        self.shown = true;
        let (cols, rows) = (self.cols, self.rows);
        let blank = CellSnap { ch: ' ', fg: DEFAULT_FG, bg: DEFAULT_BG, attrs: Attrs::default(), width: 1 };
        snap.cols = cols;
        snap.rows = rows;
        snap.cells = vec![blank; cols * rows];
        snap.row_gen = vec![1; rows];
        snap.cursor = None;
        let msg = "Terminals are not available in the web build (no wasm32 terminal core).";
        for (c, ch) in msg.chars().take(cols).enumerate() {
            snap.cells[c] = CellSnap { ch, fg: ansi16(3), ..blank };
        }
        rows
    }
}

