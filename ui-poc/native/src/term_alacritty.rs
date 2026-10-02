//! `alacritty_terminal` backend for [`TerminalCore`] (desktop only: the crate's PTY event loop
//! depends on `polling`, which does not build for wasm32).

use alacritty_terminal::event::{Event as TermEvent, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermDamage};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor, Rgb};

use crate::term::{ansi16, Attrs, CellSnap, Rgba, Snapshot, TerminalCore, DEFAULT_BG, DEFAULT_FG};

#[derive(Clone)]
struct NoEvents;
impl EventListener for NoEvents {
    fn send_event(&self, _event: TermEvent) {}
}

struct Size {
    cols: usize,
    rows: usize,
}
impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

pub struct AlacrittyCore {
    term: Term<NoEvents>,
    parser: Processor,
    generation: u64,
}

impl AlacrittyCore {
    pub fn new(cols: usize, rows: usize) -> Self {
        let config = Config {
            scrolling_history: 2000,
            ..Default::default()
        };
        AlacrittyCore {
            term: Term::new(config, &Size { cols, rows }, NoEvents),
            parser: Processor::new(),
            generation: 0,
        }
    }
}

fn indexed(i: u8) -> Rgba {
    match i {
        0..=15 => ansi16(i),
        16..=231 => {
            let i = i - 16;
            let c = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Rgba(c(i / 36), c((i / 6) % 6), c(i % 6))
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            Rgba(v, v, v)
        }
    }
}

fn resolve(c: Color, fg: bool, bold: bool, dim: bool) -> Rgba {
    let rgb = |r: Rgb| Rgba(r.r, r.g, r.b);
    let mut out = match c {
        Color::Spec(r) => rgb(r),
        Color::Indexed(i) => indexed(if fg && bold && i < 8 { i + 8 } else { i }),
        Color::Named(n) => match n {
            NamedColor::Foreground | NamedColor::BrightForeground => DEFAULT_FG,
            NamedColor::Background => DEFAULT_BG,
            NamedColor::DimForeground => Rgba(0x8a, 0x8f, 0x98),
            NamedColor::Cursor => DEFAULT_FG,
            other => {
                let idx = other as usize;
                if idx < 16 {
                    ansi16(if fg && bold && idx < 8 { idx as u8 + 8 } else { idx as u8 })
                } else if idx >= NamedColor::DimBlack as usize && idx <= NamedColor::DimWhite as usize {
                    let base = ansi16((idx - NamedColor::DimBlack as usize) as u8);
                    Rgba(base.0 * 2 / 3, base.1 * 2 / 3, base.2 * 2 / 3)
                } else if fg {
                    DEFAULT_FG
                } else {
                    DEFAULT_BG
                }
            }
        },
    };
    if dim && fg {
        out = Rgba(
            (out.0 as u16 * 2 / 3) as u8,
            (out.1 as u16 * 2 / 3) as u8,
            (out.2 as u16 * 2 / 3) as u8,
        );
    }
    out
}

impl TerminalCore for AlacrittyCore {
    fn name(&self) -> &'static str {
        "alacritty_terminal"
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(2);
        let rows = rows.max(1);
        if (cols, rows) != self.size() {
            self.term.resize(Size { cols, rows });
        }
    }

    fn size(&self) -> (usize, usize) {
        (self.term.columns(), self.term.screen_lines())
    }

    fn snapshot_into(&mut self, snap: &mut Snapshot) -> usize {
        let (cols, rows) = self.size();
        let full = snap.cols != cols || snap.rows != rows;
        if full {
            snap.cols = cols;
            snap.rows = rows;
            snap.cells = vec![
                CellSnap { ch: ' ', fg: DEFAULT_FG, bg: DEFAULT_BG, attrs: Attrs::default(), width: 1 };
                cols * rows
            ];
            snap.row_gen = vec![0; rows];
        }
        let mut dirty: Vec<usize> = Vec::new();
        match self.term.damage() {
            TermDamage::Full => dirty.extend(0..rows),
            TermDamage::Partial(it) => dirty.extend(it.map(|d| d.line)),
        }
        if full {
            dirty = (0..rows).collect();
        }
        self.term.reset_damage();
        self.generation += 1;
        let grid = self.term.grid();
        let offset = grid.display_offset() as i32;
        for &r in &dirty {
            if r >= rows {
                continue;
            }
            let line = Line(r as i32 - offset);
            let row = &grid[line];
            for c in 0..cols {
                let cell = &row[Column(c)];
                let f = cell.flags;
                let bold = f.contains(Flags::BOLD);
                let dim = f.contains(Flags::DIM);
                let mut fg = resolve(cell.fg, true, bold, dim);
                let mut bg = resolve(cell.bg, false, false, false);
                if f.contains(Flags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let width = if f.contains(Flags::WIDE_CHAR_SPACER) {
                    0
                } else if f.contains(Flags::WIDE_CHAR) {
                    2
                } else {
                    1
                };
                let ch = if f.contains(Flags::HIDDEN) || cell.c == '\0' { ' ' } else { cell.c };
                snap.cells[r * cols + c] = CellSnap {
                    ch,
                    fg,
                    bg,
                    attrs: Attrs {
                        bold,
                        italic: f.contains(Flags::ITALIC),
                        underline: f.intersects(Flags::ALL_UNDERLINES),
                        dim,
                        strike: f.contains(Flags::STRIKEOUT),
                    },
                    width,
                };
            }
            snap.row_gen[r] = self.generation;
        }
        let cur = self.term.grid().cursor.point;
        let show = self
            .term
            .mode()
            .contains(alacritty_terminal::term::TermMode::SHOW_CURSOR);
        snap.cursor = if show && offset == 0 {
            Some((cur.line.0.max(0) as usize, cur.column.0))
        } else {
            None
        };
        dirty.len()
    }

}
