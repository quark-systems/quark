//! A thin, uniform facade over the two terminal cores so the harness can
//! drive them identically.

use std::hint::black_box;

/// Width class of a cell, normalised across both cores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Width {
    Narrow,
    Wide,
    /// Trailing half of a wide char (do not render).
    SpacerTail,
    /// Padding at the end of a soft-wrapped line before a wide char.
    SpacerHead,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellInfo {
    /// Full grapheme cluster (base + combining/ZWJ continuation), "" if empty.
    pub text: String,
    pub width: Width,
}

pub trait Core {
    const NAME: &'static str;

    fn new(cols: u16, rows: u16, scrollback_lines: usize) -> Self;
    /// Feed raw PTY output.
    fn feed(&mut self, bytes: &[u8]);
    /// What a renderer does once per frame: refresh whatever per-frame state
    /// the core needs, then visit visible cells and read each grapheme,
    /// resolved fg/bg RGB and attributes. With `dirty_only` it uses the
    /// core's own damage tracking and only visits changed rows; otherwise it
    /// visits every visible cell. Returns a checksum so nothing is optimised
    /// away.
    fn render_frame(&mut self, dirty_only: bool) -> u64;
    fn resize(&mut self, cols: u16, rows: u16);
    /// 0-based (column, row) in the active area.
    fn cursor(&self) -> (u16, u16);
    /// Cell in the active area (0-based).
    fn cell(&self, col: u16, row: u16) -> CellInfo;
    /// Lines currently held in scrollback history.
    fn history_lines(&self) -> usize;
    /// Whether the alternate screen is active.
    fn alt_screen(&self) -> bool;
    /// Compress scrollback storage if the core supports it. Returns false
    /// when unsupported.
    fn compress_scrollback(&mut self) -> bool {
        false
    }

    /// Visible text of an active-area row, skipping spacer cells.
    fn row_text(&self, row: u16, cols: u16) -> String {
        let mut s = String::new();
        for c in 0..cols {
            let cell = self.cell(c, row);
            match cell.width {
                Width::SpacerTail | Width::SpacerHead => {}
                _ if cell.text.is_empty() => s.push(' '),
                _ => s.push_str(&cell.text),
            }
        }
        s.trim_end().to_owned()
    }
}

#[inline]
fn mix(h: u64, v: u64) -> u64 {
    (h ^ v).wrapping_mul(0x0100_0000_01B3)
}

#[inline]
fn rgb(r: u8, g: u8, b: u8) -> u64 {
    (u64::from(r) << 16) | (u64::from(g) << 8) | u64::from(b)
}

// ---------------------------------------------------------------------------
// libghostty-vt
// ---------------------------------------------------------------------------

pub mod ghostty {
    use super::*;
    use libghostty_vt::{
        RenderState, Terminal,
        render::{CellIterator, Dirty, RowIterator},
        screen::{CellWide, Screen},
        terminal::{CompressionMode, CompressionResult, Point, PointCoordinate},
    };

    pub struct Ghostty {
        term: Terminal<'static, 'static>,
        render: RenderState<'static>,
        rows: RowIterator<'static>,
        cells: CellIterator<'static>,
        graphemes: Vec<char>,
    }

    impl Core for Ghostty {
        const NAME: &'static str = "libghostty-vt";

        fn new(cols: u16, rows: u16, scrollback_lines: usize) -> Self {
            let mut term = Terminal::new(cols, rows).expect("terminal");
            // Use the line limit only, so both cores retain the same history.
            term.set_scrollback_max_bytes(None).expect("bytes limit");
            term.set_scrollback_max_lines(Some(scrollback_lines))
                .expect("lines limit");
            Self {
                term,
                render: RenderState::new().expect("render state"),
                rows: RowIterator::new().expect("row iter"),
                cells: CellIterator::new().expect("cell iter"),
                graphemes: vec!['\0'; 16],
            }
        }

        fn feed(&mut self, bytes: &[u8]) {
            self.term.vt_write(bytes);
        }

        fn render_frame(&mut self, dirty_only: bool) -> u64 {
            let mut h = 0xcbf2_9ce4_8422_2325u64;
            let snap = self.render.update(&self.term).expect("update");
            if dirty_only && snap.dirty().expect("dirty") == Dirty::Clean {
                return black_box(h);
            }
            let colors = snap.colors().expect("colors");
            let mut row_iter = self.rows.update(&snap).expect("rows");
            while let Some(row) = row_iter.next() {
                if dirty_only && !row.dirty().expect("row dirty") {
                    continue;
                }
                let mut cell_iter = self.cells.update(row).expect("cells");
                while let Some(cell) = cell_iter.next() {
                    let n = cell.graphemes_len().expect("glen");
                    if n > 0 {
                        if n > self.graphemes.len() {
                            self.graphemes.resize(n, '\0');
                        }
                        cell.graphemes_buf(&mut self.graphemes[..n]).expect("gbuf");
                        for &c in &self.graphemes[..n] {
                            h = mix(h, c as u64);
                        }
                    }
                    let fg = cell.fg_color().expect("fg").unwrap_or(colors.foreground);
                    let bg = cell.bg_color().expect("bg").unwrap_or(colors.background);
                    let st = cell.style().expect("style");
                    let attrs = u64::from(st.bold)
                        | u64::from(st.italic) << 1
                        | u64::from(st.faint) << 2
                        | u64::from(st.inverse) << 3
                        | u64::from(st.strikethrough) << 4
                        | (st.underline as u64) << 5;
                    h = mix(
                        h,
                        rgb(fg.r, fg.g, fg.b) ^ rgb(bg.r, bg.g, bg.b) << 24 ^ attrs << 48,
                    );
                }
                // A real renderer clears per-row dirty after drawing the row.
                row.set_dirty(false).expect("row dirty");
            }
            snap.set_dirty(Dirty::Clean).expect("dirty");
            black_box(h)
        }

        fn resize(&mut self, cols: u16, rows: u16) {
            self.term.resize(cols, rows, 8, 16).expect("resize");
        }

        fn cursor(&self) -> (u16, u16) {
            (self.term.cursor_x().unwrap(), self.term.cursor_y().unwrap())
        }

        fn cell(&self, col: u16, row: u16) -> CellInfo {
            let r = self
                .term
                .grid_ref(Point::Active(PointCoordinate {
                    x: col,
                    y: u32::from(row),
                }))
                .expect("grid ref");
            let mut buf = ['\0'; 32];
            let n = r.graphemes(&mut buf).expect("graphemes");
            let width = match r.cell().and_then(|c| c.wide()).expect("wide") {
                CellWide::Narrow => Width::Narrow,
                CellWide::Wide => Width::Wide,
                CellWide::SpacerTail => Width::SpacerTail,
                CellWide::SpacerHead => Width::SpacerHead,
            };
            // Normalise a written blank to "" so both cores compare equal.
            let text: String = buf[..n].iter().collect();
            let text = if text == " " { String::new() } else { text };
            CellInfo { text, width }
        }

        fn history_lines(&self) -> usize {
            self.term.scrollback_rows().unwrap()
        }

        fn alt_screen(&self) -> bool {
            self.term.active_screen().unwrap() == Screen::Alternate
        }

        fn compress_scrollback(&mut self) -> bool {
            matches!(
                self.term.compress(CompressionMode::Full).expect("compress"),
                CompressionResult::Complete | CompressionResult::Pending
            )
        }
    }
}

// ---------------------------------------------------------------------------
// alacritty_terminal
// ---------------------------------------------------------------------------

pub mod alacritty {
    use super::*;
    use alacritty_terminal::{
        Term,
        event::VoidListener,
        grid::Dimensions,
        index::{Column, Line},
        term::{
            Config, TermDamage, TermMode,
            cell::{Cell, Flags},
            color::Colors,
            test::TermSize,
        },
        vte::ansi::{Color, Processor, Rgb},
    };

    pub struct Alacritty {
        term: Term<VoidListener>,
        parser: Processor,
        palette: [Rgb; 256],
    }

    /// xterm default 256-colour palette. alacritty_terminal leaves palette
    /// entries unset (`None`) unless the app sets them via OSC 4; the
    /// alacritty *app* fills them from its config, so the embedder must too.
    fn xterm_palette() -> [Rgb; 256] {
        const BASE: [(u8, u8, u8); 16] = [
            (0, 0, 0),
            (205, 0, 0),
            (0, 205, 0),
            (205, 205, 0),
            (0, 0, 238),
            (205, 0, 205),
            (0, 205, 205),
            (229, 229, 229),
            (127, 127, 127),
            (255, 0, 0),
            (0, 255, 0),
            (255, 255, 0),
            (92, 92, 255),
            (255, 0, 255),
            (0, 255, 255),
            (255, 255, 255),
        ];
        let mut p = [Rgb { r: 0, g: 0, b: 0 }; 256];
        for (i, &(r, g, b)) in BASE.iter().enumerate() {
            p[i] = Rgb { r, g, b };
        }
        let step = |v: usize| if v == 0 { 0 } else { (55 + v * 40) as u8 };
        for i in 0..216 {
            p[16 + i] = Rgb {
                r: step(i / 36),
                g: step((i / 6) % 6),
                b: step(i % 6),
            };
        }
        for i in 0..24 {
            let v = (8 + i * 10) as u8;
            p[232 + i] = Rgb { r: v, g: v, b: v };
        }
        p
    }

    impl Alacritty {
        #[inline]
        fn resolve(&self, c: Color, colors: &Colors, default: Rgb) -> Rgb {
            match c {
                Color::Spec(rgb) => rgb,
                Color::Indexed(i) => colors[i as usize].unwrap_or(self.palette[i as usize]),
                Color::Named(n) => {
                    let idx = n as usize;
                    colors[n].unwrap_or(if idx < 256 {
                        self.palette[idx]
                    } else {
                        default
                    })
                }
            }
        }
    }

    impl Alacritty {
        /// Terminal mode bits (kitty keyboard flags, mouse modes, ...).
        pub fn mode(&self) -> TermMode {
            *self.term.mode()
        }
    }

    impl Core for Alacritty {
        const NAME: &'static str = "alacritty_terminal";

        fn new(cols: u16, rows: u16, scrollback_lines: usize) -> Self {
            let config = Config {
                scrolling_history: scrollback_lines,
                kitty_keyboard: true,
                ..Config::default()
            };
            let size = TermSize::new(cols as usize, rows as usize);
            Self {
                term: Term::new(config, &size, VoidListener),
                parser: Processor::new(),
                palette: xterm_palette(),
            }
        }

        fn feed(&mut self, bytes: &[u8]) {
            self.parser.advance(&mut self.term, bytes);
        }

        fn render_frame(&mut self, dirty_only: bool) -> u64 {
            let mut h = 0xcbf2_9ce4_8422_2325u64;
            // A real renderer asks for damage first, then draws, then resets.
            let damaged: Option<Vec<usize>> = match self.term.damage() {
                TermDamage::Full => None,
                TermDamage::Partial(it) => Some(it.map(|d| d.line).collect()),
            };
            let default_fg = Rgb {
                r: 229,
                g: 229,
                b: 229,
            };
            let default_bg = Rgb { r: 0, g: 0, b: 0 };
            let visit = |h: &mut u64, cell: &Cell, colors: &Colors| {
                *h = mix(*h, cell.c as u64);
                if let Some(zw) = cell.zerowidth() {
                    for &c in zw {
                        *h = mix(*h, c as u64);
                    }
                }
                let fg = self.resolve(cell.fg, colors, default_fg);
                let bg = self.resolve(cell.bg, colors, default_bg);
                let attrs = u64::from(cell.flags.bits());
                *h = mix(
                    *h,
                    rgb(fg.r, fg.g, fg.b) ^ rgb(bg.r, bg.g, bg.b) << 24 ^ attrs << 48,
                );
            };
            match damaged {
                Some(lines) if dirty_only => {
                    let grid = self.term.grid();
                    let offset = grid.display_offset() as i32;
                    for line in lines {
                        let row = &grid[Line(line as i32 - offset)];
                        for col in 0..grid.columns() {
                            visit(&mut h, &row[Column(col)], self.term.colors());
                        }
                    }
                }
                _ => {
                    let content = self.term.renderable_content();
                    for indexed in content.display_iter {
                        visit(&mut h, indexed.cell, content.colors);
                    }
                }
            }
            self.term.reset_damage();
            black_box(h)
        }

        fn resize(&mut self, cols: u16, rows: u16) {
            self.term
                .resize(TermSize::new(cols as usize, rows as usize));
        }

        fn cursor(&self) -> (u16, u16) {
            let p = self.term.grid().cursor.point;
            (p.column.0 as u16, p.line.0 as u16)
        }

        fn cell(&self, col: u16, row: u16) -> CellInfo {
            let cell = &self.term.grid()[Line(i32::from(row))][Column(col as usize)];
            let width = if cell.flags.contains(Flags::WIDE_CHAR) {
                Width::Wide
            } else if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                Width::SpacerTail
            } else if cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
                Width::SpacerHead
            } else {
                Width::Narrow
            };
            let mut text = String::new();
            if !(cell.c == ' ' && cell.zerowidth().is_none()) && width != Width::SpacerTail {
                text.push(cell.c);
                if let Some(zw) = cell.zerowidth() {
                    text.extend(zw);
                }
            }
            CellInfo { text, width }
        }

        fn history_lines(&self) -> usize {
            self.term.grid().history_size()
        }

        fn alt_screen(&self) -> bool {
            self.term.mode().contains(TermMode::ALT_SCREEN)
        }
    }
}
