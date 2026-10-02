//! libghostty-vt backend for [`TerminalCore`] (cargo feature `ghostty`).
//!
//! libghostty-vt's types are `!Send`; that is fine here because every pane's core lives on the
//! UI thread (output is fed from the event batch handler, snapshots are taken right after).
//! Per-cell reads are several C calls, so only rows the render state marks dirty are re-read.
//! Key input uses Ghostty's own `key::Encoder`, configured from the terminal's current modes
//! (cursor-key application mode, Kitty keyboard flags, ...).

use libghostty_vt::render::{CellIterator, Dirty, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::{RgbColor, Underline};
use libghostty_vt::{RenderState, Terminal, key};

use crate::term::{ansi16, Attrs, CellSnap, Rgba, Snapshot, TerminalCore, DEFAULT_BG, DEFAULT_FG};

pub struct GhosttyCore {
    term: Terminal<'static, 'static>,
    render: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    graphemes: Vec<char>,
    encoder: key::Encoder<'static>,
    event: key::Event<'static>,
    generation: u64,
    cols: usize,
    nrows: usize,
}

fn rgb(c: Rgba) -> RgbColor {
    RgbColor { r: c.0, g: c.1, b: c.2 }
}

impl GhosttyCore {
    pub fn new(cols: usize, rows: usize) -> Self {
        let mut term = Terminal::new(cols as u16, rows as u16).expect("ghostty terminal");
        term.set_scrollback_max_bytes(None).expect("scrollback bytes");
        term.set_scrollback_max_lines(Some(2000)).expect("scrollback lines");
        // Same theme as the alacritty backend so screenshots are comparable.
        term.set_default_fg_color(Some(rgb(DEFAULT_FG))).expect("fg");
        term.set_default_bg_color(Some(rgb(DEFAULT_BG))).expect("bg");
        if let Ok(mut pal) = term.default_color_palette() {
            for i in 0..16u8 {
                pal.0[i as usize] = rgb(ansi16(i));
            }
            let _ = term.set_default_color_palette(Some(pal));
        }
        GhosttyCore {
            term,
            render: RenderState::new().expect("render state"),
            rows: RowIterator::new().expect("row iter"),
            cells: CellIterator::new().expect("cell iter"),
            graphemes: vec!['\0'; 16],
            encoder: key::Encoder::new().expect("key encoder"),
            event: key::Event::new().expect("key event"),
            generation: 0,
            cols,
            nrows: rows,
        }
    }

    fn encode(&mut self, k: key::Key, mods: key::Mods, text: Option<&str>, unshifted: Option<char>) -> Option<Vec<u8>> {
        self.encoder.set_options_from_terminal(&self.term);
        self.event
            .set_action(key::Action::Press)
            .set_key(k)
            .set_mods(mods)
            .set_utf8(text.map(|t| t.to_string()));
        if let Some(u) = unshifted {
            self.event.set_unshifted_codepoint(u);
        }
        // `encode_to_vec` can fail with OutOfSpace even after its retry; use a fixed buffer.
        let mut buf = [0u8; 128];
        match self.encoder.encode(&self.event, &mut buf) {
            Ok(n) if n > 0 => Some(buf[..n].to_vec()),
            _ => None,
        }
    }
}

fn map_key(name: &str) -> Option<(key::Key, Option<char>)> {
    use key::Key as K;
    let k = match name {
        "enter" => K::Enter,
        "tab" => K::Tab,
        "escape" => K::Escape,
        "backspace" => K::Backspace,
        "delete" => K::Delete,
        "insert" => K::Insert,
        "up" => K::ArrowUp,
        "down" => K::ArrowDown,
        "left" => K::ArrowLeft,
        "right" => K::ArrowRight,
        "home" => K::Home,
        "end" => K::End,
        "pageup" => K::PageUp,
        "pagedown" => K::PageDown,
        "f1" => K::F1,
        "f2" => K::F2,
        "f3" => K::F3,
        "f4" => K::F4,
        "f5" => K::F5,
        "f6" => K::F6,
        "f7" => K::F7,
        "f8" => K::F8,
        "f9" => K::F9,
        "f10" => K::F10,
        "f11" => K::F11,
        "f12" => K::F12,
        " " => return Some((K::Space, Some(' '))),
        s if s.chars().count() == 1 => {
            let c = s.chars().next().unwrap().to_ascii_lowercase();
            let k = match c {
                'a'..='z' => {
                    const LETTERS: [K; 26] = [
                        K::A, K::B, K::C, K::D, K::E, K::F, K::G, K::H, K::I, K::J, K::K, K::L, K::M,
                        K::N, K::O, K::P, K::Q, K::R, K::S, K::T, K::U, K::V, K::W, K::X, K::Y, K::Z,
                    ];
                    LETTERS[(c as u8 - b'a') as usize]
                }
                '0'..='9' => {
                    const DIGITS: [K; 10] =
                        [K::Digit0, K::Digit1, K::Digit2, K::Digit3, K::Digit4, K::Digit5, K::Digit6, K::Digit7, K::Digit8, K::Digit9];
                    DIGITS[(c as u8 - b'0') as usize]
                }
                '[' => K::BracketLeft,
                ']' => K::BracketRight,
                '\\' => K::Backslash,
                '-' => K::Minus,
                '=' => K::Equal,
                ',' => K::Comma,
                '.' => K::Period,
                '/' => K::Slash,
                ';' => K::Semicolon,
                '\'' => K::Quote,
                '`' => K::Backquote,
                _ => K::Unidentified,
            };
            return Some((k, Some(c)));
        }
        _ => return None,
    };
    Some((k, None))
}

impl TerminalCore for GhosttyCore {
    fn name(&self) -> &'static str {
        "libghostty-vt"
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.term.vt_write(bytes);
    }

    fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(2);
        let rows = rows.max(1);
        if (cols, rows) != (self.cols, self.nrows) {
            let _ = self.term.resize(cols as u16, rows as u16, 8, 16);
            self.cols = cols;
            self.nrows = rows;
        }
    }

    fn size(&self) -> (usize, usize) {
        (self.cols, self.nrows)
    }

    fn snapshot_into(&mut self, snap: &mut Snapshot) -> usize {
        let (cols, rows) = (self.cols, self.nrows);
        let full = snap.cols != cols || snap.rows != rows;
        if full {
            snap.cols = cols;
            snap.rows = rows;
            snap.cells = vec![CellSnap { ch: ' ', fg: DEFAULT_FG, bg: DEFAULT_BG, attrs: Attrs::default(), width: 1 }; cols * rows];
            snap.row_gen = vec![0; rows];
        }
        let Ok(rs) = self.render.update(&self.term) else { return 0 };
        let dirty = rs.dirty().unwrap_or(Dirty::Full);
        // Cursor is cheap to refresh every time.
        let visible = rs.cursor_visible().unwrap_or(true);
        snap.cursor = match rs.cursor_viewport() {
            Ok(Some(c)) if visible => Some((c.y as usize, c.x as usize)),
            _ => None,
        };
        if !full && dirty == Dirty::Clean {
            return 0;
        }
        let all = full || dirty == Dirty::Full;
        let colors = match rs.colors() {
            Ok(c) => c,
            Err(_) => return 0,
        };
        let dfg = Rgba(colors.foreground.r, colors.foreground.g, colors.foreground.b);
        let dbg = Rgba(colors.background.r, colors.background.g, colors.background.b);
        self.generation += 1;
        let mut rebuilt = 0;
        let Ok(mut row_iter) = self.rows.update(&rs) else { return 0 };
        let mut r = 0usize;
        while let Some(row) = row_iter.next() {
            if r >= rows {
                break;
            }
            if !all && !row.dirty().unwrap_or(true) {
                r += 1;
                continue;
            }
            rebuilt += 1;
            if let Ok(mut cell_iter) = self.cells.update(row) {
                let mut c = 0usize;
                while let Some(cell) = cell_iter.next() {
                    if c >= cols {
                        break;
                    }
                    let n = cell.graphemes_len().unwrap_or(0);
                    let mut ch = ' ';
                    if n > 0 {
                        if n > self.graphemes.len() {
                            self.graphemes.resize(n, '\0');
                        }
                        if cell.graphemes_buf(&mut self.graphemes[..n]).is_ok() {
                            // The snapshot keeps one char per cell; combining marks are dropped.
                            ch = self.graphemes[0];
                        }
                    }
                    let st = cell.style().ok();
                    let mut fg = cell.fg_color().ok().flatten().map(|c| Rgba(c.r, c.g, c.b)).unwrap_or(dfg);
                    let mut bg = cell.bg_color().ok().flatten().map(|c| Rgba(c.r, c.g, c.b)).unwrap_or(dbg);
                    let mut attrs = Attrs::default();
                    if let Some(st) = st {
                        attrs = Attrs {
                            bold: st.bold,
                            italic: st.italic,
                            underline: !matches!(st.underline, Underline::None),
                            dim: st.faint,
                            strike: st.strikethrough,
                        };
                        if st.inverse {
                            std::mem::swap(&mut fg, &mut bg);
                        }
                        if st.invisible {
                            ch = ' ';
                        }
                        if st.faint {
                            fg = Rgba((fg.0 as u16 * 2 / 3) as u8, (fg.1 as u16 * 2 / 3) as u8, (fg.2 as u16 * 2 / 3) as u8);
                        }
                    }
                    let width = match cell.raw_cell().and_then(|rc| rc.wide()) {
                        Ok(CellWide::Wide) => 2,
                        Ok(CellWide::SpacerTail) | Ok(CellWide::SpacerHead) => 0,
                        _ => 1,
                    };
                    snap.cells[r * cols + c] = CellSnap { ch, fg, bg, attrs, width };
                    c += 1;
                }
                for cc in c..cols {
                    snap.cells[r * cols + cc] = CellSnap { ch: ' ', fg: dfg, bg: dbg, attrs: Attrs::default(), width: 1 };
                }
            }
            let _ = row.set_dirty(false);
            snap.row_gen[r] = self.generation;
            r += 1;
        }
        let _ = rs.set_dirty(Dirty::Clean);
        rebuilt
    }

    fn encode_key(&mut self, name: &str, ctrl: bool, alt: bool, shift: bool, chars: &str) -> Option<Vec<u8>> {
        let (k, unshifted) = map_key(name)?;
        let mut mods = key::Mods::empty();
        if ctrl {
            mods |= key::Mods::CTRL;
        }
        if alt {
            mods |= key::Mods::ALT;
        }
        if shift {
            mods |= key::Mods::SHIFT;
        }
        let text = if !chars.is_empty() && !chars.chars().any(|c| c.is_control()) { Some(chars) } else { None };
        self.encode(k, mods, text, unshifted)
    }

    fn encode_text(&mut self, text: &str) -> Vec<u8> {
        // Single printable keys go through the encoder (so Kitty keyboard mode is honoured);
        // multi-char text (IME commits, paste) is sent as UTF-8.
        let mut it = text.chars();
        if let (Some(c), None) = (it.next(), it.next())
            && c.is_ascii_graphic()
        {
            let lower = c.to_ascii_lowercase();
            let shift = c.is_ascii_uppercase();
            if let Some((k, _)) = map_key(&lower.to_string())
                && k != key::Key::Unidentified
            {
                let mods = if shift { key::Mods::SHIFT } else { key::Mods::empty() };
                if let Some(b) = self.encode(k, mods, Some(text), Some(lower)) {
                    return b;
                }
            }
        }
        text.as_bytes().to_vec()
    }
}
